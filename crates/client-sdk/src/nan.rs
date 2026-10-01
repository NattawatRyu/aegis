//! NanBot — sends non-finite vectors: a NaN aim on odd ticks, an infinite move
//! on even ticks. A NaN aim silently breaks hitscan; an infinite move poisons
//! the position. Countered by the sanity guard (G3), which rejects the input
//! before the sim ever sees it.
//!
//! Every other field is legal (increasing seq, one input per tick), so sanity
//! is the only guard that can stop it.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct NanBot {
    seq: u32,
}

impl NanBot {
    pub fn new() -> Self {
        Self { seq: 0 }
    }
}

impl Default for NanBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for NanBot {
    fn name(&self) -> &'static str {
        "nan"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        let (move_dir, aim) = if ctx.tick % 2 == 1 {
            (Vec2::new(1.0, 0.0), Vec2::new(f32::NAN, 0.0))
        } else {
            (Vec2::new(f32::INFINITY, 0.0), Vec2::new(1.0, 0.0))
        };
        vec![ClientMsg::Input { seq: self.seq, tick: ctx.tick, move_dir, aim, shoot: true }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vecs(b: &mut NanBot, tick: u32) -> (Vec2, Vec2) {
        match b.act(&BotCtx { tick, my_id: 1, token: 0, snapshot: &[] })[0] {
            ClientMsg::Input { move_dir, aim, .. } => (move_dir, aim),
            _ => panic!("expected Input"),
        }
    }

    #[test]
    fn odd_tick_nan_aim_even_tick_infinite_move() {
        let mut b = NanBot::new();
        let (m1, a1) = vecs(&mut b, 1);
        assert!(m1.x.is_finite() && a1.x.is_nan());
        let (m2, a2) = vecs(&mut b, 2);
        assert!(m2.x.is_infinite() && a2.x.is_finite());
    }
}
