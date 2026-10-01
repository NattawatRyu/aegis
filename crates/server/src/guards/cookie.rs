//! G-4 cookie guard (return routability)
//! STOPS: reflection / amplification — a Join forged in a bystander's name
//!        that would make the server admit the bystander and stream it
//!        snapshots, 30 a second, turning a few bytes from the attacker into
//!        a flood at the victim
//! HOW:   the server admits nobody on a first Join. It answers with a cookie
//!        = SipHash-2-4(secret key, source address, time bucket) and admits
//!        only a Join that carries that cookie back. Only a client that
//!        *receives* at the address can do that. Stateless: an unproven Join
//!        costs the server no memory.
//! EDGE:  a cookie is good in the bucket it was issued and the next one
//!        (2–4 s); two buckets later it is refused; a cookie for one address
//!        is refused from any other; one flipped bit is refused.
//!
//! Stage guard: runs at admission, after the version check. Does not stop a
//! client that really receives at many addresses (its own ports, a botnet) —
//! that is [`super::ip_sessions`]'s job and, past that, a relay's.

use super::RejectReason;
use siphasher::sip::SipHasher24;
use std::hash::Hasher;
use std::net::{IpAddr, SocketAddr};

/// Ticks per cookie bucket (2 s at 30 Hz).
pub const BUCKET_TICKS: u32 = 60;

pub struct CookieJar {
    key: [u8; 16],
}

impl CookieJar {
    /// A jar with a fresh key from the OS CSPRNG. Every server process has its
    /// own, so a cookie never outlives the server that issued it.
    pub fn new() -> Self {
        let mut key = [0u8; 16];
        getrandom::fill(&mut key).expect("aegis-server: OS random source unavailable");
        Self { key }
    }

    #[cfg(test)]
    fn with_key(key: [u8; 16]) -> Self {
        Self { key }
    }

    fn mac(&self, from: SocketAddr, bucket: u32) -> u64 {
        let mut h = SipHasher24::new_with_key(&self.key);
        match from.ip() {
            IpAddr::V4(ip) => h.write(&ip.octets()),
            IpAddr::V6(ip) => h.write(&ip.octets()),
        }
        h.write(&from.port().to_le_bytes());
        h.write(&bucket.to_le_bytes());
        h.finish()
    }

    /// The cookie for `from` at `tick`.
    pub fn issue(&self, from: SocketAddr, tick: u32) -> u64 {
        self.mac(from, tick / BUCKET_TICKS)
    }

    pub fn verify(&self, from: SocketAddr, tick: u32, cookie: u64) -> Result<(), RejectReason> {
        let now = tick / BUCKET_TICKS;
        let fresh = [Some(now), now.checked_sub(1)];
        // Compared as integers: one instruction each, no early exit on a
        // partial match to time.
        if fresh.into_iter().flatten().any(|b| self.mac(from, b) == cookie) {
            Ok(())
        } else {
            Err(RejectReason::BadCookie)
        }
    }
}

impl Default for CookieJar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jar() -> CookieJar {
        CookieJar::with_key(*b"0123456789abcdef")
    }

    fn a(port: u16) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, 1], port))
    }

    #[test]
    fn good_in_its_bucket_and_the_next_not_after() {
        let j = jar();
        let c = j.issue(a(1), 0);
        assert_eq!(j.verify(a(1), 0, c), Ok(()));
        assert_eq!(j.verify(a(1), BUCKET_TICKS - 1, c), Ok(()));
        assert_eq!(j.verify(a(1), 2 * BUCKET_TICKS - 1, c), Ok(())); // last tick of the next bucket
        assert_eq!(j.verify(a(1), 2 * BUCKET_TICKS, c), Err(RejectReason::BadCookie));
    }

    #[test]
    fn bound_to_the_address_it_was_issued_to() {
        let j = jar();
        let c = j.issue(a(1), 0);
        assert_eq!(j.verify(a(2), 0, c), Err(RejectReason::BadCookie)); // other port
        assert_eq!(j.verify(SocketAddr::from(([10, 0, 0, 2], 1)), 0, c), Err(RejectReason::BadCookie));
    }

    #[test]
    fn any_flipped_bit_is_refused() {
        let j = jar();
        let c = j.issue(a(1), 0);
        for bit in 0..64 {
            assert_eq!(j.verify(a(1), 0, c ^ (1 << bit)), Err(RejectReason::BadCookie), "bit {bit}");
        }
    }

    #[test]
    fn another_key_issues_other_cookies() {
        assert_ne!(jar().issue(a(1), 0), CookieJar::with_key([7; 16]).issue(a(1), 0));
        assert_ne!(CookieJar::new().issue(a(1), 0), CookieJar::new().issue(a(1), 0));
    }
}
