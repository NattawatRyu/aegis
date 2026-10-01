//! Authoritative simulation — the single source of truth for the world.
//!
//! STRUCTURAL DEFENSE (not a reject-guard, because the protocol gives a client
//! no way to *assert* these): the server, not the client, decides
//!   - where a player is (`apply_move` integrates an intent direction),
//!   - whether a shot connects (`apply_shot` does the hitscan here).
//!
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

/// An aim a direction can be taken from: finite and non-zero length. A zero,
/// NaN or infinite aim fires nothing and says nothing (the sanity guard should
/// already have rejected NaN/inf; this is the sim not relying on that).
fn is_usable_aim(aim: Vec2) -> bool {
    let l = aim.len();
    l.is_finite() && l > 0.0
}

/// Ticks a dead player waits before `step_respawns` revives it (1s at 30Hz).
pub const RESPAWN_TICKS: u32 = 30;

/// Server-only per-player bookkeeping, index-aligned with `players`. Kept out
/// of `PlayerState` because that type is the wire snapshot.
#[derive(Clone, Copy)]
struct Life {
    spawn: Vec2,
    dead_ticks: u32,
}

#[derive(Default)]
pub struct Sim {
    players: Vec<PlayerState>,
    lives: Vec<Life>,
}

impl Sim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a player at `pos`, which is also where it respawns.
    pub fn spawn(&mut self, id: PlayerId, pos: Vec2) {
        self.players.push(PlayerState { id, pos, health: MAX_HEALTH, alive: true });
        self.lives.push(Life { spawn: pos, dead_ticks: 0 });
    }

    /// Remove a player from the world entirely (its session ended). A later
    /// `spawn` with the same id starts from scratch.
    pub fn despawn(&mut self, id: PlayerId) {
        if let Some(i) = self.players.iter().position(|p| p.id == id) {
            self.players.remove(i);
            self.lives.remove(i);
        }
    }

    /// Advance every dead player's respawn timer by one tick; a player dead
    /// for `RESPAWN_TICKS` calls comes back at its spawn point, full health.
    /// Call once at the start of each tick. Without it a match is over as soon
    /// as the first volley lands, and the detector has nothing to measure.
    pub fn step_respawns(&mut self) {
        for (p, l) in self.players.iter_mut().zip(self.lives.iter_mut()) {
            if p.alive {
                continue;
            }
            l.dead_ticks += 1;
            if l.dead_ticks >= RESPAWN_TICKS {
                *p = PlayerState { id: p.id, pos: l.spawn, health: MAX_HEALTH, alive: true };
                l.dead_ticks = 0;
            }
        }
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

    /// Angle in radians between `aim` and the bearing from `shooter` to its
    /// nearest alive enemy — how far off a perfect snap the aim was. Computed
    /// from server positions, so it is evidence the client cannot shape except
    /// by how it aims. `None` if the shooter is dead, has no enemy, the aim is
    /// degenerate, or the enemy is point-blank (within `HIT_RADIUS`): there
    /// the bearing is undefined or every direction is on target, so the shot
    /// says nothing about aim — and two players stacked on one spot would
    /// otherwise read as `atan2(0, 0) = 0`, a perfect snap. Call before
    /// `apply_shot`, which may kill the target.
    ///
    /// `atan2(cross, dot)`, not `acos(dot)`: near 0 an f32 `acos` cannot
    /// resolve below ~5e-4 rad, which is the scale an exact-aim check lives at.
    pub fn aim_error(&self, shooter: PlayerId, aim: Vec2) -> Option<f32> {
        let s = self.player(shooter).filter(|p| p.alive)?;
        if !is_usable_aim(aim) {
            return None;
        }
        let d2 = |p: &PlayerState| {
            let (dx, dy) = (p.pos.x - s.pos.x, p.pos.y - s.pos.y);
            dx * dx + dy * dy
        };
        let e = self
            .players
            .iter()
            .filter(|p| p.id != shooter && p.alive)
            .min_by(|a, b| d2(a).total_cmp(&d2(b)))?;
        let (bx, by) = (e.pos.x - s.pos.x, e.pos.y - s.pos.y);
        if bx * bx + by * by <= HIT_RADIUS * HIT_RADIUS {
            return None;
        }
        let cross = aim.x * by - aim.y * bx;
        let dot = aim.x * bx + aim.y * by;
        Some(cross.atan2(dot).abs())
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
        if !is_usable_aim(aim) {
            return None;
        }
        let alen = aim.len();
        let (dx, dy) = (aim.x / alen, aim.y / alen);

        // Find the nearest alive enemy within range that the ray passes through.
        let mut best: Option<(f32, PlayerId)> = None;
        for p in self.players.iter() {
            if p.id == shooter || !p.alive {
                continue;
            }
            let (tx, ty) = (p.pos.x - sx, p.pos.y - sy);
            let t = tx * dx + ty * dy; // projection onto the ray
            if !(0.0..=SHOT_RANGE).contains(&t) {
                continue; // behind the shooter or out of range
            }
            // perpendicular distance from the target center to the ray
            let perp2 = (tx * tx + ty * ty) - t * t;
            if perp2 <= HIT_RADIUS * HIT_RADIUS && best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, p.id));
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

    fn kill(sim: &mut Sim, shooter: PlayerId) {
        for _ in 0..(MAX_HEALTH / SHOT_DAMAGE) {
            sim.apply_shot(shooter, Vec2::new(1.0, 0.0));
        }
    }

    /// Edge: dead through call RESPAWN_TICKS-1, back on call RESPAWN_TICKS.
    #[test]
    fn respawn_after_exactly_respawn_ticks_at_spawn_with_full_health() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(5.0, 0.0));
        sim.apply_move(2, Vec2::new(1.0, 0.0)); // now at (10,0), away from spawn
        kill(&mut sim, 1);
        assert!(!sim.player(2).unwrap().alive);
        for _ in 0..RESPAWN_TICKS - 1 {
            sim.step_respawns();
        }
        assert!(!sim.player(2).unwrap().alive, "revived one tick early");
        sim.step_respawns();
        let p = sim.player(2).unwrap();
        assert!(p.alive);
        assert_eq!(p.health, MAX_HEALTH);
        assert_eq!(p.pos, Vec2::new(5.0, 0.0)); // spawn point, not death point
    }

    #[test]
    fn respawn_timer_restarts_on_second_death() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(5.0, 0.0));
        kill(&mut sim, 1);
        for _ in 0..RESPAWN_TICKS {
            sim.step_respawns();
        }
        kill(&mut sim, 1);
        for _ in 0..RESPAWN_TICKS - 1 {
            sim.step_respawns();
        }
        assert!(!sim.player(2).unwrap().alive, "timer carried over from the first death");
    }

    #[test]
    fn respawn_leaves_the_living_alone() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.apply_move(1, Vec2::new(1.0, 0.0));
        for _ in 0..RESPAWN_TICKS * 2 {
            sim.step_respawns();
        }
        assert_eq!(sim.player(1).unwrap().pos, Vec2::new(MOVE_SPEED, 0.0));
    }

    #[test]
    fn aim_error_is_zero_on_the_bearing_and_the_angle_off_it() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        sim.spawn(3, Vec2::new(0.0, 40.0)); // farther: not the reference enemy
        assert_eq!(sim.aim_error(1, Vec2::new(1.0, 0.0)), Some(0.0));
        let (s, c) = 0.1f32.sin_cos();
        let off = sim.aim_error(1, Vec2::new(c, -s)).unwrap();
        assert!((off - 0.1).abs() < 1e-6, "{off}");
        // magnitude does not matter, only direction
        assert_eq!(sim.aim_error(1, Vec2::new(7.0, 0.0)), Some(0.0));
        // straight away from the enemy is the maximum, pi
        assert!((sim.aim_error(1, Vec2::new(-1.0, 0.0)).unwrap() - std::f32::consts::PI).abs() < 1e-6);
    }

    #[test]
    fn aim_error_resolves_below_acos_precision() {
        // 1e-4 rad off: f32 acos cannot see this, atan2 can.
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        let (s, c) = 1e-4f32.sin_cos();
        let off = sim.aim_error(1, Vec2::new(c, s)).unwrap();
        assert!((off - 1e-4).abs() < 1e-6, "{off}");
    }

    #[test]
    fn aim_error_none_without_a_target_or_an_aim() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        assert_eq!(sim.aim_error(1, Vec2::new(1.0, 0.0)), None); // alone
        sim.spawn(2, Vec2::new(10.0, 0.0));
        assert_eq!(sim.aim_error(1, Vec2::ZERO), None); // degenerate aim
        assert_eq!(sim.aim_error(9, Vec2::new(1.0, 0.0)), None); // unknown shooter
    }

    /// Edge: zero, NaN and infinite aims neither hit nor count as evidence —
    /// the sim must not depend on the sanity guard having run.
    #[test]
    fn unusable_aims_fire_nothing() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        for aim in [Vec2::ZERO, Vec2::new(f32::NAN, 0.0), Vec2::new(f32::INFINITY, 0.0), Vec2::new(f32::MAX, f32::MAX)] {
            assert_eq!(sim.apply_shot(1, aim), None, "{aim:?} hit");
            assert_eq!(sim.aim_error(1, aim), None, "{aim:?} scored");
        }
        assert_eq!(sim.player(2).unwrap().health, MAX_HEALTH);
        // smallest sane aim still works
        assert!(sim.apply_shot(1, Vec2::new(1e-3, 0.0)).is_some());
    }

    /// Edge: shot range is inclusive at SHOT_RANGE, exclusive past it.
    #[test]
    fn shot_range_edge() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(SHOT_RANGE, 0.0));
        assert!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).is_some());
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(SHOT_RANGE + 0.01, 0.0));
        assert!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).is_none());
    }

    /// Edge: stacked or inside the hitbox is no evidence; just outside is.
    #[test]
    fn aim_error_none_point_blank() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::ZERO); // stacked: atan2(0,0) would say "exact"
        assert_eq!(sim.aim_error(1, Vec2::new(0.0, 1.0)), None);

        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(HIT_RADIUS, 0.0));
        assert_eq!(sim.aim_error(1, Vec2::new(0.0, 1.0)), None);

        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(HIT_RADIUS * 1.01, 0.0));
        assert!(sim.aim_error(1, Vec2::new(1.0, 0.0)).is_some());
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
        assert!(last.unwrap().killed);
        assert!(!sim.player(2).unwrap().alive);
        // a dead target can no longer be hit
        assert!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).is_none());
    }
}
