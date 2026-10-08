//! FloodBot — emits many valid inputs in a single tick to try to act more than
//! once per tick, and to make the server pay to decode each one. Countered by
//! two guards: source_rate drops everything past its per-IP cap before decode,
//! then input_rate (G4) folds only the first of the rest. Each input has an
//! increasing seq and a legal move vector, so *only* the rate guards should
//! stop them.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct FloodBot {
    seq: u32,
    per_tick: usize,
}

impl FloodBot {
    pub fn new(per_tick: usize) -> Self {
        Self { seq: 0, per_tick }
    }
}

/// Datagrams per tick from [`FloodBot::default`] — well past the server's
/// per-IP source-rate cap, so both rate guards have work to do.
pub const DEFAULT_PER_TICK: usize = 50;

impl Default for FloodBot {
    fn default() -> Self {
        Self::new(DEFAULT_PER_TICK)
    }
}

impl Bot for FloodBot {
    fn name(&self) -> &'static str {
        "flood"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = Vec::with_capacity(self.per_tick);
        for _ in 0..self.per_tick {
            self.seq += 1;
            out.push(ClientMsg::Input {
                seq: self.seq,
                tick: ctx.tick,
                proof: ctx.proof,
                move_dir: Vec2::new(1.0, 0.0),
                aim: Vec2::new(1.0, 0.0),
                shoot: false,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_many_inputs_same_tick() {
        let mut b = FloodBot::new(50);
        let out = b.act(&BotCtx { tick: 7, my_id: 1, token: 0, proof: 0, snapshot: &[] });
        assert_eq!(out.len(), 50);
        for m in &out {
            match m {
                ClientMsg::Input { tick, .. } => assert_eq!(*tick, 7),
                _ => panic!("expected Input"),
            }
        }
    }
}
