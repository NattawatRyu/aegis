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

use crate::Visibility;

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

/// An axis-aligned box nothing can see, shoot or walk through. Its edges
/// count as solid: a line that grazes a corner is blocked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    pub min: Vec2,
    pub max: Vec2,
}

impl Wall {
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    /// Does the segment `a -> b` touch this box? Slab test: clip the segment's
    /// parameter range to each axis's slab; it hits iff something is left.
    pub fn blocks(&self, a: Vec2, b: Vec2) -> bool {
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for (a, d, lo, hi) in [(a.x, b.x - a.x, self.min.x, self.max.x), (a.y, b.y - a.y, self.min.y, self.max.y)] {
            if d == 0.0 {
                if a < lo || a > hi {
                    return false;
                }
                continue;
            }
            let (u, v) = ((lo - a) / d, (hi - a) / d);
            t0 = t0.max(u.min(v));
            t1 = t1.min(u.max(v));
            if t0 > t1 {
                return false;
            }
        }
        true
    }
}

/// The lab arena: a pillar in the middle, so players across the spawn ring
/// cannot see each other, and one box in each corner. None touches the spawn
/// ring (radius 20). Any wall that hides a player straight across the ring
/// sits on a diameter, so some walk runs into the pillar — the honest bot
/// turns when a step goes nowhere.
pub const ARENA_WALLS: [Wall; 5] = [
    Wall::new(Vec2 { x: -5.0, y: -5.0 }, Vec2 { x: 5.0, y: 5.0 }),
    Wall::new(Vec2 { x: 26.0, y: 26.0 }, Vec2 { x: 34.0, y: 34.0 }),
    Wall::new(Vec2 { x: -34.0, y: 26.0 }, Vec2 { x: -26.0, y: 34.0 }),
    Wall::new(Vec2 { x: -34.0, y: -34.0 }, Vec2 { x: -26.0, y: -26.0 }),
    Wall::new(Vec2 { x: 26.0, y: -34.0 }, Vec2 { x: 34.0, y: -26.0 }),
];

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
pub(crate) fn is_usable_aim(aim: Vec2) -> bool {
    let l = aim.len();
    l.is_finite() && l > 0.0
}

/// Squared distance — the order "nearest" means everywhere evidence is
/// chosen, so every chooser agrees to the bit.
pub(crate) fn dist2(a: Vec2, b: Vec2) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    dx * dx + dy * dy
}

/// The angular radius in radians of a hitbox of `radius` at `to`, seen from
/// `from`: `asin(radius / distance)`, so an aim within it of the bearing
/// passes through the hitbox. π/2 when `from` is inside it.
pub fn angular_radius(radius: f32, from: Vec2, to: Vec2) -> f32 {
    (radius / dist2(from, to).sqrt()).min(1.0).asin()
}

/// Angle in radians between `aim` and the bearing from `from` to `to`;
/// `None` for an aim with no direction. `atan2(cross, dot)`, not
/// `acos(dot)`: see [`Sim::aim_error`].
pub(crate) fn bearing_error(aim: Vec2, from: Vec2, to: Vec2) -> Option<f32> {
    if !is_usable_aim(aim) {
        return None;
    }
    let (bx, by) = (to.x - from.x, to.y - from.y);
    let cross = aim.x * by - aim.y * bx;
    let dot = aim.x * bx + aim.y * by;
    Some(cross.atan2(dot).abs())
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
    walls: Vec<Wall>,
}

