//! Opt-in (feature `noise`): a relay that is its own backend.
//!
//! The default model (netcode.io's) needs the game's backend to hand every
//! client its session keys over HTTPS ([`mint_connect`]). A LAN game or a
//! community server has no backend. Here the relay hands them out itself,
//! over one Noise NK round trip (Trevor Perrin, *The Noise Protocol
//! Framework*, rev 34, 2018): the client knows the relay's static public
//! key — shipped with the game, or the server list — and nothing else.
//!
//! ```text
//! client -> relay   HELLO_SID | cookie (0: none) | e, es
//! relay  -> client  HELLO_SID | cookie                    no cookie yet: no DH
//! client -> relay   HELLO_SID | cookie | e, es            a fresh handshake
//! relay  -> client  HELLO_SID | e, ee, [ClientKeys]       sealed to this e
//! ```
//!
//! After that the client is an ordinary client: its keys were minted by
//! [`mint_connect`] under the relay's current edge key, so the relay stays
//! stateless — the handshake is answered in one step and forgotten. What
//! Noise adds over HTTPS delivery is no backend; what it adds over the
//! client's own leg is forward secrecy of the key delivery (the ephemeral
//! `ee`), while the session traffic is as forward-secret as the edge key's
//! epoch ([`crate::EdgeRing`]).
//!
//! The cost is a Diffie-Hellman at the edge, which is why it is opt-in and
//! why a hello is answered with a DH only when it brings back a cookie the
//! relay issued to its source address: a forged source never costs one,
//! and a reply larger than the hello (amplification) only ever goes to an
//! address that proved it receives.

use snow::params::{DHChoice, NoiseParams};
use snow::resolvers::{CryptoResolver, DefaultResolver};

use crate::{mint_connect, ClientKeys, EdgeKey, CLIENT_KEYS_LEN, HELLO_EPOCH, SID_LEN};

const PATTERN: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"aegis noise v1";

/// The sid every hello and its answers carry: never minted, because its
/// epoch is [`HELLO_EPOCH`], which no [`EdgeKey`] may have.
pub const HELLO_SID: [u8; SID_LEN] = {
    let mut s = [0u8; SID_LEN];
    s[8] = HELLO_EPOCH;
    s
};

/// Noise message 1 with an empty payload: `e` and a tag.
const MSG1_LEN: usize = 32 + 16;
/// Noise message 2: `e`, then the client's keys and a tag.
const MSG2_LEN: usize = 32 + CLIENT_KEYS_LEN + 16;

/// A hello: the sid, a cookie, Noise message 1.
pub const HELLO_LEN: usize = SID_LEN + 8 + MSG1_LEN;
/// A cookie challenge: the sid and the cookie. Smaller than the hello.
pub const CHALLENGE_LEN: usize = SID_LEN + 8;
/// A welcome: the sid and Noise message 2. Larger than the hello (2x), so
/// it goes only to an address that brought back a cookie.
pub const WELCOME_LEN: usize = SID_LEN + MSG2_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseError {
    /// Not the length of what was expected.
    Size,
    /// Did not open: another relay's key, altered, or not a handshake.
    BadHandshake,
}

fn params() -> NoiseParams {
    PATTERN.parse().expect("a valid Noise pattern")
}

/// The relay's static key pair.
#[derive(Clone)]
pub struct RelayStatic {
    private: [u8; 32],
    public: [u8; 32],
}

impl RelayStatic {
    pub fn from_private(private: [u8; 32]) -> Self {
        let mut dh = DefaultResolver.resolve_dh(&DHChoice::Curve25519).expect("X25519");
        dh.set(&private);
        let public = dh.pubkey().try_into().expect("32-byte X25519 public key");
        Self { private, public }
    }

    /// A fresh pair from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut private = [0u8; 32];
        getrandom::fill(&mut private).expect("aegis-protocol: OS random source unavailable");
        Self::from_private(private)
    }

    /// What clients are given to pin.
    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

