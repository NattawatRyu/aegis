//! G-1 source-rate guard
//! STOPS: datagram floods — from a joined player or from a source that never
//!        joined — before the server spends a decode on them
//! HOW:   count datagrams per key per tick; past `MAX_PER_TICK` the datagram
//!        is dropped undecoded. Two budgets, so a forger cannot spend a
//!        player's:
//!          - authenticated datagrams (right address, right session token)
//!            are keyed by the player;
//!          - everything else is keyed by source IP, not IP:port — changing
//!            port is free for an attacker, changing IP is not.
//! EDGE:  the `MAX_PER_TICK`-th datagram in a tick passes; the next is
//!        dropped; a new tick starts the count again; two keys never share a
//!        budget.
//!
//! Stage guard: runs right after the token check, before decode. Drops here
//! are counters only ([`crate::NetStats`]), never telemetry records: a flood
//! must not be able to grow the server's memory one record per packet. State
//! is reset every tick, so it is bounded by what one tick can read.

use super::RejectReason;
use std::collections::HashMap;
use std::hash::Hash;

/// 8x an honest client, which sends one input per tick. Headroom for a few
/// players behind one NAT and for a burst after a stall.
pub const MAX_PER_TICK: u32 = 8;

pub struct SourceRate<K> {
    tick: u32,
    counts: HashMap<K, u32>,
}

impl<K: Hash + Eq> Default for SourceRate<K> {
    fn default() -> Self {
        Self { tick: 0, counts: HashMap::new() }
    }
}

impl<K: Hash + Eq> SourceRate<K> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&mut self, tick: u32, key: K) -> Result<(), RejectReason> {
        if tick != self.tick {
            self.tick = tick;
            self.counts.clear();
        }
        let n = self.counts.entry(key).or_insert(0);
        *n += 1;
        if *n > MAX_PER_TICK {
            Err(RejectReason::SourceRate)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const A: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const B: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    #[test]
    fn cap_passes_and_cap_plus_one_drops() {
        let mut g = SourceRate::new();
        for _ in 0..MAX_PER_TICK {
            assert_eq!(g.check(1, A), Ok(()));
        }
        assert_eq!(g.check(1, A), Err(RejectReason::SourceRate));
        assert_eq!(g.check(1, A), Err(RejectReason::SourceRate));
    }

    #[test]
    fn next_tick_starts_over() {
        let mut g = SourceRate::new();
        for _ in 0..=MAX_PER_TICK {
            let _ = g.check(1, A);
        }
        assert_eq!(g.check(2, A), Ok(()));
    }

    #[test]
    fn ips_do_not_share_a_budget() {
        let mut g = SourceRate::new();
        for _ in 0..=MAX_PER_TICK {
            let _ = g.check(1, A);
        }
        assert_eq!(g.check(1, B), Ok(()));
    }
}
