//! GarbageBot — joins legally, then sends datagrams that are not a valid
//! `ClientMsg` at all: an empty packet, an out-of-range enum tag, and a real
//! Input truncated by one byte. Countered by the packet guard (G2) at decode:
//! each is dropped, none panics the server.
//!
//! It joins legally on purpose, and puts its real session token in front of
//! each body, so the packet guard is the *only* thing that can stop it — a
//! rejected join or a bad token would hide whether G2 works.

use super::{Bot, BotCtx};
use aegis_protocol::{encode, ClientMsg, Vec2};

/// Datagrams sent per tick — one per malformed shape.
pub const SHAPES: usize = 3;

pub struct GarbageBot {
    seq: u32,
}

impl GarbageBot {
    pub fn new() -> Self {
        Self { seq: 0 }
    }
}

impl Default for GarbageBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for GarbageBot {
    fn name(&self) -> &'static str {
        "garbage"
    }

    /// Never sends a well-formed message after joining.
    fn act(&mut self, _ctx: &BotCtx) -> Vec<ClientMsg> {
        Vec::new()
    }

    fn datagrams(&mut self, ctx: &BotCtx) -> Vec<Vec<u8>> {
        self.seq += 1;
        let real = encode(&ClientMsg::Input {
            seq: self.seq,
            tick: ctx.tick,
            move_dir: Vec2::new(1.0, 0.0),
            aim: Vec2::new(1.0, 0.0),
            shoot: false,
        });
        let bodies = [
            Vec::new(),                      // empty
            vec![0xFF; 4],                   // enum tag no ClientMsg variant has
            real[..real.len() - 1].to_vec(), // truncated mid-field
        ];
        bodies
            .into_iter()
            .map(|body| {
                let mut d = ctx.token.to_le_bytes().to_vec();
                d.extend(body);
                d
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{decode, split_frame};

    #[test]
    fn every_datagram_fails_to_decode() {
        let mut b = GarbageBot::new();
        let out = b.datagrams(&BotCtx { tick: 1, my_id: 1, token: 77, snapshot: &[] });
        assert_eq!(out.len(), SHAPES);
        for d in &out {
            let (token, body) = split_frame(d).expect("every shape carries the real token");
            assert_eq!(token, 77);
            assert!(decode::<ClientMsg>(body).is_err(), "decoded: {:?}", body);
        }
    }

    #[test]
    fn joins_legally() {
        let b = GarbageBot::new();
        assert!(matches!(
            b.join(),
            ClientMsg::Join { protocol, .. } if protocol == aegis_protocol::PROTOCOL_VERSION
        ));
    }
}
