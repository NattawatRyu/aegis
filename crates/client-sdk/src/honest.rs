//! HonestBot — the baseline. Walks forward at a legal unit speed, aims at the
//! nearest visible enemy *with human error*, increments its seq every tick.
//! Passes every guard clean; the other bots are measured against it.
//!
//! The aim error is what makes it a baseline. Aiming exactly down the bearing
//! is what the aimbot does; an honest bot that did the same would be an aimbot
//! with a different name, and the detector (pillar C) would have nothing to
//! tell apart. The error is deterministic (seeded xorshift), so runs repeat.
//!
//! It also takes time to react. Each enemy that comes into sight gets its own
//! delay of [`REACT_MIN`]..=[`REACT_MAX`] ticks before the bot will fire on
//! it; one that leaves sight or dies is forgotten, and when the bot itself is
//! dead it forgets everyone. It fires only when the *nearest* enemy is ready
//! and holds fire otherwise, even if a farther one is ready: the server
//! measures each shot against the nearest enemy, so a shot at another would be
//! recorded as an instant reaction with a wild aim error. That is a lab
//! convention, not how people play — a real player picks targets freely.

use super::{jitter, my_pos, nearest_enemy, rotate, unit_towards, xorshift, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

/// Largest aim error either side of the true bearing, in radians (~8.6°).
pub const AIM_ERROR_RAD: f32 = 0.15;

/// Fewest ticks between an enemy coming into sight and the first shot at it
/// (200 ms at 30 Hz).
pub const REACT_MIN: u32 = 6;
/// Most ticks between an enemy coming into sight and the first shot at it
/// (400 ms at 30 Hz).
pub const REACT_MAX: u32 = 12;

/// A human reaction: how long after an enemy comes into sight a player is
/// ready to fire on it. Any bot that plays with an honest hand keeps one.
pub struct Reflex {
    /// Draws delays. Its own stream, so a bot's aim error sequence does not
    /// depend on how many enemies it has seen.
    rng: u32,
    /// Per enemy id in sight: the first tick the player will fire on it this
    /// encounter.
    ready: [Option<u32>; 256],
}

impl Reflex {
    /// `seed` 0 is remapped (xorshift would stall at 0).
    pub fn with_seed(seed: u32) -> Self {
        Self { rng: if seed == 0 { 0x5BD1_E995 } else { seed }, ready: [None; 256] }
    }

    /// Bring the table up to date with this snapshot — forget enemies no
    /// longer alive in sight (everyone, if the player itself is dead or
    /// absent), start a new encounter for each one newly in sight — and say
    /// whether the nearest enemy is ready to be fired on. Call every tick.
    pub fn nearest_ready(&mut self, ctx: &BotCtx) -> bool {
        let alive = ctx.snapshot.iter().any(|p| p.id == ctx.my_id && p.alive);
        let mut in_sight = [false; 256];
        if alive {
            for p in ctx.snapshot.iter().filter(|p| p.id != ctx.my_id && p.alive) {
                in_sight[p.id as usize] = true;
            }
        }
        for (id, &seen) in in_sight.iter().enumerate() {
            if !seen {
                self.ready[id] = None;
            } else if self.ready[id].is_none() {
                let delay = REACT_MIN + xorshift(&mut self.rng) % (REACT_MAX - REACT_MIN + 1);
                self.ready[id] = Some(ctx.tick + delay);
            }
        }
        nearest_enemy(ctx).is_some_and(|e| self.ready[e.id as usize].is_some_and(|at| ctx.tick >= at))
    }
}

pub struct HonestBot {
    seq: u32,
    walk: Vec2,
    rng: u32,
    reflex: Reflex,
    /// Where the last snapshot put it. Unchanged after a step means a wall
    /// (or the arena edge) is in the way, so it turns.
    last: Option<Vec2>,
}

impl HonestBot {
    pub fn new() -> Self {
        Self::with_seed(0x9E37_79B9)
    }

    /// Same player, different hand: the seed only changes the aim error
    /// sequence. How the detector's false-positive rate is measured — many
    /// honest players, not one honest player many times. A zero seed would
    /// stall xorshift at 0 (perfect aim forever), so it is remapped.
    pub fn with_seed(seed: u32) -> Self {
        let rng = if seed == 0 { 0x9E37_79B9 } else { seed };
        Self { seq: 0, walk: Vec2::new(1.0, 0.0), rng, reflex: Reflex::with_seed(rng ^ 0x5BD1_E995), last: None }
    }

    /// Next aim error in [-AIM_ERROR_RAD, AIM_ERROR_RAD].
    fn aim_error(&mut self) -> f32 {
        jitter(&mut self.rng) * AIM_ERROR_RAD
    }
}

impl Default for HonestBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for HonestBot {
    fn name(&self) -> &'static str {
        "honest"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        // Stuck: turn a quarter left. Only what a player sees of itself.
        let me = my_pos(ctx);
        if me.is_some() && me == self.last {
            self.walk = Vec2::new(-self.walk.y, self.walk.x);
        }
        self.last = me;
        let ready = self.reflex.nearest_ready(ctx);
        let (aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => {
                let bearing = unit_towards(my_pos(ctx).unwrap_or(Vec2::ZERO), e.pos);
                (rotate(bearing, self.aim_error()), ready)
            }
            None => (self.walk, false),
        };
        vec![ClientMsg::Input { seq: self.seq, tick: ctx.tick, proof: ctx.proof, move_dir: self.walk, aim, shoot }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::PlayerState;

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    #[test]
    fn no_enemy_walks_without_shooting() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO)];
        let out = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, proof: 0, snapshot: &snap });
        assert_eq!(out.len(), 1);
        if let ClientMsg::Input { shoot, move_dir, .. } = out[0] {
            assert!(!shoot);
            assert!(move_dir.len() <= 1.0 + 1e-6);
        } else {
            panic!("expected Input");
        }
    }

    #[test]
    fn aims_near_the_enemy_and_shoots() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(3.0, 4.0))];
        // bearing to (3,4) is (0.6, 0.8); every aim lands within the error
        // cone, and once the reaction delay has run out it fires every tick
        for tick in 1..=200 {
            let out = b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: &snap });
            if let ClientMsg::Input { shoot, aim, .. } = out[0] {
                if tick < 1 + REACT_MIN {
                    assert!(!shoot, "tick {tick}: fired before reacting");
                }
                if tick > REACT_MAX {
                    assert!(shoot, "tick {tick}: still holding fire");
                }
                assert!((aim.len() - 1.0).abs() < 1e-5);
                let off = (aim.x * 0.6 + aim.y * 0.8).clamp(-1.0, 1.0).acos();
                assert!(off <= AIM_ERROR_RAD + 1e-4, "tick {tick}: {off} rad off");
            } else {
                panic!("expected Input");
            }
        }
    }

    /// Not an aimbot in disguise: across many shots the aim is not exact.
    #[test]
    fn aim_is_not_perfect() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(3.0, 4.0))];
        let exact = (1..=200)
            .filter(|&tick| match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0] {
                ClientMsg::Input { aim, .. } => (aim.x - 0.6).abs() < 1e-6 && (aim.y - 0.8).abs() < 1e-6,
                _ => panic!("expected Input"),
            })
            .count();
        assert!(exact < 5, "{exact} of 200 shots were dead-on");
    }

    #[test]
    fn seed_zero_does_not_become_an_aimbot() {
        let mut b = HonestBot::with_seed(0);
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(1.0, 0.0))];
        let exact = (1..=100)
            .filter(|&tick| match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0] {
                ClientMsg::Input { aim, .. } => aim == Vec2::new(1.0, 0.0),
                _ => panic!("expected Input"),
            })
            .count();
        assert!(exact < 5, "{exact} of 100 shots were dead-on");
    }

    #[test]
    fn different_seeds_aim_differently() {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(1.0, 0.0))];
        let aim = |seed| match HonestBot::with_seed(seed).act(&BotCtx {
            tick: 1,
            my_id: 1,
            token: 0,
            proof: 0,
            snapshot: &snap,
        })[0]
        {
            ClientMsg::Input { aim, .. } => aim,
            _ => panic!("expected Input"),
        };
        assert_ne!(aim(1), aim(2));
    }

    fn walk_of(m: &ClientMsg) -> Vec2 {
        match m {
            ClientMsg::Input { move_dir, .. } => *move_dir,
            _ => panic!("expected Input"),
        }
    }

    /// Edge: a step that moved keeps the heading; the first snapshot with no
    /// change turns it a quarter left, and a second one turns it again.
    #[test]
    fn turns_when_a_step_goes_nowhere() {
        let mut b = HonestBot::new();
        let at = |x| [state(1, Vec2::new(x, 0.0))];
        assert_eq!(
            walk_of(&b.act(&BotCtx { tick: 1, my_id: 1, token: 0, proof: 0, snapshot: &at(0.0) })[0]),
            Vec2::new(1.0, 0.0)
        );
        assert_eq!(
            walk_of(&b.act(&BotCtx { tick: 2, my_id: 1, token: 0, proof: 0, snapshot: &at(5.0) })[0]),
            Vec2::new(1.0, 0.0)
        );
        assert_eq!(
            walk_of(&b.act(&BotCtx { tick: 3, my_id: 1, token: 0, proof: 0, snapshot: &at(5.0) })[0]),
            Vec2::new(-0.0, 1.0)
        );
        assert_eq!(
            walk_of(&b.act(&BotCtx { tick: 4, my_id: 1, token: 0, proof: 0, snapshot: &at(5.0) })[0]),
            Vec2::new(-1.0, -0.0)
        );
    }

    #[test]
    fn seq_increases_each_tick() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO)];
        let s1 = seq_of(&b.act(&BotCtx { tick: 1, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0]);
        let s2 = seq_of(&b.act(&BotCtx { tick: 2, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0]);
        assert!(s2 > s1);
    }

    fn shoots(b: &mut HonestBot, tick: u32, snap: &[PlayerState]) -> bool {
        match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: snap })[0] {
            ClientMsg::Input { shoot, .. } => shoot,
            _ => panic!("expected Input"),
        }
    }

    /// The first tick it fires on an enemy that stays in sight from `from`.
    fn first_shot(b: &mut HonestBot, from: u32, snap: &[PlayerState]) -> u32 {
        (from..from + 100).find(|&t| shoots(b, t, snap)).expect("never fired")
    }

    /// Edge: every delay lands in [REACT_MIN, REACT_MAX], and over many
    /// seeds both ends are drawn.
    #[test]
    fn reaction_delay_spans_its_range() {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(5.0, 0.0))];
        let delays: Vec<u32> =
            (1..=300).map(|seed| first_shot(&mut HonestBot::with_seed(seed), 10, &snap) - 10).collect();
        assert!(delays.iter().all(|d| (REACT_MIN..=REACT_MAX).contains(d)), "{delays:?}");
        assert!(delays.contains(&REACT_MIN) && delays.contains(&REACT_MAX));
    }

    /// The reaction stream is separate: the aims a seed produces are the
    /// ones it produced before reactions existed (same xorshift sequence).
    #[test]
    fn reactions_do_not_shift_the_aim_sequence() {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(1.0, 0.0))];
        let mut b = HonestBot::with_seed(42);
        let mut rng = 42;
        for tick in 1..=50 {
            let want = rotate(Vec2::new(1.0, 0.0), jitter(&mut rng) * AIM_ERROR_RAD);
            match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0] {
                ClientMsg::Input { aim, .. } => assert_eq!(aim, want, "tick {tick}"),
                _ => panic!("expected Input"),
            }
        }
    }

    /// Out of sight for one snapshot is a new encounter: the delay starts again.
    #[test]
    fn an_enemy_that_leaves_sight_is_forgotten() {
        let mut b = HonestBot::with_seed(3);
        let both = [state(1, Vec2::ZERO), state(2, Vec2::new(5.0, 0.0))];
        let alone = [state(1, Vec2::ZERO)];
        first_shot(&mut b, 1, &both);
        assert!(!shoots(&mut b, 40, &alone));
        assert!(first_shot(&mut b, 41, &both) - 41 >= REACT_MIN);
    }

    /// A dead enemy (still in the snapshot) is forgotten; its respawn is new.
    #[test]
    fn an_enemy_that_dies_is_forgotten() {
        let mut b = HonestBot::with_seed(3);
        let both = [state(1, Vec2::ZERO), state(2, Vec2::new(5.0, 0.0))];
        let dead = [state(1, Vec2::ZERO), PlayerState { alive: false, ..state(2, Vec2::new(5.0, 0.0)) }];
        first_shot(&mut b, 1, &both);
        assert!(!shoots(&mut b, 40, &dead));
        assert!(first_shot(&mut b, 41, &both) - 41 >= REACT_MIN);
    }

    /// While it is dead it forgets everyone, so it does not fire the instant
    /// it respawns on an enemy that never left sight.
    #[test]
    fn its_own_death_forgets_everyone() {
        let mut b = HonestBot::with_seed(3);
        let both = [state(1, Vec2::ZERO), state(2, Vec2::new(5.0, 0.0))];
        let me_dead = [PlayerState { alive: false, ..state(1, Vec2::ZERO) }, state(2, Vec2::new(5.0, 0.0))];
        first_shot(&mut b, 1, &both);
        assert!(!shoots(&mut b, 40, &me_dead));
        assert!(first_shot(&mut b, 41, &both) - 41 >= REACT_MIN);
    }

    /// A ready enemy that is not the nearest is not fired on: it waits for
    /// the nearest one's delay, then fires at that one.
    #[test]
    fn holds_fire_until_the_nearest_is_ready() {
        let mut b = HonestBot::with_seed(3);
        let far = [state(1, Vec2::ZERO), state(3, Vec2::new(30.0, 0.0))];
        let both = [state(1, Vec2::ZERO), state(2, Vec2::new(5.0, 0.0)), state(3, Vec2::new(30.0, 0.0))];
        first_shot(&mut b, 1, &far);
        // the far one is ready; a nearer one walks in
        for t in 40..40 + REACT_MIN {
            assert!(!shoots(&mut b, t, &both), "tick {t}: fired past the nearest");
        }
        assert!(first_shot(&mut b, 40 + REACT_MIN, &both) <= 40 + REACT_MAX);
    }

    fn seq_of(m: &ClientMsg) -> u32 {
        match m {
            ClientMsg::Input { seq, .. } => *seq,
            _ => panic!("expected Input"),
        }
    }
}
