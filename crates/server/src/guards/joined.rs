//! G0 joined guard
//! STOPS: inputs from a player the server never admitted (join rejected, or
//!        never sent) — e.g. a bad-version client that skips the handshake
//!        result and starts playing anyway
//! HOW:   the input's player must be in the set of players whose Join passed
//! EDGE:  admitted player accepted; unknown player rejected; empty set rejects
//!        everyone
//!
//! Stage guard: runs after decode, before the per-input pipeline. Rejecting
//! here (rather than dropping silently) keeps the attempt visible to the
//! detector.

use super::RejectReason;
use aegis_protocol::PlayerId;
use std::collections::BTreeSet;

pub fn check_input(joined: &BTreeSet<PlayerId>, player: PlayerId) -> Result<(), RejectReason> {
    if joined.contains(&player) {
        Ok(())
    } else {
        Err(RejectReason::NotJoined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_player_ok() {
        let joined = BTreeSet::from([1, 2]);
        assert!(check_input(&joined, 2).is_ok());
    }

    #[test]
    fn unknown_player_rejected() {
        let joined = BTreeSet::from([1, 2]);
        assert_eq!(check_input(&joined, 3), Err(RejectReason::NotJoined));
    }

    #[test]
    fn empty_set_rejects_everyone() {
        assert_eq!(check_input(&BTreeSet::new(), 1), Err(RejectReason::NotJoined));
    }
}
