//! ReflectBot — uses the server as a cannon. It forges Joins whose source
//! address is a bystander's (someone who is not playing at all). If the server
//! admits a player on a single Join, it then streams snapshots, 30 a second,
//! at the bystander: a few dozen bytes from the attacker become a flood at
//! the victim, and the victim's traffic comes from the game server, not from
//! the attacker.
//!
//! Countered by return-routability: the server answers an unproven address
//! only with a small challenge, and admits nobody until the challenge comes
//! back — which a forger, who never receives it, cannot do. So each tick it
//! also tries to skip the challenge with a guessed cookie (refused as
//! `bad_cookie`).
//!
//! The harness measures it as amplification: bytes the server sent to the
//! bystander over bytes the attacker sent in its name. Must stay <= 1.

use super::{Bot, BotCtx};
use aegis_protocol::{frame, ClientMsg, NO_TOKEN, PROTOCOL_VERSION};

/// The address it forges: a harness address no bot plays from.
pub const BYSTANDER: &str = "bystander";

/// The cookie it guesses. Any fixed value is as good as any other: the real
/// one is a keyed hash of the bystander's address it never sees.
pub const GUESS: u64 = 0x5EED_5EED_5EED_5EED;

#[derive(Default)]
pub struct ReflectBot;

impl ReflectBot {
    pub fn new() -> Self {
        Self
    }

    /// A forged Join, as it goes on the wire.
    pub fn forged_join(cookie: Option<u64>) -> Vec<u8> {
        frame(NO_TOKEN, &ClientMsg::Join { name: String::new(), protocol: PROTOCOL_VERSION, cookie })
    }
}

impl Bot for ReflectBot {
    fn name(&self) -> &'static str {
        "reflect"
    }

    fn impersonates(&self) -> Option<&'static str> {
        Some(BYSTANDER)
    }

    fn act(&mut self, _ctx: &BotCtx) -> Vec<ClientMsg> {
        Vec::new()
    }

    /// Every tick: one forged Join to be answered, one with a guessed cookie.
    fn datagrams(&mut self, _ctx: &BotCtx) -> Vec<Vec<u8>> {
        vec![Self::forged_join(None), Self::forged_join(Some(GUESS))]
    }
}
