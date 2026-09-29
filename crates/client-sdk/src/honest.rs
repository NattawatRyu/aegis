//! HonestBot — the baseline. Walks forward at a legal unit speed, aims at the
//! nearest visible enemy *with human error*, increments its seq every tick.
//! Passes every guard clean; the other bots are measured against it.
//!
//! The aim error is what makes it a baseline. Aiming exactly down the bearing
//! is what the aimbot does; an honest bot that did the same would be an aimbot
//! with a different name, and the detector (pillar C) would have nothing to
//! tell apart. The error is deterministic (seeded xorshift), so runs repeat.

use super::{my_pos, nearest_enemy, unit_towards, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

/// Largest aim error either side of the true bearing, in radians (~8.6°).
pub const AIM_ERROR_RAD: f32 = 0.15;

pub struct HonestBot {
    seq: u32,
    walk: Vec2,
    rng: u32,
}

impl HonestBot {
    pub fn new() -> Self {
        Self { seq: 0, walk: Vec2::new(1.0, 0.0), rng: 0x9E37_79B9 }
    }

    /// Next aim error in [-AIM_ERROR_RAD, AIM_ERROR_RAD] (xorshift32).
    fn aim_error(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * AIM_ERROR_RAD
    }
}

fn rotate(v: Vec2, a: f32) -> Vec2 {
    let (s, c) = a.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
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
        let (aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => {
                let bearing = unit_towards(my_pos(ctx).unwrap_or(Vec2::ZERO), e.pos);
                (rotate(bearing, self.aim_error()), true)
            }
            None => (self.walk, false),
        };
        vec![ClientMsg::Input { seq: self.seq, tick: ctx.tick, move_dir: self.walk, aim, shoot }]
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
        let out = b.act(&BotCtx { tick: 1, my_id: 1, snapshot: &snap });
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
        // bearing to (3,4) is (0.6, 0.8); every shot lands within the error cone
        for tick in 1..=200 {
            let out = b.act(&BotCtx { tick, my_id: 1, snapshot: &snap });
            if let ClientMsg::Input { shoot, aim, .. } = out[0] {
                assert!(shoot);
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
            .filter(|&tick| match b.act(&BotCtx { tick, my_id: 1, snapshot: &snap })[0] {
                ClientMsg::Input { aim, .. } => (aim.x - 0.6).abs() < 1e-6 && (aim.y - 0.8).abs() < 1e-6,
                _ => panic!("expected Input"),
            })
            .count();
        assert!(exact < 5, "{exact} of 200 shots were dead-on");
    }

    #[test]
    fn seq_increases_each_tick() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO)];
        let s1 = seq_of(&b.act(&BotCtx { tick: 1, my_id: 1, snapshot: &snap })[0]);
        let s2 = seq_of(&b.act(&BotCtx { tick: 2, my_id: 1, snapshot: &snap })[0]);
        assert!(s2 > s1);
    }

    fn seq_of(m: &ClientMsg) -> u32 {
        match m {
            ClientMsg::Input { seq, .. } => *seq,
            _ => panic!("expected Input"),
        }
    }
}