impl Sim {
    /// An open arena, no walls.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_walls(walls: &[Wall]) -> Self {
        Self { walls: walls.to_vec(), ..Self::default() }
    }

    /// Is the straight line `a -> b` clear of every wall?
    pub fn clear(&self, a: Vec2, b: Vec2) -> bool {
        !self.walls.iter().any(|w| w.blocks(a, b))
    }

    /// Can a player standing at `from` see any of a hitbox centred at `to`?
    /// Three rays: to the centre and to the two edges across the line of
    /// sight. An approximation — a sliver of hitbox between the rays can be
    /// shot yet read as hidden — chosen so the common case (half a player
    /// sticking out past a corner) is seen.
    pub fn sees(&self, from: Vec2, to: Vec2) -> bool {
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let l = (dx * dx + dy * dy).sqrt();
        if l == 0.0 {
            return self.clear(from, to);
        }
        let (px, py) = (-dy / l * HIT_RADIUS, dx / l * HIT_RADIUS);
        [to, Vec2::new(to.x + px, to.y + py), Vec2::new(to.x - px, to.y - py)].into_iter().any(|t| self.clear(from, t))
    }

    /// Could a player at `from` see a player at `to` at some moment in the
    /// next `ticks` ticks, both moving? A sampled sweep: each side's own spot
    /// plus, for every j in 1..=ticks, the 8 points j full steps away that it
    /// can walk to in a straight line — and any viewer sample that
    /// [`Sim::sees`] any target sample counts. `ticks == 0` is exactly
    /// [`Sim::sees`].
    ///
    /// Samples, not the exact reachable sets (players turn freely): it can
    /// miss a pair that becomes visible between samples, and say "visible"
    /// for one that never will be. Both errors are measured, not assumed —
    /// the lab counts the first as `late` and the second as `hidden`.
    pub fn sees_within(&self, from: Vec2, to: Vec2, ticks: u32) -> bool {
        self.sees_within_step(from, to, ticks, MOVE_SPEED)
    }

    /// [`Sim::sees_within`] for players that move `step` a tick instead of
    /// `MOVE_SPEED` — for the lab's sweep of what the margin would leak in a
    /// world at another scale, not for a live server.
    pub fn sees_within_step(&self, from: Vec2, to: Vec2, ticks: u32, step: f32) -> bool {
        if self.sees(from, to) {
            return true;
        }
        if ticks == 0 {
            return false;
        }
        let targets = self.straight_paths(to, ticks, step);
        self.straight_paths(from, ticks, step).into_iter().any(|v| targets.iter().any(|&t| self.sees(v, t)))
    }

    /// `c`, and every spot a player at `c` reaches walking straight in one of
    /// 8 directions for 1..=`ticks` ticks of `step` — stepped as
    /// [`Sim::apply_move`] steps (clamped to the arena, a step into a wall
    /// not taken).
    fn straight_paths(&self, c: Vec2, ticks: u32, step: f32) -> Vec<Vec2> {
        let mut out = vec![c];
        for k in 0..8 {
            let (s, co) = (std::f32::consts::FRAC_PI_4 * k as f32).sin_cos();
            let mut p = c;
            for _ in 0..ticks {
                let next = Vec2::new(
                    (p.x + co * step).clamp(-ARENA_HALF, ARENA_HALF),
                    (p.y + s * step).clamp(-ARENA_HALF, ARENA_HALF),
                );
                if !self.clear(p, next) {
                    break;
                }
                p = next;
                out.push(p);
            }
        }
        out
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

    /// Every player, in world order (the order snapshots and views use).
    pub fn players(&self) -> &[PlayerState] {
        &self.players
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
    /// A step whose path touches a wall is not taken at all — the whole path,
    /// not just where it ends, so a step can never tunnel through a thin wall.
    pub fn apply_move(&mut self, id: PlayerId, dir_unit: Vec2) {
        let Some(p) = self.player(id).filter(|p| p.alive) else { return };
        let from = p.pos;
        let nx = (from.x + dir_unit.x * MOVE_SPEED).clamp(-ARENA_HALF, ARENA_HALF);
        let ny = (from.y + dir_unit.y * MOVE_SPEED).clamp(-ARENA_HALF, ARENA_HALF);
        let to = Vec2::new(nx, ny);
        if self.clear(from, to) {
            self.player_mut(id).expect("checked above").pos = to;
        }
    }

    /// Angle in radians between `aim` and the bearing from `shooter` to its
    /// nearest alive enemy it can see ([`Sim::sees`]) — how far off a perfect
    /// snap the aim was; an enemy behind a wall is not one it could aim at. Computed
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
        self.aim_evidence(shooter, aim, |id| self.player(id).is_some_and(|p| p.alive)).map(|(err, _)| err)
    }

    /// [`Sim::aim_error`], and the enemy it was measured against, choosing
    /// only among the players `shown` admits instead of the living. Within a
    /// tick shots resolve one by one, so by a shooter's turn its target may
    /// already be dead; the evidence is still about the world its snapshot
    /// showed — the server passes who was alive in it.
    pub fn aim_evidence(
        &self,
        shooter: PlayerId,
        aim: Vec2,
        shown: impl Fn(PlayerId) -> bool,
    ) -> Option<(f32, PlayerId)> {
        let s = self.player(shooter)?.pos;
        self.aim_error_by(shooter, aim, |p| shown(p.id) && self.sees(s, p.pos))
    }

    /// [`Sim::aim_evidence`] with line of sight read from `vis` (computed on
    /// these positions) instead of ray-cast again.
    pub fn aim_evidence_in(
        &self,
        vis: &Visibility,
        shooter: PlayerId,
        aim: Vec2,
        shown: impl Fn(PlayerId) -> bool,
    ) -> Option<(f32, PlayerId)> {
        self.aim_error_by(shooter, aim, |p| shown(p.id) && vis.sees(shooter, p.id))
    }

    /// Nearest target admitted by `candidate` (the shooter itself never is).
    /// The shooter must be alive now: a shot from the dead is not fired.
    fn aim_error_by(
        &self,
        shooter: PlayerId,
        aim: Vec2,
        candidate: impl Fn(&PlayerState) -> bool,
    ) -> Option<(f32, PlayerId)> {
        let s = self.player(shooter).filter(|p| p.alive)?;
        if !is_usable_aim(aim) {
            return None;
        }
        let e = self
            .players
            .iter()
            .filter(|p| p.id != shooter && candidate(p))
            .min_by(|a, b| dist2(s.pos, a.pos).total_cmp(&dist2(s.pos, b.pos)))?;
        if dist2(s.pos, e.pos) <= HIT_RADIUS * HIT_RADIUS {
            return None;
        }
        bearing_error(aim, s.pos, e.pos).map(|err| (err, e.id))
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
            // The shot travels to the point on the ray nearest the target; a
            // wall anywhere before that stops it.
            let near = Vec2::new(sx + dx * t, sy + dy * t);
            if perp2 <= HIT_RADIUS * HIT_RADIUS
                && best.is_none_or(|(bt, _)| t < bt)
                && self.clear(Vec2::new(sx, sy), near)
            {
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

    /// The whole world. Server-side only: what goes on the wire to a player is
    /// its [`Sim::view`].
    pub fn snapshot(&self) -> Vec<PlayerState> {
        self.players.clone()
    }

    /// What player `id` is sent: itself, and every other player it
    /// [`Sim::sees`]. A player behind a wall is not in it, so no client —
    /// honest or wallhacked — holds a position it could not have seen; ESP has
    /// nothing to draw. Empty for an id not in the world.
    ///
    /// No margin: an enemy appears the tick it comes into sight. Over a real
    /// network that pop-in is the peeker's advantage; [`Sim::view_within`]
    /// widens the view by what latency can move a player, at the cost of
    /// leaking that much.
    pub fn view(&self, id: PlayerId) -> Vec<PlayerState> {
        self.view_within(id, 0)
    }

    /// [`Sim::view`] with a margin: every player `id` could see within the
    /// next `ticks` ticks ([`Sim::sees_within`]). `ticks == 0` is the exact
    /// view.
    pub fn view_within(&self, id: PlayerId, ticks: u32) -> Vec<PlayerState> {
        self.view_within_step(id, ticks, MOVE_SPEED)
    }

    /// [`Sim::view_within`] at another step size ([`Sim::sees_within_step`]).
    pub fn view_within_step(&self, id: PlayerId, ticks: u32, step: f32) -> Vec<PlayerState> {
        let Some(me) = self.player(id) else { return Vec::new() };
        self.players
            .iter()
            .filter(|p| p.id == id || self.sees_within_step(me.pos, p.pos, ticks, step))
            .copied()
            .collect()
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
        for aim in [Vec2::ZERO, Vec2::new(f32::NAN, 0.0), Vec2::new(f32::INFINITY, 0.0), Vec2::new(f32::MAX, f32::MAX)]
        {
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

    fn pillar() -> Sim {
        // a 2x2 box centred on (5,0)
        Sim::with_walls(&[Wall::new(Vec2::new(4.0, -1.0), Vec2::new(6.0, 1.0))])
    }

    #[test]
    fn wall_blocks_at_its_edges() {
        let w = Wall::new(Vec2::new(4.0, -1.0), Vec2::new(6.0, 1.0));
        assert!(w.blocks(Vec2::ZERO, Vec2::new(10.0, 0.0))); // straight through
        assert!(w.blocks(Vec2::ZERO, Vec2::new(4.0, 0.0))); // ends on the face
        assert!(!w.blocks(Vec2::ZERO, Vec2::new(3.99, 0.0))); // stops short
        assert!(w.blocks(Vec2::new(0.0, 1.0), Vec2::new(10.0, 1.0))); // grazes the top edge
        assert!(!w.blocks(Vec2::new(0.0, 1.01), Vec2::new(10.0, 1.01))); // just above it
        assert!(w.blocks(Vec2::new(5.0, 10.0), Vec2::new(5.0, -10.0))); // vertical, d.x == 0
        assert!(!w.blocks(Vec2::new(7.0, 10.0), Vec2::new(7.0, -10.0))); // vertical, beside it
        assert!(w.blocks(Vec2::new(3.0, 2.0), Vec2::new(5.0, 0.0))); // diagonal into the corner
    }

    #[test]
    fn wall_stops_a_shot_and_its_evidence() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0)); // behind the box
        assert_eq!(sim.apply_shot(1, Vec2::new(1.0, 0.0)), None);
        assert_eq!(sim.aim_error(1, Vec2::new(1.0, 0.0)), None, "hidden enemy scored as a target");
        // same pair, open arena: hit
        let mut open = Sim::new();
        open.spawn(1, Vec2::ZERO);
        open.spawn(2, Vec2::new(10.0, 0.0));
        assert!(open.apply_shot(1, Vec2::new(1.0, 0.0)).is_some());
    }

    #[test]
    fn wall_in_front_of_a_farther_enemy_does_not_stop_a_nearer_one() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(3.0, 0.0)); // before the box
        sim.spawn(3, Vec2::new(10.0, 0.0)); // behind it
        assert_eq!(sim.apply_shot(1, Vec2::new(1.0, 0.0)).map(|r| r.target), Some(2));
    }

    /// Aim evidence is measured against the nearest enemy the shooter can see,
    /// not the nearest one through a wall.
    #[test]
    fn aim_error_skips_a_nearer_hidden_enemy() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0)); // nearer, hidden
        sim.spawn(3, Vec2::new(0.0, 20.0)); // farther, in the open
        assert_eq!(sim.aim_error(1, Vec2::new(0.0, 1.0)), Some(0.0));
    }

    /// Edge: a hitbox whose centre is behind the corner but whose edge sticks
    /// out is seen; one fully behind is not.
    #[test]
    fn sees_a_hitbox_edge_past_a_corner() {
        let sim = pillar();
        // Viewer on the axis, target straight behind the box: hidden.
        assert!(!sim.sees(Vec2::ZERO, Vec2::new(10.0, 0.0)));
        // Target 1.5 above the axis: its centre ray clips the box's top
        // corner region, its upper edge ray clears it.
        assert!(sim.sees(Vec2::ZERO, Vec2::new(10.0, 2.5)));
        // Open arena sees everything.
        assert!(Sim::new().sees(Vec2::ZERO, Vec2::new(10.0, 0.0)));
    }

    #[test]
    fn move_into_a_wall_is_not_taken() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::new(0.0, 0.0));
        sim.apply_move(1, Vec2::new(1.0, 0.0)); // would end at (5,0), inside the box
        assert_eq!(sim.player(1).unwrap().pos, Vec2::ZERO);
        // Path through a thin wall without ending in it is also refused.
        let mut thin = Sim::with_walls(&[Wall::new(Vec2::new(2.0, -1.0), Vec2::new(2.5, 1.0))]);
        thin.spawn(1, Vec2::ZERO);
        thin.apply_move(1, Vec2::new(1.0, 0.0)); // (0,0) -> (5,0) crosses x=2..2.5
        assert_eq!(thin.player(1).unwrap().pos, Vec2::ZERO, "tunnelled through");
        // A step that stays clear is taken.
        thin.apply_move(1, Vec2::new(0.0, 1.0));
        assert_eq!(thin.player(1).unwrap().pos, Vec2::new(0.0, MOVE_SPEED));
    }

    /// No arena wall touches a spawn on the ring or blocks a neighbour on it,
    /// and the pillar blocks the player straight across.
    #[test]
    fn arena_walls_leave_spawns_clear_and_hide_across_the_ring() {
        let sim = Sim::with_walls(&ARENA_WALLS);
        let ring = |a: f32| Vec2::new(20.0 * a.cos(), 20.0 * a.sin());
        for k in 0..12 {
            let a = std::f32::consts::TAU * k as f32 / 12.0;
            assert!(sim.clear(ring(a), ring(a)), "spawn {k} inside a wall");
            assert!(sim.sees(ring(a), ring(a + std::f32::consts::TAU / 12.0)), "neighbour of {k} hidden");
            assert!(!sim.sees(ring(a), ring(a + std::f32::consts::PI)), "across from {k} visible");
        }
    }

    /// The before/after of culling. The whole world (what every client got
    /// before D4) hands player 1 a position it cannot see; its view does not.
    #[test]
    fn view_drops_exactly_the_hidden() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0)); // behind the box
        sim.spawn(3, Vec2::new(0.0, 20.0)); // in the open
        let hidden = |seen: &[PlayerState]| seen.iter().filter(|p| !sim.sees(Vec2::ZERO, p.pos)).count();
        assert_eq!(hidden(&sim.snapshot()), 1, "the old full snapshot leaks player 2");
        let v = sim.view(1);
        assert_eq!(hidden(&v), 0);
        assert_eq!(v.iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 3]);
        // player 2 sees past nobody either: 1 is behind the box from its side
        assert_eq!(sim.view(2).iter().map(|p| p.id).collect::<Vec<_>>(), vec![2, 3]);
        assert!(sim.view(9).is_empty());
        // open arena: the view is the world
        let mut open = Sim::new();
        open.spawn(1, Vec2::ZERO);
        open.spawn(2, Vec2::new(10.0, 0.0));
        assert_eq!(open.view(1), open.snapshot());
    }

    /// The margin against the real mechanics. Over a grid of hidden pairs in
    /// the lab arena, each side walks straight in each of 8 directions (or
    /// stands) through `apply_move` for up to `k` ticks; whenever that makes
    /// the pair visible, `sees_within(.., k)` must already have said so at
    /// the start. That is the pop-in a margin exists to prevent.
    #[test]
    fn margin_covers_every_straight_walk_into_sight() {
        let dirs: Vec<Vec2> = std::iter::once(Vec2::ZERO)
            .chain((0..8).map(|k| {
                let (s, c) = (std::f32::consts::FRAC_PI_4 * k as f32).sin_cos();
                Vec2::new(c, s)
            }))
            .collect();
        let world = Sim::with_walls(&ARENA_WALLS);
        let grid: Vec<Vec2> = (-6..=6)
            .flat_map(|i| (-6..=6).map(move |j| Vec2::new(i as f32 * 7.5, j as f32 * 7.5)))
            .filter(|&p| world.clear(p, p))
            .collect();
        let (mut checked, mut revealed) = (0, 0);
        for k in 1..=2u32 {
            for &a in &grid {
                for &b in &grid {
                    if world.sees(a, b) {
                        continue;
                    }
                    let predicted = world.sees_within(a, b, k);
                    for &da in &dirs {
                        for &db in &dirs {
                            let mut sim = Sim::with_walls(&ARENA_WALLS);
                            sim.spawn(1, a);
                            sim.spawn(2, b);
                            for step in 1..=k {
                                sim.apply_move(1, da);
                                sim.apply_move(2, db);
                                let (pa, pb) = (sim.player(1).unwrap().pos, sim.player(2).unwrap().pos);
                                if sim.sees(pa, pb) {
                                    assert!(predicted, "k={k}: {a:?}->{b:?} visible after {step} steps ({da:?}, {db:?}) but not predicted");
                                    revealed += 1;
                                }
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 10_000, "only {checked} walks checked");
        assert!(revealed > 1_000, "only {revealed} walks came into sight: the test proves little");
    }

    /// Edge: margin 0 is the exact view; a margin is never narrower than it.
    #[test]
    fn margin_zero_is_exact_and_margins_only_widen() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0)); // behind the box
        sim.spawn(3, Vec2::new(0.0, 20.0));
        assert_eq!(sim.view_within(1, 0), sim.view(1));
        assert!(sim.view(1).iter().all(|p| p.id != 2));
        // one step up and player 2 clears the 2x2 box: in the margin-1 view
        assert!(sim.view_within(1, 1).iter().any(|p| p.id == 2));
        for k in 0..4 {
            let (narrow, wide) = (sim.view_within(1, k), sim.view_within(1, k + 1));
            assert!(narrow.iter().all(|p| wide.contains(p)), "k={k} -> {}", k + 1);
        }
    }

    /// The dead are culled like the living: a corpse's position behind a wall
    /// still says where the fight was.
    #[test]
    fn view_culls_the_dead_too() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        sim.spawn(3, Vec2::new(10.0, 3.0)); // shoots 2 from the side
        for _ in 0..(MAX_HEALTH / SHOT_DAMAGE) {
            sim.apply_shot(3, Vec2::new(0.0, -1.0));
        }
        assert!(!sim.player(2).unwrap().alive);
        assert!(sim.view(1).iter().all(|p| p.id != 2));
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
