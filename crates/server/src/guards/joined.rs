//! G0 joined guard
//! STOPS: anything but a Join from a source the server never admitted (join
//!        rejected, or never sent) — e.g. a bad-version client that ignores
//!        the refusal and starts playing anyway
//! HOW:   a session exists only for a source address whose Join passed; a
//!        non-Join datagram must come from one of those addresses (and then
//!        pass the [`super::session`] token check)
//! EDGE:  admitted address -> its session; unknown address rejected, even on a
//!        port of an admitted IP; empty map rejects everyone
//!
//! Stage guard: runs before decode. An unknown source has no player id, so its
//! rejections are counted in [`crate::NetStats`], not in per-player telemetry —
//! issuing ids to unknown sources would let spoofed addresses exhaust the 255
//! of them.

use super::session::Session;
use super::RejectReason;
use std::collections::BTreeMap;
use std::net::SocketAddr;

pub fn check_source(sessions: &BTreeMap<SocketAddr, Session>, from: SocketAddr) -> Result<Session, RejectReason> {
    sessions.get(&from).copied().ok_or(RejectReason::NotJoined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn s(player_id: u8) -> Session {
        Session { player_id, token: 100 + player_id as u64 }
    }

    #[test]
    fn admitted_source_gets_its_session() {
        let m = BTreeMap::from([(addr("10.0.0.1:5000"), s(1)), (addr("10.0.0.2:5000"), s(2))]);
        assert_eq!(check_source(&m, addr("10.0.0.2:5000")), Ok(s(2)));
    }

    #[test]
    fn unknown_source_rejected_even_on_an_admitted_ip() {
        let m = BTreeMap::from([(addr("10.0.0.1:5000"), s(1))]);
        assert_eq!(check_source(&m, addr("10.0.0.1:5001")), Err(RejectReason::NotJoined));
        assert_eq!(check_source(&m, addr("10.0.0.3:5000")), Err(RejectReason::NotJoined));
    }

    #[test]
    fn empty_map_rejects_everyone() {
        assert_eq!(check_source(&BTreeMap::new(), addr("10.0.0.1:5000")), Err(RejectReason::NotJoined));
    }
}
