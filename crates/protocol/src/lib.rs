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

use std::hash::Hasher;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use siphasher::sip::SipHasher24;

/// Bumped whenever the wire format changes. Clients on a different version
/// must be rejected at `Join` (handled by the server crate).
/// v1: session token header on every client datagram.
/// v2: two-step join — a cookie challenge proves the client receives at its
///     source address before the server admits it.
pub const PROTOCOL_VERSION: u16 = 2;

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
        /// `None` on the first try; the server answers with a
        /// [`ServerMsg::Challenge`], whose cookie the client sends back here.
        cookie: Option<u64>,
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
    /// This client's view of the world: itself and every player it can see.
    /// One behind a wall is left out, so a wallhack has nothing to draw —
    /// killed at the source instead of detected. Same shape as the full
    /// world, so culling needed no version bump.
    Snapshot {
        tick: u32,
        players: Vec<PlayerState>,
    },
    /// Answer to a Join without a valid cookie: send the Join again with this
    /// cookie. Proves the client receives at its source address. Never larger
    /// than the smallest Join, so it cannot amplify.
    Challenge {
        cookie: u64,
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

/// Largest client datagram anyone reads. Every legal client message is far
/// smaller; anything that does not fit is not a client message.
pub const MAX_DATAGRAM: usize = 2048;

/// Bytes of MAC in front of every envelope.
pub const MAC_LEN: usize = 8;

/// Largest [`wrap`] header: MAC, tag, an IPv6 address, a port.
pub const ENVELOPE_MAX: usize = MAC_LEN + 1 + 16 + 2;

/// The secret a relay and its origin share. Whoever holds it can speak for
/// any client to the origin, so it never leaves those two machines.
pub type LinkKey = [u8; 16];

/// Which way an envelope travels. Part of what the MAC covers, so an
/// envelope captured going one way is refused going the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Relay -> origin: a client's datagram.
    Up = 1,
    /// Origin -> relay: a datagram for a client.
    Down = 2,
}

/// Why [`unwrap`] refused an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    /// Too short to carry a MAC.
    Short,
    /// The MAC does not match: not from the key holder, or altered, or sent
    /// the other way.
    BadMac,
    /// Authentic but not a well-formed header (an unknown tag, cut short).
    Malformed,
}

fn link_mac(key: &LinkKey, dir: Dir, body: &[u8]) -> u64 {
    let mut h = SipHasher24::new_with_key(key);
    h.write(&[dir as u8]);
    h.write(body);
    h.finish()
}

/// MAC domain byte for session tokens — distinct from both [`Dir`] values,
/// so a token MAC can never be confused with an envelope MAC.
const TOKEN_DOMAIN: u8 = 3;

fn token_mac(key: &LinkKey, client: SocketAddr, nonce: u32) -> u32 {
    let mut h = SipHasher24::new_with_key(key);
    h.write(&[TOKEN_DOMAIN]);
    match client.ip() {
        IpAddr::V4(ip) => h.write(&ip.octets()),
        IpAddr::V6(ip) => h.write(&ip.octets()),
    }
    h.write(&client.port().to_le_bytes());
    h.write(&nonce.to_le_bytes());
    h.finish() as u32
}

/// A session token anyone holding `key` can check without remembering it:
/// a random `nonce` (high 32 bits) and a 32-bit MAC of the client's address
/// and that nonce (low 32 bits). The origin issues it; the relay checks it
/// with [`token_valid`] and drops a datagram whose token was not issued for
/// its source address — before it crosses to the origin, with no table.
///
/// The origin still checks the whole 64-bit token against the live session:
/// the relay cannot tell a token whose session has ended, and it does not
/// need to.
pub fn mint_token(key: &LinkKey, client: SocketAddr, nonce: u32) -> u64 {
    (u64::from(nonce) << 32) | u64::from(token_mac(key, client, nonce))
}

/// Was `token` minted under `key` for `client`? A random guess passes with
/// probability 2^-32 — a filter's error rate, not a lock's: the origin's
/// exact match is the lock.
pub fn token_valid(key: &LinkKey, client: SocketAddr, token: u64) -> bool {
    token as u32 == token_mac(key, client, (token >> 32) as u32)
}

