//! E-1 join-rate edge guard
//! STOPS: a flood of token-0 datagrams (Joins, or junk that claims to be one)
//!        from one IP crossing the link — the one kind of client traffic the
//!        stateless token check has to let through
//! HOW:   count token-0 datagrams per source IP per window; past
//!        `MAX_PER_WINDOW` they are dropped at the relay. The window is the
//!        origin's tick and the cap is the origin's own unauthenticated
//!        budget (`source_rate::MAX_PER_TICK`, asserted equal by the harness),
//!        so what is dropped here is what the origin's source-rate guard would
//!        have dropped undecoded anyway — no player's record changes.
//! EDGE:  the `MAX_PER_WINDOW`-th token-0 datagram from an IP in a window
//!        crosses; the next is dropped; a new window starts the count again;
//!        two IPs never share a budget; with the table full, a new IP is not
//!        tracked and crosses (the origin's guard still stands behind it).
//!
//! Keyed by IP, not IP:port: changing port is free for an attacker. State is
//! cleared every window and capped at `MAX_IPS`, so it is bounded twice — by
//! what one window can read, and by a hard limit.

use std::collections::HashMap;
use std::net::IpAddr;

/// The origin's unauthenticated budget per IP per tick.
pub const MAX_PER_WINDOW: u32 = 8;

/// IPs tracked in one window. Past this, new IPs go untracked (fail open):
/// a spoofed many-source flood must not grow the relay without bound, and
/// refusing untracked IPs would let it lock everyone out of joining.
pub const MAX_IPS: usize = 1 << 16;

#[derive(Default)]
pub struct JoinRate {
    window: u32,
    counts: HashMap<IpAddr, u32>,
}

impl JoinRate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether one more token-0 datagram from `ip` in `window` may cross.
    pub fn allow(&mut self, window: u32, ip: IpAddr) -> bool {
        if window != self.window {
            self.window = window;
            self.counts.clear();
        }
        if self.counts.len() >= MAX_IPS && !self.counts.contains_key(&ip) {
            return true;
        }
        let n = self.counts.entry(ip).or_insert(0);
        *n += 1;
        *n <= MAX_PER_WINDOW
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const A: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const B: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    #[test]
    fn cap_crosses_and_cap_plus_one_drops() {
        let mut g = JoinRate::new();
        for _ in 0..MAX_PER_WINDOW {
            assert!(g.allow(0, A));
        }
        assert!(!g.allow(0, A));
        assert!(!g.allow(0, A));
    }

    #[test]
    fn next_window_starts_over() {
        let mut g = JoinRate::new();
        for _ in 0..=MAX_PER_WINDOW {
            g.allow(1, A);
        }
        assert!(g.allow(2, A));
    }

    #[test]
    fn ips_do_not_share_a_budget() {
        let mut g = JoinRate::new();
        for _ in 0..=MAX_PER_WINDOW {
            g.allow(1, A);
        }
        assert!(g.allow(1, B));
    }

    /// At the hard cap: an IP already tracked is still limited, a new one
    /// crosses untracked, and the table does not grow past the cap.
    #[test]
    fn a_full_table_fails_open_for_new_ips_only() {
        let mut g = JoinRate::new();
        for i in 0..MAX_IPS as u32 {
            assert!(g.allow(0, IpAddr::V4(Ipv4Addr::from(i))));
        }
        let tracked = IpAddr::V4(Ipv4Addr::from(0));
        for _ in 1..MAX_PER_WINDOW {
            assert!(g.allow(0, tracked));
        }
        assert!(!g.allow(0, tracked));
        for _ in 0..=MAX_PER_WINDOW {
            assert!(g.allow(0, A));
        }
        assert_eq!(g.counts.len(), MAX_IPS);
    }
}
