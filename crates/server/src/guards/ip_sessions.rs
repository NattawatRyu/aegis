//! G-3 ip-sessions guard
//! STOPS: one machine taking every player slot — a legal Join from each of
//!        hundreds of source ports, each of which the server would otherwise
//!        admit as a new player, until the server is full for everyone
//! HOW:   at most `MAX_PER_IP` live sessions per source IP. A Join past that
//!        is refused (and counted) until one of that IP's sessions ends.
//! EDGE:  the `MAX_PER_IP`-th session from an IP is admitted; the next is
//!        refused; another IP is unaffected; a freed slot admits again.
//!
//! Stage guard: runs at admission, after the version check. Sessions that
//! never send anything are what the idle timeout ([`crate::server::IDLE_TICKS`])
//! clears, so a flooder's slots come back too.

use super::session::Session;
use super::RejectReason;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

/// Room for a household or a small LAN party behind one NAT.
pub const MAX_PER_IP: usize = 4;

pub fn check_join(sessions: &BTreeMap<SocketAddr, Session>, ip: IpAddr) -> Result<(), RejectReason> {
    if sessions.keys().filter(|a| a.ip() == ip).count() < MAX_PER_IP {
        Ok(())
    } else {
        Err(RejectReason::IpSessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(ip: [u8; 4], n: u16) -> BTreeMap<SocketAddr, Session> {
        (0..n).map(|p| (SocketAddr::from((ip, 5000 + p)), Session { player_id: p as u8 + 1, token: 1 })).collect()
    }

    #[test]
    fn cap_admits_up_to_the_limit_and_refuses_the_next() {
        let ip = IpAddr::from([10, 0, 0, 1]);
        assert_eq!(check_join(&with([10, 0, 0, 1], MAX_PER_IP as u16 - 1), ip), Ok(()));
        assert_eq!(check_join(&with([10, 0, 0, 1], MAX_PER_IP as u16), ip), Err(RejectReason::IpSessions));
    }

    #[test]
    fn other_ips_do_not_count() {
        let full = with([10, 0, 0, 1], MAX_PER_IP as u16);
        assert_eq!(check_join(&full, IpAddr::from([10, 0, 0, 2])), Ok(()));
    }
}
