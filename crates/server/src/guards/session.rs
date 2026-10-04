//! G-2 session guard
//! STOPS: forged source addresses — an off-path attacker who knows a player's
//!        address and sends datagrams "from" it, to act as that player, spend
//!        its rate budget, or fill its record with rejects so the detector
//!        flags the victim instead of the attacker
//! HOW:   a legal Join is answered with a 64-bit token (a random nonce and a
//!        MAC of the address, so a relay holding the key can pre-check it),
//!        sent only to the joining address. A datagram is the player's only if it comes
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
//! Does NOT stop an on-path attacker between client and relay (or client
//! and server, with no relay), who can read the token off the wire: that
//! leg is not encrypted. Behind a relay, the relay-to-origin link is sealed,
//! so the token is not readable there.

use std::net::SocketAddr;

use super::RejectReason;
use aegis_protocol::{mint_token, LinkKey, PlayerId, NO_TOKEN};

/// One admitted player: its id and the token it must present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub player_id: PlayerId,
    pub token: u64,
}

/// A fresh token for `client`: a nonce from the OS CSPRNG, MAC'd under
/// `key` ([`mint_token`]), so a relay holding the key can check it without a
/// table. Never [`NO_TOKEN`], which every client sends before it has one.
pub fn new_token(key: &LinkKey, client: SocketAddr) -> u64 {
    loop {
        let nonce = getrandom::u32().expect("aegis-server: OS random source unavailable");
        let t = mint_token(key, client, nonce);
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
    fn tokens_are_never_zero_do_not_repeat_and_check_for_their_address() {
        let key = aegis_protocol::LinkKey::new([9; 32]);
        let a: SocketAddr = "10.0.0.7:4000".parse().unwrap();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let t = new_token(&key, a);
            assert_ne!(t, NO_TOKEN);
            assert!(seen.insert(t), "repeated token {t:#x}");
            assert!(aegis_protocol::token_valid(&key, a, t));
        }
    }
}
