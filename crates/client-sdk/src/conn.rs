//! A real client's side of the wire, with no game in it: what a game client
//! (an engine, through `aegis-ffi`) needs to speak to an Aegis server or
//! relay. The bots in this crate decide *what* to send; a [`Connection`]
//! turns that into bytes and keeps the state the protocol asks a client to
//! keep — the join handshake, the session token, the input counter, and the
//! proofs of the snapshots it was sent.
//!
//! The protocol, as a client lives it:
//!   1. send [`Connection::join`];
//!   2. a [`Received::Challenge`] means "send `join` again" (the cookie is
//!      kept here and goes with it);
//!   3. [`Received::Joined`]: inputs may go from now on;
//!   4. every [`Received::Snapshot`] carries a proof of its tick; an input
//!      names the tick it was chosen on — the one the client *displayed* —
//!      and [`Connection::input`] echoes that tick's proof. The server keeps
//!      16 ticks, so so does this.
//!
//! Through a relay every datagram is sealed under the [`ClientKeys`] the
//! game's backend issued; directly to an origin, nothing is.

use aegis_protocol::{
    decode, frame, open_down, seal_up, ClientKeys, ClientMsg, EdgeError, EventKind, PlayerId, PlayerState, ServerMsg,
    Vec2, NO_TOKEN, PROTOCOL_VERSION,
};

/// Snapshots whose proofs are kept: as many as the server judges against
/// (`server::history`, 16 ticks).
pub const PROOFS: usize = 16;

/// What a datagram from the server was.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// Admitted as `player_id`; inputs may go from now on.
    Joined {
        player_id: PlayerId,
        tick: u32,
    },
    /// Send [`Connection::join`] again: it now carries the cookie.
    Challenge,
    /// A snapshot of `tick`, now [`Connection::players`]. An older one than
    /// the newest seen is still kept as a proof but does not replace the
    /// players.
    Snapshot {
        tick: u32,
    },
    Event {
        tick: u32,
        kind: EventKind,
    },
}

/// Why a call refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnError {
    /// An input before `Joined`.
    NotJoined,
    /// An input claiming a tick whose snapshot is not among the last
    /// [`PROOFS`] received.
    UnknownTick,
    /// An input claiming an older tick than an earlier input did: the
    /// server's `stale_tick` guard drops it, so it is refused here first.
    TickRegressed,
    /// The datagram did not open under this client's keys.
    BadSeal,
    /// It opened (or needed no opening) but is not a server message.
    Malformed,
}

impl From<EdgeError> for ConnError {
    fn from(e: EdgeError) -> Self {
        match e {
            EdgeError::Short | EdgeError::BadSeal | EdgeError::Retired => ConnError::BadSeal,
        }
    }
}

pub struct Connection {
    name: String,
    keys: Option<ClientKeys>,
    cookie: Option<u64>,
    joined: Option<(PlayerId, u64)>,
    seq: u32,
    /// (tick, proof), at `tick % PROOFS`.
    proofs: [Option<(u32, u32)>; PROOFS],
    newest: Option<u32>,
    claimed: Option<u32>,
    players: Vec<PlayerState>,
}

impl Connection {
    /// A client named `name`, sealing under `keys` (through a relay) or not
    /// (`None`: straight to an origin).
    pub fn new(name: impl Into<String>, keys: Option<ClientKeys>) -> Self {
        Self {
            name: name.into(),
            keys,
            cookie: None,
            joined: None,
            seq: 0,
            proofs: [None; PROOFS],
            newest: None,
            claimed: None,
            players: Vec::new(),
        }
    }

    fn wire(&self, framed: Vec<u8>) -> Vec<u8> {
        match &self.keys {
            Some(k) => seal_up(k, &framed),
            None => framed,
        }
    }