/// Never prints the private key.
impl std::fmt::Debug for RelayStatic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RelayStatic {{ public: {:02x?}, .. }}", &self.public[..4])
    }
}

/// A client's handshake in flight.
pub struct Hello {
    state: snow::HandshakeState,
}

/// Client side: a hello to the relay whose static public key is
/// `relay_public`, carrying `cookie` once challenged. Each call is a fresh
/// handshake.
pub fn hello(relay_public: &[u8; 32], cookie: Option<u64>) -> (Hello, Vec<u8>) {
    let mut state = snow::Builder::new(params())
        .prologue(PROLOGUE)
        .and_then(|b| b.remote_public_key(relay_public))
        .and_then(|b| b.build_initiator())
        .expect("an NK initiator");
    let mut out = vec![0u8; HELLO_LEN];
    out[..SID_LEN].copy_from_slice(&HELLO_SID);
    out[SID_LEN..SID_LEN + 8].copy_from_slice(&cookie.unwrap_or(0).to_le_bytes());
    let n = state.write_message(&[], &mut out[SID_LEN + 8..]).expect("message 1");
    debug_assert_eq!(n, MSG1_LEN);
    (Hello { state }, out)
}

/// What the relay answered a hello with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Send [`hello`] again with this cookie.
    Challenge(u64),
    /// The handshake finished: these are the client's keys.
    Welcome(ClientKeys),
}

impl Hello {
    /// Read the relay's answer. A challenge leaves the handshake unused
    /// (start a new one with the cookie); a welcome finishes it. Anything
    /// else — not a hello answer, or the wrong size — touches nothing, so a
    /// stray datagram costs the handshake nothing; a welcome that does not
    /// open ([`NoiseError::BadHandshake`] at [`WELCOME_LEN`]) spends it.
    pub fn answer(&mut self, reply: &[u8]) -> Result<Answer, NoiseError> {
        if reply.get(..SID_LEN) != Some(&HELLO_SID[..]) {
            return Err(NoiseError::BadHandshake);
        }
        match reply.len() {
            CHALLENGE_LEN => Ok(Answer::Challenge(u64::from_le_bytes(reply[SID_LEN..].try_into().expect("8 bytes")))),
            WELCOME_LEN => {
                let mut keys = [0u8; CLIENT_KEYS_LEN + 16];
                let n = self.state.read_message(&reply[SID_LEN..], &mut keys).map_err(|_| NoiseError::BadHandshake)?;
                let keys: &[u8; CLIENT_KEYS_LEN] = keys[..n].try_into().map_err(|_| NoiseError::BadHandshake)?;
                Ok(Answer::Welcome(ClientKeys::from_bytes(keys)))
            }
            _ => Err(NoiseError::Size),
        }
    }
}

/// Relay side: is this client datagram a hello (and not a session's)?
pub fn is_hello(bytes: &[u8]) -> bool {
    bytes.get(..SID_LEN) == Some(&HELLO_SID[..])
}

/// Relay side: the cookie a hello brought (`None`: none, or not a hello of
/// the right size).
pub fn hello_cookie(bytes: &[u8]) -> Option<u64> {
    if bytes.len() != HELLO_LEN || !is_hello(bytes) {
        return None;
    }
    Some(u64::from_le_bytes(bytes[SID_LEN..SID_LEN + 8].try_into().expect("8 bytes"))).filter(|&c| c != 0)
}

/// Relay side: the challenge to a hello without a valid cookie. No DH.
pub fn challenge(cookie: u64) -> Vec<u8> {
    let mut out = HELLO_SID.to_vec();
    out.extend(cookie.to_le_bytes());
    out
}

