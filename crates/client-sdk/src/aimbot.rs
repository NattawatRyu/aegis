//! AimbotBot — the one that gets through.
//!
//! It reads the snapshot the server sent (exactly what a real client receives),
//! computes a *perfect* unit-length aim at the nearest enemy, and fires. Every
//! value it sends is legal: unit move, unit aim, strictly-increasing seq, one
//! input per tick. So NO guard rejects it.
//!
//! That is the point. This bot proves that server-authoritative netcode + the
//! input-trust guards are necessary but not sufficient: an aimbot plays inside
//! the rules. Catching it needs the detector (pillar C, behavioural analysis of
//! the telemetry stream), and taking away its inputs needs snapshot culling
//! (pillar D). The lab exists so we can see this with our own eyes before we
//! build C and D.

use super::{my_pos, nearest_enemy, unit_towards, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct AimbotBot {
    seq: u32,
}

impl AimbotBot {
    pub fn new() -> Self {
        Self { seq: 0 }
    }
}

impl Default for AimbotBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for AimbotBot {
    fn name(&self) -> &'static str {
        "aimbot"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        let me = my_pos(ctx).unwrap_or(Vec2::ZERO);
        let (aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => (unit_towards(me, e.pos), true),
            None => (Vec2::new(1.0, 0.0), false),
        };
        vec![ClientMsg::Input {
            seq: self.seq,
            tick: ctx.tick,
            move_dir: Vec2::ZERO, // stand still, just snap-aim
            aim,
            shoot,
        }]
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
    fn aims_perfectly_at_nearest_enemy() {
        let mut b = AimbotBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(10.0, 0.0)), state(3, Vec2::new(0.0, 40.0))];
        let out = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &snap });
        if let ClientMsg::Input { aim, shoot, .. } = out[0] {
            assert!(shoot);
            // nearest enemy is id 2 at (10,0); perfect aim is exactly (1,0)
            let dot = aim.x * 1.0 + aim.y * 0.0;
            assert!((dot - 1.0).abs() < 1e-6);
        } else {
            panic!("expected Input");
        }
    }

    #[test]
    fn every_value_it_sends_is_guard_legal() {
        let mut b = AimbotBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(5.0, 5.0))];
        let s1 = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &snap });
        let s2 = b.act(&BotCtx { tick: 2, my_id: 1, token: 0, snapshot: &snap });
        for (v, prev, cur) in [(&s1, 0u32, 1u32), (&s2, 1, 2)] {
            assert_eq!(v.len(), 1); // one input per tick -> rate guard clean
            if let ClientMsg::Input { seq, move_dir, aim, .. } = v[0] {
                assert!(seq > prev && seq == cur); // strictly increasing -> replay clean
                assert!(move_dir.len() <= 1.0 + 1e-6); // unit -> move_speed clean
                assert!(aim.x.is_finite() && aim.y.is_finite()); // -> sanity clean
            } else {
                panic!("expected Input");
            }
        }
    }
}
