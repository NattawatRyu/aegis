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

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::XChaCha20Poly1305;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
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
    Input { seq: u32, tick: u32, move_dir: Vec2, aim: Vec2, shoot: bool },
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
    Hit { shooter: PlayerId, target: PlayerId, damage: u8 },
    Death { player: PlayerId, by: PlayerId },
    Join { player: PlayerId },
    Leave { player: PlayerId },
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

/// Bytes of random nonce in front of every envelope (XChaCha20-Poly1305).
pub const NONCE_LEN: usize = 24;

/// Bytes of authentication tag at the end of every envelope.
pub const TAG_LEN: usize = 16;

/// Largest [`wrap`] overhead: nonce, tag, and the sealed header — address
/// tag, an IPv6 address, a port.
pub const ENVELOPE_MAX: usize = NONCE_LEN + TAG_LEN + 1 + 16 + 2;

/// The secret a relay and its origin share. Whoever holds it can speak for
/// any client to the origin and read the link, so it never leaves those two
/// machines.
///
/// Made from one 32-byte secret, from which HKDF-SHA256 derives two
/// independent subkeys — no key is used by two primitives:
///   - `mac` (16 bytes, SipHash-2-4): session tokens and edge cookies,
///     values the relay recomputes to check;
///   - `seal` (32 bytes, XChaCha20-Poly1305): the link envelopes.
#[derive(Clone, Copy)]
pub struct LinkKey {
    mac: [u8; 16],
    seal: [u8; 32],
}

impl LinkKey {
    /// Derive the subkeys from the shared `secret`.
    pub fn new(secret: [u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(None, &secret);
        let (mut mac, mut seal) = ([0u8; 16], [0u8; 32]);
        hk.expand(b"aegis link v1 mac", &mut mac).expect("16 bytes is a valid HKDF-SHA256 length");
        hk.expand(b"aegis link v1 seal", &mut seal).expect("32 bytes is a valid HKDF-SHA256 length");
        Self { mac, seal }
    }

    /// A fresh secret from the OS CSPRNG, as a deployment would provision
    /// one (and hand the same 32 bytes to relay and origin).
    pub fn random() -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("aegis-protocol: OS random source unavailable");
        Self::new(secret)
    }

    fn sip(&self) -> SipHasher24 {
        SipHasher24::new_with_key(&self.mac)
    }
}

/// Never prints key material.
impl std::fmt::Debug for LinkKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkKey(..)")
    }
}

/// Which way an envelope travels. Bound into every envelope's tag (as
/// associated data), so an envelope captured going one way is refused going
/// the other.
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
    /// Too short to carry a nonce and a tag.
    Short,
    /// The tag does not verify: not sealed under the key, or altered, or
    /// sent the other way. Nothing of it was decrypted for use.
    BadMac,
    /// Authentic but not a well-formed header (an unknown tag, cut short).
    Malformed,
}

/// MAC domain byte for session tokens. The `mac` subkey is used for nothing
/// but these SipHash domains.
const TOKEN_DOMAIN: u8 = 3;

fn token_mac(key: &LinkKey, client: SocketAddr, nonce: u32) -> u32 {
    let mut h = key.sip();
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

/// MAC domain byte for join cookies a relay issues — distinct from
/// `TOKEN_DOMAIN`, so a token can never stand in for a cookie or back.
const COOKIE_DOMAIN: u8 = 4;

/// The join cookie a relay holding `key` issues to `client` in time bucket
/// `bucket`. Only a client that receives at `client` learns it; the relay
/// checks it by recomputing, with no table.
pub fn edge_cookie(key: &LinkKey, client: SocketAddr, bucket: u32) -> u64 {
    let mut h = key.sip();
    h.write(&[COOKIE_DOMAIN]);
    match client.ip() {
        IpAddr::V4(ip) => h.write(&ip.octets()),
        IpAddr::V6(ip) => h.write(&ip.octets()),
    }
    h.write(&client.port().to_le_bytes());
    h.write(&bucket.to_le_bytes());
    h.finish()
}

/// Relay <-> origin only — never seen by a client, so not part of
/// `PROTOCOL_VERSION`. The address of the client a datagram came from (going
/// up) or is for (coming down), then the datagram untouched. Lets the origin
/// run every guard against the client's real address while only ever talking
/// to the relay.
///
/// Layout: a random 24-byte nonce, then — sealed with XChaCha20-Poly1305
/// under the key's `seal` subkey, with `dir` as associated data — the
/// address tag `4`/`6`, the IP's octets, the port LE and the payload, then
/// the 16-byte tag. Without the key no one who forges the relay's source
/// address can claim to be a client, the origin cannot be made to address a
/// reply, and an on-path observer of the link learns neither the client
/// addresses, nor their session tokens, nor what they sent — only sizes and
/// timing. The nonce is random per envelope (192 bits: no counter to keep,
/// no realistic collision), so the same datagram sealed twice looks
/// different both times.
///
/// Not covered: an on-path attacker can replay a captured envelope the same
/// way. Up, that is a client datagram replayed — what the session token and
/// replay guard already judge; down, a stale snapshot resent.
pub fn wrap(key: &LinkKey, dir: Dir, client: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_MAX + payload.len());
    out.extend([0u8; NONCE_LEN]);
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
    seal(key, dir, out)
}

