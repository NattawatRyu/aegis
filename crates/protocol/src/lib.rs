//! Aegis wire protocol v1 — the contract every crate depends on.
//!
//! Core rule: **the server never accepts a position from the client.**
//! Clients send *intent* (`move_dir`, `aim`, `shoot`); the server computes
//! the authoritative state. This makes teleport / speedhack impossible by
//! construction rather than by after-the-fact detection.
//!
//! Every client datagram is a [`frame`]: an 8-byte session token, then the
//! encoded [`ClientMsg`]. The token is 0 until the server issues one in
//! [`ServerMsg::Joined`]. It sits in a fixed header so the server can check
//! it before spending a decode.

use serde::{Deserialize, Serialize};

/// Bumped whenever the wire format changes. Clients on a different version
/// must be rejected at `Join` (handled by the server crate).
/// v1: session token header on every client datagram.
pub const PROTOCOL_VERSION: u16 = 1;

/// Bytes of session token in front of every client datagram.
pub const TOKEN_LEN: usize = 8;

/// The token a client sends before it has one (its Join, and nothing else).
pub const NO_TOKEN: u64 = 0;

/// Fixed simulation rate. Inputs arriving faster than this buy the sender
/// nothing — the server folds at most one input per player per tick.
pub const TICK_HZ: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn len(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// Clamp to unit length. The server calls this on every incoming
    /// `move_dir` so a client cannot smuggle a speed multiplier inside the
    /// vector's magnitude (e.g. sending `(10, 0)` to move 10x per tick).
    pub fn clamped_unit(self) -> Vec2 {
        let l = self.len();
        if l > 1.0 {
            Vec2::new(self.x / l, self.y / l)
        } else {
            self
        }
    }
}

pub type PlayerId = u8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub id: PlayerId,
    pub pos: Vec2,
    pub health: u8,
    pub alive: bool,
}

/// Client -> server. Note there is no variant carrying a position: the only
/// spatial thing a client may assert is a *direction*, never a location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Join {
        name: String,
        /// Client's protocol version. The server's G1 version guard rejects a
        /// join whose value differs from `PROTOCOL_VERSION`.
        protocol: u16,
    },
    /// `seq`: client's monotonic counter, used for dup/replay detection.
    /// `tick`: the tick the client believes it is acting on (lag context).
    Input {
        seq: u32,
        tick: u32,
        move_dir: Vec2,
        aim: Vec2,
        shoot: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// `token` goes in front of every datagram this client sends from now on.
    /// It is only good from the address this reply was sent to.
    Joined {
        player_id: PlayerId,
        token: u64,
        tick: u32,
    },
    /// Full snapshot for now. Pillar D replaces this with a per-player culled
    /// view so a client never receives enemies it cannot see (kills wallhack
    /// at the source instead of trying to detect it).
    Snapshot {
        tick: u32,
        players: Vec<PlayerState>,
    },
    Event {
        tick: u32,
        kind: EventKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    Hit {
        shooter: PlayerId,
        target: PlayerId,
        damage: u8,
    },
    Death {
        player: PlayerId,
        by: PlayerId,
    },
    Join {
        player: PlayerId,
    },
    Leave {
        player: PlayerId,
    },
}

/// Serialize a message to bytes for the wire.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    bincode::serialize(msg).expect("aegis-protocol: serialize failed")
}

/// Deserialize a message from wire bytes. Returns `Err` on malformed input —
/// the server must drop such packets, never trust or panic on them.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, bincode::Error> {
    bincode::deserialize(bytes)
}

/// A client datagram: `token` (little-endian) then `msg`.
pub fn frame(token: u64, msg: &ClientMsg) -> Vec<u8> {
    let mut out = token.to_le_bytes().to_vec();
    out.extend(encode(msg));
    out
}

/// Split a client datagram into its token and body. `None` if it is too short
/// to carry a token at all.
pub fn split_frame(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let (head, body) = bytes.split_first_chunk::<TOKEN_LEN>()?;
    Some((u64::from_le_bytes(*head), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let m = ClientMsg::Join { name: "riw".into(), protocol: PROTOCOL_VERSION };
        let bytes = frame(0xDEAD_BEEF_0000_0001, &m);
        let (token, body) = split_frame(&bytes).unwrap();
        assert_eq!(token, 0xDEAD_BEEF_0000_0001);
        assert_eq!(decode::<ClientMsg>(body).unwrap(), m);
    }

    /// The edge: exactly a token and nothing else splits (into an empty body
    /// the decoder will refuse); one byte less does not split at all.
    #[test]
    fn split_frame_at_the_header_edge() {
        assert_eq!(split_frame(&[7, 0, 0, 0, 0, 0, 0, 0]), Some((7, &[][..])));
        assert_eq!(split_frame(&[7, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(split_frame(&[]), None);
    }

    #[test]
    fn clientmsg_join_roundtrip() {
        let m = ClientMsg::Join { name: "riw".into(), protocol: PROTOCOL_VERSION };
        let back: ClientMsg = decode(&encode(&m)).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn clientmsg_input_roundtrip() {
        let m = ClientMsg::Input {
            seq: 7,
            tick: 100,
            move_dir: Vec2::new(0.3, -0.4),
            aim: Vec2::new(1.0, 0.0),
            shoot: true,
        };
        let back: ClientMsg = decode(&encode(&m)).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn servermsg_snapshot_roundtrip() {
        let m = ServerMsg::Snapshot {
            tick: 42,
            players: vec![
                PlayerState { id: 1, pos: Vec2::new(1.0, 2.0), health: 100, alive: true },
                PlayerState { id: 2, pos: Vec2::ZERO, health: 0, alive: false },
            ],
        };
        let back: ServerMsg = decode(&encode(&m)).unwrap();
        assert_eq!(m, back);
    }

    // Guard asserted at its edge: a client claiming a length-10 move vector
    // to move 10x faster must be clamped to unit length. This is the
    // speedhack-via-magnitude defense the server relies on.
    #[test]
    fn move_vector_cannot_smuggle_speed() {
        let cheat = Vec2::new(10.0, 0.0);
        assert!((cheat.clamped_unit().len() - 1.0).abs() < 1e-6);

        // an exactly-unit vector is left untouched
        let unit = Vec2::new(1.0, 0.0);
        assert_eq!(unit.clamped_unit(), unit);

        // honest sub-unit input passes through unchanged
        let honest = Vec2::new(0.3, 0.4);
        assert_eq!(honest.clamped_unit(), honest);
    }

    #[test]
    fn decode_garbage_is_err_not_panic() {
        let bad = [0xFFu8, 0xFF, 0xFF, 0xFF];
        let r: Result<ClientMsg, _> = decode(&bad);
        assert!(r.is_err());
    }
}
