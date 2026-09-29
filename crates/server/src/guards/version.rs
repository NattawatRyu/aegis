//! G1 version guard
//! STOPS: clients on a mismatched/spoofed protocol version
//! HOW:   compare the version declared at Join against PROTOCOL_VERSION
//! EDGE:  exact match accepted; any other value (older, newer, garbage) rejected
//!
//! Stage guard: runs at `Join`, not in the per-input pipeline.

use super::RejectReason;
use aegis_protocol::PROTOCOL_VERSION;

pub fn check_join(client_protocol: u16) -> Result<(), RejectReason> {
    if client_protocol == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(RejectReason::BadVersion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_version_ok() {
        assert!(check_join(PROTOCOL_VERSION).is_ok());
    }

    #[test]
    fn mismatched_version_rejected() {
        assert_eq!(check_join(PROTOCOL_VERSION + 1), Err(RejectReason::BadVersion));
        assert_eq!(check_join(9999), Err(RejectReason::BadVersion));
    }
}
