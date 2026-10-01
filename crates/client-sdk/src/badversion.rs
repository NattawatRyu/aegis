//! BadVersionBot — joins with the wrong protocol version, as a mismatched or
//! spoofed client would, then keeps sending legal inputs as if it were in.
//!
//! Countered twice: the version guard (G1) rejects the join, and the joined
//! guard (G0) rejects every input from a player the server never admitted.
//! The inputs themselves are legal on purpose, so only G0 can stop them.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2, PROTOCOL_VERSION};

pub struct BadVersionBot {
    bad: u16,
    seq: u32,
}

impl BadVersionBot {
    pub fn new() -> Self {
        // deliberately not PROTOCOL_VERSION
        Self::with_version(PROTOCOL_VERSION.wrapping_add(999))
    }
    pub fn with_version(bad: u16) -> Self {
        Self { bad, seq: 0 }
    }
}

impl Default for BadVersionBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for BadVersionBot {
    fn name(&self) -> &'static str {
        "badversion"
    }

    fn join(&self) -> ClientMsg {
        ClientMsg::Join { name: "badver".into(), protocol: self.bad, cookie: None }
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        vec![ClientMsg::Input {
            seq: self.seq,
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
    fn joins_with_wrong_version() {
        let b = BadVersionBot::new();
        match b.join() {
            ClientMsg::Join { protocol, .. } => assert_ne!(protocol, PROTOCOL_VERSION),
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn then_sends_legal_inputs() {
        let mut b = BadVersionBot::new();
        let out = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &[] });
        match out[0] {
            ClientMsg::Input { move_dir, .. } => assert!(move_dir.len() <= 1.0),
            _ => panic!("expected Input"),
        }
    }
}