/// Relay <-> origin only — never seen by a client, so not part of
/// `PROTOCOL_VERSION`. The address of the client a datagram came from (going
/// up) or is for (coming down), then the datagram untouched. Lets the origin
/// run every guard against the client's real address while only ever talking
/// to the relay.
///
/// Layout: MAC (8 bytes LE), tag `4`/`6`, the IP's octets, the port LE, the
/// payload. The MAC is SipHash-2-4 under `key` of `dir` and everything after
/// it: without the key, no one who forges the relay's source address can
/// claim to be a client, and the origin cannot be made to address a reply.
///
/// Not covered: an on-path attacker can replay a captured envelope the same
/// way. Up, that is a client datagram replayed — what the session token and
/// replay guard already judge; down, a stale snapshot resent.
pub fn wrap(key: &LinkKey, dir: Dir, client: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_MAX + payload.len());
    out.extend([0u8; MAC_LEN]);
    match client.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend(ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend(ip.octets());
        }
    }
    out.extend(client.port().to_le_bytes());
    out.extend(payload);
    let mac = link_mac(key, dir, &out[MAC_LEN..]);
    out[..MAC_LEN].copy_from_slice(&mac.to_le_bytes());
    out
}

/// The inverse of [`wrap`]: the MAC is checked before a byte of the header
/// is read.
pub fn unwrap<'a>(key: &LinkKey, dir: Dir, bytes: &'a [u8]) -> Result<(SocketAddr, &'a [u8]), EnvelopeError> {
    let (mac, body) = bytes.split_first_chunk::<MAC_LEN>().ok_or(EnvelopeError::Short)?;
    if u64::from_le_bytes(*mac) != link_mac(key, dir, body) {
        return Err(EnvelopeError::BadMac);
    }
    parse_header(body).ok_or(EnvelopeError::Malformed)
}

