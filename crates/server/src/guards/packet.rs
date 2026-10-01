//! G2 packet guard
//! STOPS: malformed / truncated / hostile datagrams
//! HOW:   attempt decode; on failure drop the packet, never panic or trust it
//! EDGE:  valid bytes decode to a ClientMsg; garbage bytes -> Rejected, no panic
//!
//! Stage guard: runs at decode, before the pipeline sees anything.

use super::RejectReason;
use aegis_protocol::{decode, ClientMsg};

pub fn decode_client(bytes: &[u8]) -> Result<ClientMsg, RejectReason> {
    decode::<ClientMsg>(bytes).map_err(|_| RejectReason::MalformedPacket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{encode, PROTOCOL_VERSION};

    #[test]
    fn valid_bytes_decode() {
        let bytes = encode(&ClientMsg::Join { name: "riw".into(), protocol: PROTOCOL_VERSION, cookie: None });
        assert!(decode_client(&bytes).is_ok());
    }

    #[test]
    fn garbage_bytes_rejected_not_panic() {
        assert_eq!(decode_client(&[0xFF, 0xFF, 0xFF, 0xFF]), Err(RejectReason::MalformedPacket));
    }

    #[test]
    fn empty_bytes_rejected() {
        assert_eq!(decode_client(&[]), Err(RejectReason::MalformedPacket));
    }
}
