//! G2 packet guard
//! STOPS: malformed / truncated / hostile datagrams
//! HOW:   attempt decode; on failure drop the packet, never panic or trust it
//! EDGE:  valid bytes decode to a ClientMsg; garbage bytes -> Rejected, no panic;
//!        an Input with more than MAX_OLDER earlier frames -> Rejected
//!
//! Stage guard: runs at decode, before the pipeline sees anything.

use super::RejectReason;
use aegis_protocol::{decode, ClientMsg, MAX_OLDER};

pub fn decode_client(bytes: &[u8]) -> Result<ClientMsg, RejectReason> {
    match decode::<ClientMsg>(bytes) {
        Ok(ClientMsg::Input { older, .. }) if older.len() > MAX_OLDER => Err(RejectReason::MalformedPacket),
        Ok(m) => Ok(m),
        Err(_) => Err(RejectReason::MalformedPacket),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{encode, InputFrame, Vec2, PROTOCOL_VERSION};

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

    /// At the edge: two earlier frames is a legal Input, three is not.
    #[test]
    fn more_than_max_older_frames_is_malformed() {
        let f = |seq| InputFrame { seq, tick: 1, proof: 0, move_dir: Vec2::ZERO, aim: Vec2::ZERO, shoot: false };
        let with = |n: u32| encode(&ClientMsg::input_with(f(10), (1..=n).map(f).collect()));
        assert!(decode_client(&with(MAX_OLDER as u32)).is_ok());
        assert_eq!(decode_client(&with(MAX_OLDER as u32 + 1)), Err(RejectReason::MalformedPacket));
    }
}