    /// The Join datagram: with the cookie once challenged.
    pub fn join(&self) -> Vec<u8> {
        let msg = ClientMsg::Join { name: self.name.clone(), protocol: PROTOCOL_VERSION, cookie: self.cookie };
        self.wire(frame(NO_TOKEN, &msg))
    }

    /// An input chosen on the snapshot of `tick` (`None`: the newest
    /// received). Directions are the game's; the server clamps `move_dir`.
    pub fn input(&mut self, tick: Option<u32>, move_dir: Vec2, aim: Vec2, shoot: bool) -> Result<Vec<u8>, ConnError> {
        let (_, token) = self.joined.ok_or(ConnError::NotJoined)?;
        let tick = tick.or(self.newest).ok_or(ConnError::UnknownTick)?;
        let proof = self.proof(tick).ok_or(ConnError::UnknownTick)?;
        if self.claimed.is_some_and(|c| tick < c) {
            return Err(ConnError::TickRegressed);
        }
        self.claimed = Some(tick);
        self.seq += 1;
        Ok(self.wire(frame(token, &ClientMsg::Input { seq: self.seq, tick, proof, move_dir, aim, shoot })))
    }

    /// Read one datagram from the server (opened in place when sealed).
    pub fn receive(&mut self, wire: &mut [u8]) -> Result<Received, ConnError> {
        let body = match &self.keys {
            Some(k) => open_down(k, wire)?,
            None => wire,
        };
        let msg = decode::<ServerMsg>(body).map_err(|_| ConnError::Malformed)?;
        Ok(match msg {
            ServerMsg::Joined { player_id, token, tick } => {
                self.joined = Some((player_id, token));
                Received::Joined { player_id, tick }
            }
            ServerMsg::Challenge { cookie } => {
                self.cookie = Some(cookie);
                Received::Challenge
            }
            ServerMsg::Snapshot { tick, proof, players } => {
                self.proofs[tick as usize % PROOFS] = Some((tick, proof));
                if self.newest.is_none_or(|n| tick > n) {
                    self.newest = Some(tick);
                    self.players = players;
                }
                Received::Snapshot { tick }
            }
            ServerMsg::Event { tick, kind } => Received::Event { tick, kind },
        })
    }

    fn proof(&self, tick: u32) -> Option<u32> {
        self.proofs[tick as usize % PROOFS].filter(|&(t, _)| t == tick).map(|(_, p)| p)
    }

    /// The player id, once joined.
    pub fn player_id(&self) -> Option<PlayerId> {
        self.joined.map(|(id, _)| id)
    }

    /// The tick of the newest snapshot received.
    pub fn newest_tick(&self) -> Option<u32> {
        self.newest
    }

