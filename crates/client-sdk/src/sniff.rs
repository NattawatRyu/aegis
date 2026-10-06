//! SniffBot — the on-path attacker: same café Wi-Fi as its victim.
//!
//! It reads every datagram the victim sends ([`Bot::taps`]) and forges its
//! own from the victim's address ([`Bot::impersonates`]). From the victim's
//! last input it takes the session token and the seq; each tick it gets an
//! input under that token, with a seq far ahead, to the server before the
//! victim's. Accepted, it moves the victim's player where the sniffer says,
//! and the victim's own input that tick is refused (one input per player per
//! tick). The victim is locked out of its own player.
//!
//! The session token only proves a datagram came from the address it was
//! issued to; an on-path attacker can send from that address. What stops it
//! is not being able to read the token at all: behind a relay the client's
//! leg is sealed (protocol v3), so the first 8 bytes it reads are the
//! victim's session id, not its token, and the relay drops its forgery
//! (`bad_token`) before it crosses. Its own datagrams are sealed under its
//! own keys — it is a player too — so they open; they just say nothing the
//! relay accepts.

use super::{Bot, BotCtx};
use aegis_protocol::{decode, frame, split_frame, ClientMsg, Vec2};

/// The bot it listens to and speaks as.
pub const VICTIM: &str = "honest";

/// How far ahead of the victim's seq its forged input goes: past anything
/// the victim will send in a match, so even a victim input that wins a tick
/// is a replay.
pub const AHEAD: u32 = 1_000_000;

pub struct SniffBot {
    /// What it read off the victim's last datagram: the first 8 bytes as a
    /// token, and the seq if the rest decoded as an input.
    heard: Option<(u64, u32)>,
}

impl SniffBot {
    pub fn new() -> Self {
        Self { heard: None }
    }
}

impl Default for SniffBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for SniffBot {
    fn name(&self) -> &'static str {
        "sniff"
    }

    fn impersonates(&self) -> Option<&'static str> {
        Some(VICTIM)
    }

    fn taps(&self) -> Option<&'static str> {
        Some(VICTIM)
    }

    /// Whatever the wire shows. Sealed, the "token" is the victim's session
    /// id and the body decodes as nothing: it tries anyway.
    fn overheard(&mut self, wire: &[u8]) {
        self.heard = split_frame(wire).map(|(token, body)| match decode::<ClientMsg>(body) {
            Ok(ClientMsg::Input { seq, .. }) => (token, seq),
            _ => (token, 0),
        });
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        match self.heard {
            Some((_, seq)) => vec![ClientMsg::Input {
                seq: seq.saturating_add(AHEAD),
                tick: ctx.tick,
                move_dir: Vec2::new(0.0, -1.0), // walk the victim where it chooses
                aim: Vec2::new(1.0, 0.0),
                shoot: false,
            }],
            None => Vec::new(),
        }
    }

    /// Framed under the token it overheard, not its own.
    fn datagrams(&mut self, ctx: &BotCtx) -> Vec<Vec<u8>> {
        let token = self.heard.map(|(t, _)| t);
        self.act(ctx).iter().filter_map(|m| token.map(|t| frame(t, m))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> BotCtx<'static> {
        BotCtx { tick: 9, my_id: 2, token: 0xAAAA, snapshot: &[] }
    }

    /// Reading a plain input, it speaks under the victim's token, far ahead
    /// of the victim's seq — never under its own token.
    #[test]
    fn a_plain_input_gives_it_the_token_and_the_seq() {
        let mut b = SniffBot::new();
        assert!(b.datagrams(&ctx()).is_empty(), "nothing heard, nothing to say");
        let victim = ClientMsg::Input { seq: 41, tick: 9, move_dir: Vec2::ZERO, aim: Vec2::ZERO, shoot: true };
        b.overheard(&frame(0x7070_7070, &victim));
        let out = b.datagrams(&ctx());
        assert_eq!(out.len(), 1);
        let (token, body) = split_frame(&out[0]).unwrap();
        assert_eq!(token, 0x7070_7070);
        assert!(matches!(decode::<ClientMsg>(body), Ok(ClientMsg::Input { seq, .. }) if seq == 41 + AHEAD));
    }

    /// Sealed bytes give it 8 bytes that are not a token and no seq; it
    /// sends under them anyway.
    #[test]
    fn sealed_bytes_give_it_nothing_true() {
        let mut b = SniffBot::new();
        b.overheard(&[0x11; 80]);
        let out = b.datagrams(&ctx());
        assert_eq!(split_frame(&out[0]).unwrap().0, u64::from_le_bytes([0x11; 8]));
    }
}
