//! E-2 edge cookie (return routability at the relay)
//! STOPS: unproven Joins crossing the link — above all a flood of them from
//!        many forged source IPs, which the per-IP budget ([`super::join_rate`])
//!        cannot thin because every forged IP gets a fresh budget
//! HOW:   the relay answers a cookieless Join itself with a challenge whose
//!        cookie = SipHash-2-4(link key, cookie domain, source address, time
//!        bucket), and forwards only a Join that brings that cookie back. Only
//!        a client that receives at its address can, so a forger never gets a
//!        Join across. Stateless: recomputed, never stored. The origin, which
//!        hears only from the relay over the MAC'd link, trusts a cookie the
//!        relay forwarded (`Server::trust_edge_cookies`).
//! EDGE:  a cookie is good in the bucket it was issued and the next one;
//!        two buckets later it is refused; a cookie for one address is
//!        refused from any other; one flipped bit is refused.
//!
//! Runs after the token-0 budget, so a challenge is never sent to an IP more
//! often than the origin itself would have sent one — a forged Join cannot
//! make the relay a better reflector than the origin was.

use aegis_protocol::{edge_cookie, LinkKey};
use std::net::SocketAddr;

/// Windows (origin ticks) per cookie bucket — the origin's own
/// `cookie::BUCKET_TICKS` (asserted equal by the harness).
pub const BUCKET_WINDOWS: u32 = 60;

/// The cookie for `client` in `window`.
pub fn issue(key: &LinkKey, client: SocketAddr, window: u32) -> u64 {
    edge_cookie(key, client, window / BUCKET_WINDOWS)
}

/// Whether `cookie` was issued to `client` in this bucket or the last.
pub fn valid(key: &LinkKey, client: SocketAddr, window: u32, cookie: u64) -> bool {
    let now = window / BUCKET_WINDOWS;
    [Some(now), now.checked_sub(1)].into_iter().flatten().any(|b| edge_cookie(key, client, b) == cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    static KEY: std::sync::LazyLock<LinkKey> = std::sync::LazyLock::new(|| LinkKey::new([5; 32]));

    fn a(port: u16) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, 1], port))
    }

    #[test]
    fn good_in_its_bucket_and_the_next_not_after() {
        let c = issue(&KEY, a(1), 0);
        assert!(valid(&KEY, a(1), 0, c));
        assert!(valid(&KEY, a(1), BUCKET_WINDOWS - 1, c));
        assert!(valid(&KEY, a(1), 2 * BUCKET_WINDOWS - 1, c));
        assert!(!valid(&KEY, a(1), 2 * BUCKET_WINDOWS, c));
    }

    #[test]
    fn bound_to_address_and_key() {
        let c = issue(&KEY, a(1), 0);
        assert!(!valid(&KEY, a(2), 0, c));
        assert!(!valid(&KEY, SocketAddr::from(([10, 0, 0, 2], 1)), 0, c));
        assert!(!valid(&LinkKey::new([6; 32]), a(1), 0, c));
    }

    #[test]
    fn any_flipped_bit_is_refused() {
        let c = issue(&KEY, a(1), 0);
        for bit in 0..64 {
            assert!(!valid(&KEY, a(1), 0, c ^ (1 << bit)), "bit {bit}");
        }
    }
}