fn parse_header(bytes: &[u8]) -> Option<(SocketAddr, &[u8])> {
    let (&tag, rest) = bytes.split_first()?;
    let (ip, rest): (IpAddr, &[u8]) = match tag {
        4 => {
            let (o, rest) = rest.split_first_chunk::<4>()?;
            (Ipv4Addr::from(*o).into(), rest)
        }
        6 => {
            let (o, rest) = rest.split_first_chunk::<16>()?;
            (Ipv6Addr::from(*o).into(), rest)
        }
        _ => return None,
    };
    let (port, payload) = rest.split_first_chunk::<2>()?;
    Some((SocketAddr::new(ip, u16::from_le_bytes(*port)), payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let m = ClientMsg::Join { name: "riw".into(), protocol: PROTOCOL_VERSION, cookie: None };
        let bytes = frame(0xDEAD_BEEF_0000_0001, &m);
        let (token, body) = split_frame(&bytes).unwrap();
        assert_eq!(token, 0xDEAD_BEEF_0000_0001);
        assert_eq!(decode::<ClientMsg>(body).unwrap(), m);
    }

    /// No amplification by construction: the challenge (the only thing an
    /// unproven address ever gets) is no larger than the smallest possible
    /// Join — empty name, no cookie — that could have triggered it.
    #[test]
    fn challenge_is_never_larger_than_a_join() {
        let smallest_join = frame(NO_TOKEN, &ClientMsg::Join { name: String::new(), protocol: 0, cookie: None });
        let challenge = encode(&ServerMsg::Challenge { cookie: u64::MAX });
        assert!(challenge.len() <= smallest_join.len(), "{} > {}", challenge.len(), smallest_join.len());
        // A forged re-Join in an admitted player's name is answered with
        // Joined, to that player — also no larger.
        let joined = encode(&ServerMsg::Joined { player_id: 255, token: u64::MAX, tick: u32::MAX });
        assert!(joined.len() <= smallest_join.len(), "{} > {}", joined.len(), smallest_join.len());
    }

    /// The edge: exactly a token and nothing else splits (into an empty body
    /// the decoder will refuse); one byte less does not split at all.
    #[test]
    fn split_frame_at_the_header_edge() {
        assert_eq!(split_frame(&[7, 0, 0, 0, 0, 0, 0, 0]), Some((7, &[][..])));
        assert_eq!(split_frame(&[7, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(split_frame(&[]), None);
    }

    const KEY: LinkKey = [7; 16];

    #[test]
    fn envelope_roundtrip_v4_v6_and_empty() {
        for client in ["10.0.0.7:40001", "[2001:db8::1]:65535", "127.0.0.1:0"] {
            let client: SocketAddr = client.parse().unwrap();
            for payload in [&b""[..], &[1, 2, 3][..], &[0u8; 2048][..]] {
                for dir in [Dir::Up, Dir::Down] {
                    let w = wrap(&KEY, dir, client, payload);
                    assert!(w.len() <= ENVELOPE_MAX + payload.len());
                    assert_eq!(unwrap(&KEY, dir, &w), Ok((client, payload)));
                }
            }
        }
    }

    /// The MAC at its edges: the wrong key, the other direction, and one
    /// flipped bit anywhere — MAC, header or payload — are all refused.
    #[test]
    fn envelope_refuses_a_forgery() {
        let client: SocketAddr = "10.0.0.7:9".parse().unwrap();
        let w = wrap(&KEY, Dir::Up, client, b"input");
        assert_eq!(unwrap(&[8; 16], Dir::Up, &w), Err(EnvelopeError::BadMac));
        assert_eq!(unwrap(&KEY, Dir::Down, &w), Err(EnvelopeError::BadMac));
        for i in 0..w.len() {
            let mut bad = w.clone();
            bad[i] ^= 1;
            assert_eq!(unwrap(&KEY, Dir::Up, &bad), Err(EnvelopeError::BadMac), "bit flip at byte {i} accepted");
        }
        // Redirecting a reply: same payload, another client — needs the key.
        let mut moved = w.clone();
        moved[MAC_LEN + 1..MAC_LEN + 5].copy_from_slice(&[10, 0, 0, 8]);
        assert_eq!(unwrap(&KEY, Dir::Up, &moved), Err(EnvelopeError::BadMac));
    }

    /// A minted token checks for its own address only: another port, another
    /// IP, another key, one flipped bit, or a token from a key-less server
    /// (random) all fail.
    #[test]
    fn token_is_valid_only_for_its_address_and_key() {
        let a: SocketAddr = "10.0.0.7:4000".parse().unwrap();
        for nonce in [0, 1, 0xDEAD_BEEF, u32::MAX] {
            let t = mint_token(&KEY, a, nonce);
            assert!(token_valid(&KEY, a, t));
            assert!(!token_valid(&KEY, "10.0.0.7:4001".parse().unwrap(), t));
            assert!(!token_valid(&KEY, "10.0.0.8:4000".parse().unwrap(), t));
            assert!(!token_valid(&[8; 16], a, t));
            for bit in 0..64 {
                assert!(!token_valid(&KEY, a, t ^ (1 << bit)), "nonce {nonce:#x}, bit {bit} flipped still valid");
            }
        }
    }

    /// Edge: shorter than a MAC is `Short`; authentic but cut or unknown
    /// headers are `Malformed`, never read past.
    #[test]
    fn envelope_refuses_a_cut_header() {
        let w4 = wrap(&KEY, Dir::Up, "10.0.0.7:9".parse().unwrap(), &[]);
        assert_eq!(w4.len(), MAC_LEN + 7);
        assert!(unwrap(&KEY, Dir::Up, &w4).is_ok());
        let w6 = wrap(&KEY, Dir::Up, "[::1]:9".parse().unwrap(), &[]);
        assert_eq!(w6.len(), ENVELOPE_MAX);
        assert_eq!(unwrap(&KEY, Dir::Up, &w4[..MAC_LEN - 1]), Err(EnvelopeError::Short));
        assert_eq!(unwrap(&KEY, Dir::Up, &[]), Err(EnvelopeError::Short));
        // A correctly MAC'd body that is not a header.
        let sign = |body: &[u8]| {
            let mut out = link_mac(&KEY, Dir::Up, body).to_le_bytes().to_vec();
            out.extend(body);
            out
        };
        assert_eq!(unwrap(&KEY, Dir::Up, &sign(&w4[MAC_LEN..w4.len() - 1])), Err(EnvelopeError::Malformed));
        assert_eq!(unwrap(&KEY, Dir::Up, &sign(&[5, 0, 0, 0, 0, 0, 0])), Err(EnvelopeError::Malformed));
        assert_eq!(unwrap(&KEY, Dir::Up, &sign(&[])), Err(EnvelopeError::Malformed));
    }

    #[test]
    fn clientmsg_join_roundtrip() {
        let m = ClientMsg::Join { name: "riw".into(), protocol: PROTOCOL_VERSION, cookie: None };
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
