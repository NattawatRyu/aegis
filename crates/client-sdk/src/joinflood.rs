//! JoinFloodBot — one machine, many source ports, a legal Join from each. If
//! the server treats every IP:port as a new player and never lets one go, a
//! single attacker fills every player slot in about a second and nobody else
//! can join until a restart.
//!
//! Countered by two server rules: a cap on live sessions per source IP, and an
//! idle timeout that evicts sessions that never send anything (which every
//! session it opens is).
//!
//! It stays inside the per-IP datagram budget on purpose, so the session cap
//! is the *only* thing that can stop it.

use super::{Bot, BotCtx};
use aegis_protocol::{frame, ClientMsg, NO_TOKEN, PROTOCOL_VERSION};

/// Source ports it joins from (index 0, its own, joins normally at tick 0).
/// Enough to take every one of the 255 player ids.
pub const SOURCES: u16 = 255;

/// Joins per tick — the per-IP datagram budget, no more.
pub const JOINS_PER_TICK: u16 = 8;

pub struct JoinFloodBot {
    next: u16,
}

impl JoinFloodBot {
    pub fn new() -> Self {
        Self { next: 0 }
    }
}

impl Default for JoinFloodBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for JoinFloodBot {
    fn name(&self) -> &'static str {
        "joinflood"
    }

    fn sources(&self) -> u16 {
        SOURCES + 1
    }

    /// Never plays: everything it sends is a routed Join.
    fn act(&mut self, _ctx: &BotCtx) -> Vec<ClientMsg> {
        Vec::new()
    }

    fn routed(&mut self, _ctx: &BotCtx) -> Vec<(u16, Vec<u8>)> {
        let join = frame(NO_TOKEN, &ClientMsg::Join { name: self.name().into(), protocol: PROTOCOL_VERSION });
        (0..JOINS_PER_TICK)
            .map(|_| {
                self.next = self.next % SOURCES + 1; // 1..=SOURCES, then around again
                (self.next, join.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_from_every_source_port_in_turn() {
        let mut b = JoinFloodBot::new();
        let ctx = BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &[] };
        let mut seen: Vec<u16> = Vec::new();
        for _ in 0..(SOURCES / JOINS_PER_TICK + 1) {
            let out = b.routed(&ctx);
            assert_eq!(out.len(), JOINS_PER_TICK as usize);
            seen.extend(out.iter().map(|(s, _)| *s));
        }
        assert_eq!(&seen[..3], &[1, 2, 3]);
        assert_eq!(seen[SOURCES as usize], 1); // wrapped
        assert!(seen.iter().all(|&s| (1..=SOURCES).contains(&s)));
    }
}
