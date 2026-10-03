//! DirectBot — the client that found the origin.
//!
//! It plays exactly like [`super::honest`], but every datagram — its join
//! included — goes to the origin server's own address instead of the relay's.
//! Room-DDoS starts this way: the attacker learns the real server address (a
//! leaked IP, a peer-to-peer lobby, a misconfigured DNS record) and aims
//! there, past whatever stands in front.
//!
//! Without a relay the origin's address is the one every client is given, so
//! it joins and plays. Behind one, the origin hears only the relay: nothing
//! this bot sends is read, it never joins, and it is never sent a byte.

use super::honest::HonestBot;
use super::{Bot, BotCtx};
use aegis_protocol::ClientMsg;

pub struct DirectBot {
    play: HonestBot,
}

impl DirectBot {
    pub fn new() -> Self {
        Self { play: HonestBot::with_seed(0x27D4_EB2F) }
    }
}

impl Default for DirectBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for DirectBot {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.play.act(ctx)
    }

    fn bypasses_relay(&self) -> bool {
        true
    }
}
