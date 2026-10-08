//! G-9 tick-proof guard
//! STOPS: a client claiming it chose an input on a newer snapshot than it
//!        had — the way a cheater on a slow link would make an instant shot
//!        read as a human reaction once reaction is timed from the snapshot
//!        the client acted on (`crate::reaction`)
//! HOW:   every snapshot carries proof = SipHash-2-4(secret key, player,
//!        session token, tick), truncated to 32 bits; every input echoes
//!        the tick it was chosen on and that tick's proof. Only the server
//!        can compute one, and it hands player P the proof of tick T only by
//!        sending P snapshot T. The token binds it to the session: whoever
//!        held P's id before (even the same person, rejoined) holds proofs
//!        that fail. Stateless: nothing is stored per tick.
//! EDGE:  the proof of the tick claimed -> accepted | any other tick's, or
//!        another player's, another session's, or one flipped bit ->
//!        refused. How old a proven tick may be is [`super::stale_tick`]'s
//!        call.
//!
//! Stage guard: runs on every authenticated input, before the pipeline. A
//! guess passes with probability 2^-32 per input, at one input per tick.

use super::RejectReason;
use aegis_protocol::PlayerId;
use siphasher::sip::SipHasher24;
use std::hash::Hasher;

pub struct TickProof {
    key: [u8; 16],
}

impl TickProof {
    /// A fresh key from the OS CSPRNG, per server process.
    pub fn new() -> Self {
        let mut key = [0u8; 16];
        getrandom::fill(&mut key).expect("aegis-server: OS random source unavailable");
        Self { key }
    }

    #[cfg(test)]
    fn with_key(key: [u8; 16]) -> Self {
        Self { key }
    }

    /// The proof that goes out in `player`'s snapshot of `tick`.
    pub fn issue(&self, player: PlayerId, token: u64, tick: u32) -> u32 {
        let mut h = SipHasher24::new_with_key(&self.key);
        h.write(&[player]);
        h.write(&token.to_le_bytes());
        h.write(&tick.to_le_bytes());
        h.finish() as u32
    }

    pub fn verify(&self, player: PlayerId, token: u64, tick: u32, proof: u32) -> Result<(), RejectReason> {
        if self.issue(player, token, tick) == proof {
            Ok(())
        } else {
            Err(RejectReason::BadTickProof)
        }
    }
}

impl Default for TickProof {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: u64 = 0xAAAA_0001;

    fn tp() -> TickProof {
        TickProof::with_key(*b"0123456789abcdef")
    }

    #[test]
    fn its_own_tick_passes_any_other_does_not() {
        let t = tp();
        let p = t.issue(1, TOKEN, 100);
        assert_eq!(t.verify(1, TOKEN, 100, p), Ok(()));
        assert_eq!(t.verify(1, TOKEN, 101, p), Err(RejectReason::BadTickProof), "claimed one tick newer");
        assert_eq!(t.verify(1, TOKEN, 99, p), Err(RejectReason::BadTickProof));
    }

    #[test]
    fn bound_to_the_player_it_was_sent_to() {
        let t = tp();
        assert_eq!(t.verify(2, TOKEN, 100, t.issue(1, TOKEN, 100)), Err(RejectReason::BadTickProof));
    }

    /// The same id in a later session: the earlier session's proofs fail.
    #[test]
    fn bound_to_the_session_it_was_sent_in() {
        let t = tp();
        assert_eq!(t.verify(1, TOKEN + 1, 100, t.issue(1, TOKEN, 100)), Err(RejectReason::BadTickProof));
    }

    #[test]
    fn any_flipped_bit_is_refused() {
        let t = tp();
        let p = t.issue(7, TOKEN, 5);
        for bit in 0..32 {
            assert_eq!(t.verify(7, TOKEN, 5, p ^ (1 << bit)), Err(RejectReason::BadTickProof), "bit {bit}");
        }
    }

    #[test]
    fn another_key_issues_other_proofs() {
        assert_ne!(tp().issue(1, TOKEN, 1), TickProof::with_key([7; 16]).issue(1, TOKEN, 1));
        assert_ne!(TickProof::new().issue(1, TOKEN, 1), TickProof::new().issue(1, TOKEN, 1));
    }
}
