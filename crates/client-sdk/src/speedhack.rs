//! SpeedhackBot — sends an over-length move vector to try to travel faster than
//! MOVE_SPEED. Countered by the move_speed guard (G6), which clamps it to unit.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct SpeedhackBot {
    seq: u32,
    mult: f32,
}

impl SpeedhackBot {
    pub fn new() -> Self {
        Self { seq: 0, mult: 10.0 }
    }
    pub fn with_mult(mult: f32) -> Self {
        Self { seq: 0, mult }
    }
}

impl Default for SpeedhackBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for SpeedhackBot {
    fn name(&self) -> &'static str {
        "speedhack"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        vec![ClientMsg::Input {
            seq: self.seq,
            tick: ctx.tick,
            proof: ctx.proof,
            move_dir: Vec2::new(self.mult, 0.0),
            aim: Vec2::new(1.0, 0.0),
            shoot: false,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_oversized_move_vector() {
        let mut b = SpeedhackBot::new();
        let out = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, proof: 0, snapshot: &[] });
        if let ClientMsg::Input { move_dir, .. } = out[0] {
            assert!(move_dir.len() > 1.0);
        } else {
            panic!("expected Input");
        }
    }
}
