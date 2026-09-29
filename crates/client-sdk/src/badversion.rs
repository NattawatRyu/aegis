//! BadVersionBot — tries to join with the wrong protocol version, as a mismatched
//! or spoofed client would. Countered by the version guard (G1) at Join.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, PROTOCOL_VERSION};

pub struct BadVersionBot {
    bad: u16,
}

impl BadVersionBot {
    pub fn new() -> Self {
        // deliberately not PROTOCOL_VERSION
        Self { bad: PROTOCOL_VERSION.wrapping_add(999) }
    }
    pub fn with_version(bad: u16) -> Self {
        Self { bad }
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

    fn act(&mut self, _ctx: &BotCtx) -> Vec<ClientMsg> {
        vec![ClientMsg::Join { name: "badver".into(), protocol: self.bad }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_with_wrong_version() {
        let mut b = BadVersionBot::new();
        let out = b.act(&BotCtx { tick: 0, my_id: 1, snapshot: &[] });
        match &out[0] {
            ClientMsg::Join { protocol, .. } => assert_ne!(*protocol, PROTOCOL_VERSION),
            _ => panic!("expected Join"),
        }
    }
}
