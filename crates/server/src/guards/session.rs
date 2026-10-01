//! G-2 session guard
//! STOPS: forged source addresses — an off-path attacker who knows a player's
//!        address and sends datagrams "from" it, to act as that player, spend
//!        its rate budget, or fill its record with rejects so the detector
//!        flags the victim instead of the attacker
//! HOW:   a legal Join is answered with a random 64-bit token, sent only to
//!        the joining address. A datagram is the player's only if it comes
//!        from that address AND carries that token. An off-path attacker
//!        never sees the token.
//! EDGE:  right address + right token -> the player; right address + any other
//!        token (0, a guess, the attacker's own valid token) -> BadToken.
//!        (Unknown address is the [`super::joined`] guard's call.)
//!
//! Stage guard: runs on the fixed 8-byte header, before decode and before the
//! rate budgets. A forgery is counted in [`crate::NetStats`], never on the
//! victim's record.
//!
//! Does NOT stop an on-path attacker, who can read the token off the wire:
//! that needs encryption, which Aegis does not do yet.

use super::RejectReason;
use aegis_protocol::{PlayerId, NO_TOKEN};

/// One admitted player: its id and the token it must present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub player_id: PlayerId,
    pub token: u64,
}

/// A fresh token from the OS CSPRNG. Never [`NO_TOKEN`], which every client
/// sends before it has one.
pub fn new_token() -> u64 {
    loop {
        let t = getrandom::u64().expect("aegis-server: OS random source unavailable");
        if t != NO_TOKEN {
            return t;
        }
    }
}

pub fn verify(session: &Session, token: u64) -> Result<PlayerId, RejectReason> {
    if token == session.token {
        Ok(session.player_id)
    } else {
        Err(RejectReason::BadToken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Session = Session { player_id: 3, token: 0x1234_5678_9ABC_DEF0 };

    #[test]
    fn right_token_is_the_player() {
        assert_eq!(verify(&S, S.token), Ok(3));
    }

    #[test]
    fn any_other_token_is_refused() {
        for t in [NO_TOKEN, S.token ^ 1, S.token.wrapping_add(1), u64::MAX] {
            assert_eq!(verify(&S, t), Err(RejectReason::BadToken), "token {t:#x}");
        }
    }

    #[test]
    fn tokens_are_never_zero_and_do_not_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let t = new_token();
            assert_ne!(t, NO_TOKEN);
            assert!(seen.insert(t), "repeated token {t:#x}");
        }
    }
}
