//! ReplayBot — resends the same captured input (fixed seq) every tick, as if
//! replaying a sniffed packet. Countered by the replay guard (G5): the first is
//! accepted, every later one with a stale seq is rejected.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct ReplayBot {
    fixed_seq: u32,
}

impl ReplayBot {
    pub fn new() -> Self {
        Self { fixed_seq: 1 }
    }
    pub fn with_seq(fixed_seq: u32) -> Self {
        Self { fixed_seq }
    }
}

impl Default for ReplayBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for ReplayBot {
    fn name(&self) -> &'static str {
        "replay"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        // Always the same seq — the hallmark of a replayed packet.
        vec![ClientMsg::Input {
            seq: self.fixed_seq,
            tick: ctx.tick,
            move_dir: Vec2::new(1.0, 0.0),
            aim: Vec2::new(1.0, 0.0),
            shoot: false,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_the_same_seq() {
        let mut b = ReplayBot::new();
        let a = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &[] });
        let c = b.act(&BotCtx { tick: 2, my_id: 1, token: 0, snapshot: &[] });
        let sa = match a[0] {
            ClientMsg::Input { seq, .. } => seq,
            _ => panic!(),
        };
        let sc = match c[0] {
            ClientMsg::Input { seq, .. } => seq,
            _ => panic!(),
        };
        assert_eq!(sa, sc);
    }
}