    /// The players in the newest snapshot: this client and whoever it can
    /// see.
    pub fn players(&self) -> &[PlayerState] {
        &self.players
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{encode, mint_connect, open_up, seal_down, split_frame, EdgeKey};

    fn snap(tick: u32) -> Vec<u8> {
        encode(&ServerMsg::Snapshot { tick, proof: tick * 7 + 1, players: Vec::new() })
    }

    fn input_of(wire: &[u8]) -> (u64, ClientMsg) {
        let (token, body) = split_frame(wire).unwrap();
        (token, decode(body).unwrap())
    }

    #[test]
    fn the_handshake_carries_the_cookie_and_then_the_token() {
        let mut c = Connection::new("riw", None);
        assert_eq!(c.input(None, Vec2::ZERO, Vec2::ZERO, false), Err(ConnError::NotJoined));
        let (t, m) = input_of(&c.join());
        assert_eq!(
            (t, m),
            (NO_TOKEN, ClientMsg::Join { name: "riw".into(), protocol: PROTOCOL_VERSION, cookie: None })
        );
        assert_eq!(c.receive(&mut encode(&ServerMsg::Challenge { cookie: 42 })), Ok(Received::Challenge));
        assert!(matches!(input_of(&c.join()).1, ClientMsg::Join { cookie: Some(42), .. }));
        let joined = ServerMsg::Joined { player_id: 3, token: 99, tick: 5 };
        assert_eq!(c.receive(&mut encode(&joined)), Ok(Received::Joined { player_id: 3, tick: 5 }));
        assert_eq!(c.input(None, Vec2::ZERO, Vec2::ZERO, false), Err(ConnError::UnknownTick), "no snapshot yet");
        c.receive(&mut snap(6)).unwrap();
        let (t, m) = input_of(&c.input(None, Vec2::ZERO, Vec2::ZERO, true).unwrap());
        assert_eq!(t, 99);
        assert!(matches!(m, ClientMsg::Input { seq: 1, tick: 6, proof: 43, shoot: true, .. }));
    }

    /// The proof window at its edges: 16 snapshots back is kept, 17 is
    /// overwritten; a claimed tick never goes back.
    #[test]
    fn proofs_cover_the_last_sixteen_ticks_and_claims_only_go_forward() {
        let mut c = Connection::new("riw", None);
        c.receive(&mut encode(&ServerMsg::Joined { player_id: 1, token: 9, tick: 0 })).unwrap();
        for t in 1..=40 {
            c.receive(&mut snap(t)).unwrap();
        }
        let oldest_kept = 40 - PROOFS as u32 + 1;
        assert_eq!(c.input(Some(oldest_kept - 1), Vec2::ZERO, Vec2::ZERO, false), Err(ConnError::UnknownTick));
        assert!(matches!(
            input_of(&c.input(Some(oldest_kept), Vec2::ZERO, Vec2::ZERO, false).unwrap()).1,
            ClientMsg::Input { tick: 25, proof: 176, seq: 1, .. }
        ));
        assert_eq!(c.input(Some(41), Vec2::ZERO, Vec2::ZERO, false), Err(ConnError::UnknownTick), "never sent");
        c.input(Some(30), Vec2::ZERO, Vec2::ZERO, false).unwrap();
        assert_eq!(c.input(Some(29), Vec2::ZERO, Vec2::ZERO, false), Err(ConnError::TickRegressed));
        assert!(matches!(
            input_of(&c.input(Some(30), Vec2::ZERO, Vec2::ZERO, false).unwrap()).1,
            ClientMsg::Input { seq: 3, .. }
        ));
    }

    #[test]
    fn an_older_snapshot_is_a_proof_not_the_picture() {
        let mut c = Connection::new("riw", None);
        let me = |x| ServerMsg::Snapshot {
            tick: x as u32,
            proof: 0,
            players: vec![PlayerState { id: 1, pos: Vec2::new(x, 0.0), health: 100, alive: true }],
        };
        c.receive(&mut encode(&me(5.0))).unwrap();
        c.receive(&mut encode(&me(4.0))).unwrap();
        assert_eq!((c.newest_tick(), c.players()[0].pos.x), (Some(5), 5.0));
    }

    /// Sealed both ways under keys the relay re-derives; anything else is a
    /// bad seal, and garbage that needs no opening is malformed.
    #[test]
    fn through_a_relay_every_datagram_is_sealed() {
        let edge = EdgeKey::new([3; 32]);
        let keys = mint_connect(&edge, u64::MAX);
        let mut c = Connection::new("riw", Some(keys));
        let mut up = c.join();
        let (sid, framed) = open_up(&edge, &mut up).unwrap();
        assert_eq!(sid, keys.sid);
        assert!(matches!(input_of(framed).1, ClientMsg::Join { .. }));
        let mut down = seal_down(&edge, &sid, &encode(&ServerMsg::Challenge { cookie: 1 }));
        assert_eq!(c.receive(&mut down), Ok(Received::Challenge));
        assert_eq!(c.receive(&mut encode(&ServerMsg::Challenge { cookie: 1 })), Err(ConnError::BadSeal));
        assert_eq!(Connection::new("riw", None).receive(&mut [1, 2, 3]), Err(ConnError::Malformed));
    }
}