/// Seal `out` in place: its first `NONCE_LEN` bytes are overwritten with a
/// fresh random nonce, the rest is encrypted, and the tag is appended.
fn seal(key: &LinkKey, dir: Dir, mut out: Vec<u8>) -> Vec<u8> {
    let (nonce, body) = out.split_at_mut(NONCE_LEN);
    getrandom::fill(nonce).expect("aegis-protocol: OS random source unavailable");
    let tag = XChaCha20Poly1305::new(&key.seal.into())
        .encrypt_in_place_detached((&*nonce).into(), &[dir as u8], body)
        .expect("aegis-protocol: envelope too large to seal");
    out.extend(tag);
    out
}

/// The inverse of [`wrap`], in place: the tag is verified before anything is
/// decrypted for use, and `bytes` holds the plaintext afterwards (on an
/// error its contents are unspecified). Returns the client's address and the
/// payload, borrowed from `bytes`.
pub fn unwrap<'a>(key: &LinkKey, dir: Dir, bytes: &'a mut [u8]) -> Result<(SocketAddr, &'a [u8]), EnvelopeError> {
    if bytes.len() < NONCE_LEN + TAG_LEN {
        return Err(EnvelopeError::Short);
    }
    let (nonce, rest) = bytes.split_at_mut(NONCE_LEN);
    let (body, tag) = rest.split_at_mut(rest.len() - TAG_LEN);
    XChaCha20Poly1305::new(&key.seal.into())
        .decrypt_in_place_detached((&*nonce).into(), &[dir as u8], body, (&*tag).into())
        .map_err(|_| EnvelopeError::BadMac)?;
    let body: &'a [u8] = body;
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

    static KEY: std::sync::LazyLock<LinkKey> = std::sync::LazyLock::new(|| LinkKey::new([7; 32]));

    fn other_key() -> LinkKey {
        LinkKey::new([8; 32])
    }

    #[test]
    fn envelope_roundtrip_v4_v6_and_empty() {
        for client in ["10.0.0.7:40001", "[2001:db8::1]:65535", "127.0.0.1:0"] {
            let client: SocketAddr = client.parse().unwrap();
            for payload in [&b""[..], &[1, 2, 3][..], &[0u8; 2048][..]] {
                for dir in [Dir::Up, Dir::Down] {
                    let mut w = wrap(&KEY, dir, client, payload);
                    assert!(w.len() <= ENVELOPE_MAX + payload.len());
                    assert_eq!(unwrap(&KEY, dir, &mut w), Ok((client, payload)));
                }
            }
        }
    }

    /// The tag at its edges: the wrong key, the other direction, and one
    /// flipped bit anywhere — nonce, sealed header, payload or tag — are all
    /// refused.
    #[test]
    fn envelope_refuses_a_forgery() {
        let client: SocketAddr = "10.0.0.7:9".parse().unwrap();
        let w = wrap(&KEY, Dir::Up, client, b"input");
        assert_eq!(unwrap(&other_key(), Dir::Up, &mut w.clone()), Err(EnvelopeError::BadMac));
        assert_eq!(unwrap(&KEY, Dir::Down, &mut w.clone()), Err(EnvelopeError::BadMac));
        for i in 0..w.len() {
            let mut bad = w.clone();
            bad[i] ^= 1;
            assert_eq!(unwrap(&KEY, Dir::Up, &mut bad), Err(EnvelopeError::BadMac), "bit flip at byte {i} accepted");
        }
    }

    /// What an on-path observer of the link sees: none of the client's
    /// address, its token or its message appears in the envelope, and the
    /// same datagram sealed twice is not the same bytes (nothing to match).
    #[test]
    fn envelope_hides_address_token_and_payload() {
        let client: SocketAddr = "10.11.12.13:47806".parse().unwrap();
        let token = 0xA1B2_C3D4_E5F6_0718u64;
        let payload = frame(
            token,
            &ClientMsg::Join {
                name: "secret-name".into(),
                protocol: PROTOCOL_VERSION,
                cookie: Some(0x0123_4567_89AB_CDEF),
            },
        );
        let w = wrap(&KEY, Dir::Up, client, &payload);
        let contains = |needle: &[u8]| w.windows(needle.len()).any(|win| win == needle);
        assert!(!contains(&[10, 11, 12, 13]), "client IP in the clear");
        assert!(!contains(&token.to_le_bytes()), "session token in the clear");
        assert!(!contains(b"secret-name"), "payload in the clear");
        assert!(!contains(&0x0123_4567_89AB_CDEFu64.to_le_bytes()), "cookie in the clear");
        assert_ne!(w, wrap(&KEY, Dir::Up, client, &payload), "two seals of one datagram match");
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
            assert!(!token_valid(&other_key(), a, t));
            for bit in 0..64 {
                assert!(!token_valid(&KEY, a, t ^ (1 << bit)), "nonce {nonce:#x}, bit {bit} flipped still valid");
            }
        }
    }

    /// An edge cookie is bound to its address, bucket and key, and is not a
    /// token MAC of the same inputs (domain separation).
    #[test]
    fn edge_cookie_is_bound_to_address_bucket_and_key() {
        let a: SocketAddr = "10.0.0.7:4000".parse().unwrap();
        let c = edge_cookie(&KEY, a, 5);
        assert_eq!(c, edge_cookie(&KEY, a, 5));
        assert_ne!(c, edge_cookie(&KEY, "10.0.0.7:4001".parse().unwrap(), 5));
        assert_ne!(c, edge_cookie(&KEY, "10.0.0.8:4000".parse().unwrap(), 5));
        assert_ne!(c, edge_cookie(&KEY, a, 6));
        assert_ne!(c, edge_cookie(&other_key(), a, 5));
        assert_ne!(c as u32, token_mac(&KEY, a, 5));
    }

    /// The subkeys are independent of each other and of the secret: a
    /// different secret changes both, and neither is the secret itself.
    #[test]
    fn subkeys_are_derived_and_distinct() {
        let (k, o) = (LinkKey::new([7; 32]), other_key());
        assert_ne!(k.mac, o.mac);
        assert_ne!(k.seal, o.seal);
        assert_ne!(k.seal, [7; 32]);
        assert_ne!(k.mac[..], k.seal[..16]);
        assert_eq!(format!("{k:?}"), "LinkKey(..)");
    }

    /// Edge: shorter than a nonce and a tag is `Short`; authentic but cut or
    /// unknown headers are `Malformed`, never read past.
    #[test]
    fn envelope_refuses_a_cut_header() {
        let mut w4 = wrap(&KEY, Dir::Up, "10.0.0.7:9".parse().unwrap(), &[]);
        assert_eq!(w4.len(), NONCE_LEN + 7 + TAG_LEN);
        let w6 = wrap(&KEY, Dir::Up, "[::1]:9".parse().unwrap(), &[]);
        assert_eq!(w6.len(), ENVELOPE_MAX);
        assert_eq!(unwrap(&KEY, Dir::Up, &mut w4[..NONCE_LEN + TAG_LEN - 1]), Err(EnvelopeError::Short));
        assert_eq!(unwrap(&KEY, Dir::Up, &mut []), Err(EnvelopeError::Short));
        assert!(unwrap(&KEY, Dir::Up, &mut w4).is_ok());
        // Correctly sealed bodies that are not a header.
        let sealed = |body: &[u8]| {
            let mut out = vec![0u8; NONCE_LEN];
            out.extend(body);
            seal(&KEY, Dir::Up, out)
        };
        assert_eq!(unwrap(&KEY, Dir::Up, &mut sealed(&[4, 10, 0, 0, 7, 9])), Err(EnvelopeError::Malformed));
        assert_eq!(unwrap(&KEY, Dir::Up, &mut sealed(&[5, 0, 0, 0, 0, 0, 0])), Err(EnvelopeError::Malformed));
        // Exactly a nonce and a tag around nothing: authentic, but empty.
        assert_eq!(unwrap(&KEY, Dir::Up, &mut sealed(&[])), Err(EnvelopeError::Malformed));
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
