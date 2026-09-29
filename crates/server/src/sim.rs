//! Authoritative simulation — the single source of truth for the world.
//!
//! STRUCTURAL DEFENSE (not a reject-guard, because the protocol gives a client
//! no way to *assert* these): the server, not the client, decides
//!   - where a player is (`apply_move` integrates an intent direction),
//!   - whether a shot connects (`apply_shot` does the hitscan here).
//! A client can only send `move_dir` / `aim` / `shoot`. It can never send a
//! position or a hit, so teleport / instant-hit / fake-damage have nothing to
//! ride in on. See `guards/` for the input-trust defenses layered on top.

use aegis_protocol::{PlayerId, PlayerState, Vec2};

/// Distance a player travels per tick along a unit direction.
pub const MOVE_SPEED: f32 = 5.0;
/// Half-width of the square arena; positions are clamped to it.
pub const ARENA_HALF: f32 = 50.0;
/// Max distance a shot travels.
pub const SHOT_RANGE: f32 = 100.0;
/// Radius of a player's hitbox for hitscan.
pub const HIT_RADIUS: f32 = 1.0;
/// Damage one connecting shot deals.
pub const SHOT_DAMAGE: u8 = 25;
/// Player starting health.
pub const MAX_HEALTH: u8 = 100;

/// Outcome of an authoritative shot resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShotResult {
    pub target: PlayerId,
    pub damage: u8,
    pub killed: bool,
}

#[derive(Default)]
pub struct Sim {
    players: Vec<PlayerState>,
}

impl Sim {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(&mut self, id: PlayerId, pos: Vec2) {
        self.players.push(PlayerState { id, pos, health: MAX_HEALTH, alive: true });
    }

    pub fn player(&self, id: PlayerId) -> Option<&PlayerState> {
        self.players.iter().find(|p| p.id == id)
    }

    fn player_mut(&mut self, id: PlayerId) -> Option<&mut PlayerState> {
        self.players.iter_mut().find(|p| p.id == id)
    }

    /// Move a player by an already-unit-clamped direction. The caller (the
    /// move_speed guard) guarantees `dir_unit.len() <= 1`, so the per-tick
    /// travel can never exceed `MOVE_SPEED`. Position is clamped to the arena.
    pub fn apply_move(&mut self, id: PlayerId, dir_unit: Vec2) {
        if let Some(p) = self.player_mut(id) {
            if !p.alive {
                return;
            }
            let nx = (p.pos.x + dir_unit.x * MOVE_SPEED).clamp(-ARENA_HALF, ARENA_HALF);
            let ny = (p.pos.y + dir_unit.y * MOVE_SPEED).clamp(-ARENA_HALF, ARENA_HALF);
            p.pos = Vec2::new(nx, ny);
        }
    }

    /// Resolve a shot authoritatively. `aim` is a direction (any magnitude);
    /// the hit is computed from server-side positions, never trusted from the
    /// client. Returns the nearest enemy struck, if any.
    pub fn apply_shot(&mut self, shooter: PlayerId, aim: Vec2) -> Option<ShotResult> {
        let (sx, sy) = {
            let s = self.player(shooter)?;
            if !s.alive {
                return None;
            }
            (s.pos.x, s.pos.y)
        };

        // Normalize aim into a unit ray. A zero/degenerate aim fires nothing.
        let alen = aim.len();
        if !(alen > 0.0) {
            return None;
        }
        let (dx, dy) = (aim.x / alen, aim.y / alen);

        // Find the nearest alive enemy within range that the ray passes through.
        let mut best: Option<(f32, PlayerId)> = None;
        for p in self.players.iter() {
            if p.id == shooter || !p.alive {
                continue;
            }
            let (tx, ty) = (p.pos.x - sx, p.pos.y - sy);
            let t = tx * dx + ty * dy; // projection onto the ray
            if t < 0.0 || t > SHOT_RANGE {
                continue; // behind the shooter or out of range
            }
            // perpendicular distance from the target center to the ray
            let perp2 = (tx * tx + ty * ty) - t * t;
            if perp2 <= HIT_RADIUS * HIT_RADIUS {
                if best.map_or(true, |(bt, _)| t < bt) {
                    best = Some((t, p.id));
                }
            }
        }

        let (_, target) = best?;
        let killed;
        {
            let p = self.player_mut(target).expect("target exists");
            p.health = p.health.saturating_sub(SHOT_DAMAGE);
            killed = p.health == 0;
            if killed {
                p.alive = false;
            }
        }
        Some(ShotResult { target, damage: SHOT_DAMAGE, killed })
    }

    pub fn snapshot(&self) -> Vec<PlayerState> {
        self.players.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_travels_exactly_move_speed_along_unit_dir() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.apply_move(1, Vec2::new(1.0, 0.0));
        assert_eq!(sim.player(1).unwrap().pos, Vec2::new(MOVE_SPEED, 0.0));
    }

    #[test]
    fn move_is_clamped_to_arena() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::new(ARENA_HALF, 0.0));
        sim.apply_move(1, Vec2::new(1.0, 0.0)); // would exceed the wall
        assert_eq!(sim.player(1).unwrap().pos.x, ARENA_HALF);
    }

    #[test]
    fn shot_hits_enemy_on_the_ray() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        let r = sim.apply_shot(1, Vec2::new(1.0, 0.0)).expect("hit");
        assert_eq!(r.target, 2);
        assert_eq!(sim.player(2).unwrap().health, MAX_HEALTH - SHOT_DAMAGE);
    }

    #[test]
    fn shot_misses_enemy_off_the_ray() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 5.0)); // perpendicular distance 5 > hitbox
        assert!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).is_none());
    }

    #[test]
    fn shot_misses_enemy_behind_shooter() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(-10.0, 0.0));
        assert!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).is_none());
    }

    #[test]
    fn enough_shots_kill() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(5.0, 0.0));
        let mut last = None;
        for _ in 0..(MAX_HEALTH / SHOT_DAMAGE) {
            last = sim.apply_shot(1, Vec2::new(1.0, 0.0));
        }
        assert_eq!(last.unwrap().killed, true);
        assert!(!sim.player(2).unwrap().alive);
        // a dead target can no longer be hit
        assert!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).is_none());
    }
}
