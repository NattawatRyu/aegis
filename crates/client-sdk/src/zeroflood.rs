//! ZeroFloodBot — floods from its own address with datagrams whose session
//! token is 0. Token 0 is what a Join carries, so a relay's token check has
//! to let it through, and the origin has to read every one of them: it is the
//! cheapest traffic that crosses the edge.
//!
//! The bodies are junk (no `ClientMsg` decodes from them), so nothing but the
//! rate limit can be what stops most of it: whatever crosses is refused at
//! decode, and nothing is ever answered.
//!
//! Countered by the per-IP unauthenticated budget: the origin's source-rate
//! guard, and in front of it the relay's join-rate guard, which drops the
//! same excess at the edge so it never crosses the link.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, NO_TOKEN};

/// Datagrams per tick: 5x the per-IP unauthenticated budget of 8.
pub const PER_TICK: usize = 40;

#[derive(Default)]
pub struct ZeroFloodBot;

impl ZeroFloodBot {
    pub fn new() -> Self {
        Self
    }

    /// One datagram as it goes on the wire: token 0, then a tag no
    /// `ClientMsg` variant has.
    pub fn datagram() -> Vec<u8> {
        let mut d = NO_TOKEN.to_le_bytes().to_vec();
        d.extend([0xFF; 4]);
        d
    }
}

impl Bot for ZeroFloodBot {
    fn name(&self) -> &'static str {
        "zeroflood"
    }

    /// Never plays: everything it sends is the flood.
    fn act(&mut self, _ctx: &BotCtx) -> Vec<ClientMsg> {
        Vec::new()
    }

    fn datagrams(&mut self, _ctx: &BotCtx) -> Vec<Vec<u8>> {
        vec![Self::datagram(); PER_TICK]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{decode, split_frame};

    /// Token 0 even after it has a session, and a body that never decodes.
    #[test]
    fn every_datagram_is_token_zero_junk() {
        let mut b = ZeroFloodBot::new();
        let out = b.datagrams(&BotCtx { tick: 1, my_id: 1, token: 77, proof: 0, snapshot: &[] });
        assert_eq!(out.len(), PER_TICK);
        for d in &out {
            let (token, body) = split_frame(d).expect("carries a token");
            assert_eq!(token, NO_TOKEN);
            assert!(decode::<ClientMsg>(body).is_err());
        }
    }
}
