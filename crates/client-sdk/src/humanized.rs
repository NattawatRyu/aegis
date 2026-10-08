//! HumanizedAimbot — the aimbot that learned about the aim_exact detector.
//!
//! Same snap as [`super::aimbot`], plus a small random twist on every shot so
//! no two land dead-on the bearing. Commercial aimbots ship exactly this
//! ("smoothing", "humanizer"). Every input is guard-legal, and the jitter is
//! wider than the detector's EXACT_RAD, so aim_exact sees nothing.
//!
//! What it cannot hide is the outcome: the jitter is small next to a hitbox at
//! fight range, so it still hits nearly every shot (~0.98). So does an honest
//! rusher fighting point-blank, which is why accuracy left the standard suite
//! (2026-10-08). What it still cannot hide is that it fires the tick it sees
//! you — the reaction detector flags it for that.

use super::{jitter, my_pos, nearest_enemy, rotate, unit_towards, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

/// Largest twist either side of the true bearing (~1.7°). 30x the detector's
/// EXACT_RAD, 1/5 of the honest bot's error.
pub const JITTER_RAD: f32 = 0.03;

pub struct HumanizedAimbot {
    seq: u32,
    rng: u32,
}

impl HumanizedAimbot {
    pub fn new() -> Self {
        Self { seq: 0, rng: 0x85EB_CA6B }
    }
}

impl Default for HumanizedAimbot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for HumanizedAimbot {
    fn name(&self) -> &'static str {
        "humanized"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        let me = my_pos(ctx).unwrap_or(Vec2::ZERO);
        let (aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => (rotate(unit_towards(me, e.pos), jitter(&mut self.rng) * JITTER_RAD), true),
            None => (Vec2::new(1.0, 0.0), false),
        };
        vec![ClientMsg::Input { seq: self.seq, tick: ctx.tick, move_dir: Vec2::ZERO, aim, shoot }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::PlayerState;

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    /// Almost never dead-on (what hides it from aim_exact), never far off
    /// (what keeps it hitting).
    #[test]
    fn aim_is_close_but_not_exact() {
        let mut b = HumanizedAimbot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(10.0, 0.0))];
        let mut exact = 0;
        for tick in 1..=300 {
            match b.act(&BotCtx { tick, my_id: 1, token: 0, snapshot: &snap })[0] {
                ClientMsg::Input { aim, shoot, .. } => {
                    assert!(shoot);
                    let off = aim.y.atan2(aim.x).abs();
                    assert!(off <= JITTER_RAD + 1e-6, "tick {tick}: {off}");
                    exact += (off < 1e-3) as u32;
                }
                _ => panic!("expected Input"),
            }
        }
        assert!(exact < 30, "{exact} of 300 dead-on");
    }
}