/// Relay side: answer a hello whose cookie was checked. One responder
/// handshake (two DH), a session minted under `edge` admitting Joins until
/// `expires`, sealed to the client's ephemeral key; nothing is kept.
pub fn welcome(relay: &RelayStatic, edge: &EdgeKey, expires: u64, hello: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if hello.len() != HELLO_LEN || !is_hello(hello) {
        return Err(NoiseError::Size);
    }
    let mut state = snow::Builder::new(params())
        .prologue(PROLOGUE)
        .and_then(|b| b.local_private_key(&relay.private))
        .and_then(|b| b.build_responder())
        .expect("an NK responder");
    let mut empty = [0u8; MSG1_LEN];
    state.read_message(&hello[SID_LEN + 8..], &mut empty).map_err(|_| NoiseError::BadHandshake)?;
    let keys = mint_connect(edge, expires).to_bytes();
    let mut out = vec![0u8; WELCOME_LEN];
    out[..SID_LEN].copy_from_slice(&HELLO_SID);
    let n = state.write_message(&keys, &mut out[SID_LEN..]).map_err(|_| NoiseError::BadHandshake)?;
    debug_assert_eq!(n, MSG2_LEN);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open_up, seal_up};

    #[test]
    fn a_welcome_carries_keys_the_relay_can_open_and_nobody_else_can_read() {
        let relay = RelayStatic::generate();
        let edge = EdgeKey::with_epoch([4; 32], 3);
        let (mut h, wire) = hello(&relay.public(), Some(9));
        assert_eq!((wire.len(), hello_cookie(&wire)), (HELLO_LEN, Some(9)));
        let reply = welcome(&relay, &edge, 1234, &wire).unwrap();
        assert!(reply.len() == WELCOME_LEN && WELCOME_LEN > HELLO_LEN && CHALLENGE_LEN < HELLO_LEN);
        // Strays first: a session's datagram, a short one, one a byte long.
        // None spends the handshake.
        assert_eq!(h.answer(&[7; WELCOME_LEN]), Err(NoiseError::BadHandshake));
        assert_eq!(h.answer(&reply[..WELCOME_LEN - 1]), Err(NoiseError::Size));
        assert_eq!(h.answer(&reply[..1]), Err(NoiseError::BadHandshake));
        let Ok(Answer::Welcome(keys)) = h.answer(&reply) else { panic!("no welcome") };
        assert_eq!((keys.sid.expires(), keys.sid.epoch()), (1234, 3));
        let mut up = seal_up(&keys, b"a frame of some length");
        assert_eq!(
            open_up(&edge, &mut up).map(|(s, f)| (s, f.to_vec())),
            Ok((keys.sid, b"a frame of some length".to_vec()))
        );
        // The keys are not in the welcome in the clear.
        assert!(!reply.windows(32).any(|w| keys.to_bytes().windows(32).any(|k| k == w)));
    }

    /// Pinned to another relay's key, the handshake does not open at the
    /// relay; altered on the way back, it does not open at the client.
    #[test]
    fn the_wrong_relay_or_an_altered_welcome_is_refused() {
        let (relay, other) = (RelayStatic::generate(), RelayStatic::generate());
        let edge = EdgeKey::new([4; 32]);
        let (_, wire) = hello(&other.public(), Some(1));
        assert_eq!(welcome(&relay, &edge, 1, &wire), Err(NoiseError::BadHandshake));
        let (mut h, wire) = hello(&relay.public(), Some(1));
        let mut reply = welcome(&relay, &edge, 1, &wire).unwrap();
        reply[SID_LEN + 40] ^= 1;
        assert_eq!(h.answer(&reply).err(), Some(NoiseError::BadHandshake));
        assert_eq!(welcome(&relay, &edge, 1, &wire[..HELLO_LEN - 1]), Err(NoiseError::Size));
    }

    #[test]
    fn a_challenge_and_a_cookieless_hello() {
        let relay = RelayStatic::from_private([5; 32]);
        assert_eq!(RelayStatic::from_private([5; 32]).public(), relay.public(), "public from private is fixed");
        let (mut h, wire) = hello(&relay.public(), None);
        assert_eq!(hello_cookie(&wire), None);
        assert!(is_hello(&wire) && !is_hello(&wire[1..]));
        assert_eq!(h.answer(&challenge(77)), Ok(Answer::Challenge(77)));
        assert!(!format!("{relay:?}").contains(&format!("{:02x?}", [5u8; 4])), "the private key printed");
    }
}
