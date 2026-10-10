//! The client's side of the C ABI: an engine's game client speaking to an
//! Aegis relay or origin through [`Connection`] — join handshake, session
//! token, input counter, snapshot proofs, sealing. The engine owns its
//! socket and its game; this turns intent into datagrams and datagrams into
//! what happened.

use std::ffi::{c_char, CStr};

use aegis_client_sdk::conn::{ConnError, Connection, Received};
use aegis_protocol::{ClientKeys, EventKind, Vec2, CLIENT_KEYS_LEN};

use crate::{boundary, AEGIS_ERR_ARG, AEGIS_ERR_NULL, AEGIS_ERR_PANIC, AEGIS_OK};

/// `buf` too small for the datagram; nothing was written.
pub const AEGIS_ERR_BUFFER: i32 = -6;
/// An input before the client was admitted.
pub const AEGIS_ERR_NOT_JOINED: i32 = -7;
/// An input claiming a tick whose snapshot is not among the last 16.
pub const AEGIS_ERR_UNKNOWN_TICK: i32 = -8;
/// An input claiming an older tick than an earlier input.
pub const AEGIS_ERR_TICK_REGRESSED: i32 = -9;
/// A datagram that did not open under this client's keys.
pub const AEGIS_ERR_BAD_SEAL: i32 = -10;
/// A datagram that is not a server message.
pub const AEGIS_ERR_MALFORMED: i32 = -11;

/// Bytes of the keys the game's backend hands a client.
pub const AEGIS_CLIENT_KEYS_LEN: usize = CLIENT_KEYS_LEN;
/// Longest name a client may join under, in bytes of UTF-8.
pub const AEGIS_NAME_MAX: usize = 32;
/// A send buffer this large holds any datagram a client sends.
pub const AEGIS_SEND_MAX: usize = 160;
/// `tick` for "the newest snapshot received".
pub const AEGIS_TICK_NEWEST: u32 = u32::MAX;

pub const AEGIS_RX_JOINED: u8 = 1;
pub const AEGIS_RX_CHALLENGE: u8 = 2;
pub const AEGIS_RX_SNAPSHOT: u8 = 3;
pub const AEGIS_RX_EVENT: u8 = 4;

pub const AEGIS_EVENT_HIT: u8 = 1;
pub const AEGIS_EVENT_DEATH: u8 = 2;
pub const AEGIS_EVENT_JOIN: u8 = 3;
pub const AEGIS_EVENT_LEAVE: u8 = 4;

/// What a datagram from the server was. `kind` is an `AEGIS_RX_*`; the
/// other fields are read by kind:
///   - JOINED: `tick`, `player` (this client's id);
///   - CHALLENGE: nothing — send `aegis_client_join` again;
///   - SNAPSHOT: `tick` (`aegis_client_player` reads the players);
///   - EVENT: `tick`, `event` (an `AEGIS_EVENT_*`), and `player`, `other`,
///     `damage`: HIT shooter, target, damage; DEATH player, killer; JOIN
///     and LEAVE player.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AegisReceived {
    pub tick: u32,
    pub kind: u8,
    pub event: u8,
    pub player: u8,
    pub other: u8,
    pub damage: u8,
}

impl From<Received> for AegisReceived {
    fn from(r: Received) -> Self {
        let z = Self::default();
        match r {
            Received::Joined { player_id, tick } => Self { tick, kind: AEGIS_RX_JOINED, player: player_id, ..z },
            Received::Challenge => Self { kind: AEGIS_RX_CHALLENGE, ..z },
            Received::Snapshot { tick } => Self { tick, kind: AEGIS_RX_SNAPSHOT, ..z },
            Received::Event { tick, kind } => {
                let (event, player, other, damage) = match kind {
                    EventKind::Hit { shooter, target, damage } => (AEGIS_EVENT_HIT, shooter, target, damage),
                    EventKind::Death { player, by } => (AEGIS_EVENT_DEATH, player, by, 0),
                    EventKind::Join { player } => (AEGIS_EVENT_JOIN, player, 0, 0),
                    EventKind::Leave { player } => (AEGIS_EVENT_LEAVE, player, 0, 0),
                };
                Self { tick, kind: AEGIS_RX_EVENT, event, player, other, damage }
            }
        }
    }
}

/// One player in the newest snapshot.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AegisPlayer {
    pub x: f32,
    pub y: f32,
    pub id: u8,
    pub health: u8,
    pub alive: u8,
}

/// Opaque to C.
pub struct AegisClient {
    conn: Connection,
    poisoned: bool,
}

fn status(e: ConnError) -> i32 {
    match e {
        ConnError::NotJoined => AEGIS_ERR_NOT_JOINED,
        ConnError::UnknownTick => AEGIS_ERR_UNKNOWN_TICK,
        ConnError::TickRegressed => AEGIS_ERR_TICK_REGRESSED,
        ConnError::BadSeal => AEGIS_ERR_BAD_SEAL,
        ConnError::Malformed => AEGIS_ERR_MALFORMED,
    }
}

fn with_client(c: *mut AegisClient, f: impl FnOnce(&mut AegisClient) -> i32) -> i32 {
    // SAFETY: from `aegis_client_new`, not freed, one thread at a time
    // (aegis.h); null checked.
    let Some(c) = (unsafe { c.as_mut() }) else { return AEGIS_ERR_NULL };
    if c.poisoned {
        return AEGIS_ERR_PANIC;
    }
    let s = boundary(|| f(c));
    if s == AEGIS_ERR_PANIC {
        c.poisoned = true;
    }
    s
}

/// Copy `d` into `buf` (capacity `cap`): its length, or AEGIS_ERR_BUFFER.
fn emit(d: &[u8], buf: *mut u8, cap: usize) -> i32 {
    if buf.is_null() {
        return AEGIS_ERR_NULL;
    }
    if d.len() > cap {
        return AEGIS_ERR_BUFFER;
    }
    // SAFETY: the caller says `buf` holds `cap` bytes; d.len() <= cap.
    unsafe { std::ptr::copy_nonoverlapping(d.as_ptr(), buf, d.len()) };
    d.len() as i32
}

/// A client joining as `name` (UTF-8, NUL-terminated, at most
/// AEGIS_NAME_MAX bytes, else AEGIS_ERR_ARG). `keys`: the
/// AEGIS_CLIENT_KEYS_LEN bytes the game's backend issued, to speak through
/// a relay; NULL to speak straight to an origin, unsealed.
#[no_mangle]
pub extern "C" fn aegis_client_new(name: *const c_char, keys: *const u8, out: *mut *mut AegisClient) -> i32 {
    boundary(|| {
        if name.is_null() || out.is_null() {
            return AEGIS_ERR_NULL;
        }
        // SAFETY: the caller passes a NUL-terminated string.
        let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else { return AEGIS_ERR_ARG };
        if name.len() > AEGIS_NAME_MAX {
            return AEGIS_ERR_ARG;
        }
        // SAFETY: the caller says `keys`, when not null, holds
        // AEGIS_CLIENT_KEYS_LEN bytes.
        let keys = unsafe { keys.cast::<[u8; CLIENT_KEYS_LEN]>().as_ref() }.map(ClientKeys::from_bytes);
        let c = Box::new(AegisClient { conn: Connection::new(name, keys), poisoned: false });
        // SAFETY: null checked above.
        unsafe { *out = Box::into_raw(c) };
        AEGIS_OK
    })
}

/// Free a client. NULL is a no-op.
#[no_mangle]
pub extern "C" fn aegis_client_free(c: *mut AegisClient) {
    if !c.is_null() {
        // SAFETY: from `aegis_client_new`, freed once (aegis.h).
        drop(unsafe { Box::from_raw(c) });
    }
}

/// Write the Join datagram into `buf`: its length, or a negative status.
/// Send it again after a CHALLENGE: it then carries the cookie.
#[no_mangle]
pub extern "C" fn aegis_client_join(c: *mut AegisClient, buf: *mut u8, cap: usize) -> i32 {
    with_client(c, |c| emit(&c.conn.join(), buf, cap))
}

/// Write an input chosen on the snapshot of `tick` (AEGIS_TICK_NEWEST: the
/// newest received) into `buf`: its length, or a negative status. Ticks
/// claimed must not go backwards. Nothing is counted when refused.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aegis_client_input(
    c: *mut AegisClient,
    tick: u32,
    move_x: f32,
    move_y: f32,
    aim_x: f32,
    aim_y: f32,
    shoot: bool,
    buf: *mut u8,
    cap: usize,
) -> i32 {
    if buf.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        let tick = (tick != AEGIS_TICK_NEWEST).then_some(tick);
        match c.conn.input(tick, Vec2::new(move_x, move_y), Vec2::new(aim_x, aim_y), shoot) {
            Ok(d) => emit(&d, buf, cap),
            Err(e) => status(e),
        }
    })
}

/// Read one datagram from the server (`len` bytes at `data`, not
/// modified) into `*out`.
#[no_mangle]
pub extern "C" fn aegis_client_receive(
    c: *mut AegisClient,
    data: *const u8,
    len: usize,
    out: *mut AegisReceived,
) -> i32 {
    if data.is_null() || out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        // SAFETY: the caller says `data` holds `len` bytes.
        let mut d = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        match c.conn.receive(&mut d) {
            Ok(r) => {
                // SAFETY: null checked above.
                unsafe { *out = r.into() };
                AEGIS_OK
            }
            Err(e) => status(e),
        }
    })
}

/// This client's player id once joined, else AEGIS_ERR_NOT_JOINED.
#[no_mangle]
pub extern "C" fn aegis_client_player_id(c: *mut AegisClient) -> i32 {
    with_client(c, |c| c.conn.player_id().map_or(AEGIS_ERR_NOT_JOINED, i32::from))
}

/// How many players the newest snapshot holds.
#[no_mangle]
pub extern "C" fn aegis_client_player_count(c: *mut AegisClient) -> i32 {
    with_client(c, |c| c.conn.players().len() as i32)
}

/// Player `i` of the newest snapshot; AEGIS_ERR_ARG past the count.
#[no_mangle]
pub extern "C" fn aegis_client_player(c: *mut AegisClient, i: u32, out: *mut AegisPlayer) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| match c.conn.players().get(i as usize) {
        Some(p) => {
            let p = AegisPlayer { x: p.pos.x, y: p.pos.y, id: p.id, health: p.health, alive: p.alive as u8 };
            // SAFETY: null checked above.
            unsafe { *out = p };
            AEGIS_OK
        }
        None => AEGIS_ERR_ARG,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{encode, mint_connect, seal_down, EdgeKey, ServerMsg};
    use std::ptr::{null, null_mut};

    fn client(name: &[u8], keys: Option<&[u8; CLIENT_KEYS_LEN]>) -> Result<*mut AegisClient, i32> {
        let mut c = null_mut();
        match aegis_client_new(name.as_ptr().cast(), keys.map_or(null(), |k| k.as_ptr()), &mut c) {
            AEGIS_OK => Ok(c),
            e => Err(e),
        }
    }

    /// The name's edge, and the largest datagram a client can send fits
    /// AEGIS_SEND_MAX: a Join with the longest name and a cookie, sealed.
    #[test]
    fn the_longest_name_fits_the_send_buffer_one_byte_more_is_refused() {
        let edge = EdgeKey::new([1; 32]);
        let keys = mint_connect(&edge, u64::MAX);
        let mut name = vec![b'x'; AEGIS_NAME_MAX + 1];
        name.push(0);
        assert_eq!(client(&name, Some(&keys.to_bytes())).err(), Some(AEGIS_ERR_ARG));
        name.remove(0);
        let c = client(&name, Some(&keys.to_bytes())).unwrap();
        let challenge = seal_down(&edge, &keys.sid, &encode(&ServerMsg::Challenge { cookie: u64::MAX }));
        let mut rx = AegisReceived::default();
        assert_eq!(aegis_client_receive(c, challenge.as_ptr(), challenge.len(), &mut rx), AEGIS_OK);
        assert_eq!(rx.kind, AEGIS_RX_CHALLENGE);
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let n = aegis_client_join(c, buf.as_mut_ptr(), buf.len());
        assert!(n > 0 && n as usize <= AEGIS_SEND_MAX, "{n}");
        assert_eq!(aegis_client_join(c, buf.as_mut_ptr(), n as usize - 1), AEGIS_ERR_BUFFER);
        assert_eq!(aegis_client_join(c, buf.as_mut_ptr(), n as usize), n);
        aegis_client_free(c);
        assert_eq!(client(b"\xff\0", None).err(), Some(AEGIS_ERR_ARG), "not UTF-8");
    }

    #[test]
    fn statuses_map_one_to_one() {
        let c = client(b"riw\0", None).unwrap();
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let mut input = |c, tick| aegis_client_input(c, tick, 0.0, 0.0, 1.0, 0.0, true, buf.as_mut_ptr(), buf.len());
        assert_eq!(input(c, AEGIS_TICK_NEWEST), AEGIS_ERR_NOT_JOINED);
        let mut rx = AegisReceived::default();
        fn recv(c: *mut AegisClient, m: &ServerMsg, rx: &mut AegisReceived) -> i32 {
            let d = encode(m);
            aegis_client_receive(c, d.as_ptr(), d.len(), rx)
        }
        assert_eq!(recv(c, &ServerMsg::Joined { player_id: 4, token: 7, tick: 1 }, &mut rx), AEGIS_OK);
        assert_eq!(aegis_client_player_id(c), 4);
        assert_eq!(input(c, AEGIS_TICK_NEWEST), AEGIS_ERR_UNKNOWN_TICK);
        recv(c, &ServerMsg::Snapshot { tick: 2, proof: 1, players: Vec::new() }, &mut rx);
        recv(c, &ServerMsg::Snapshot { tick: 3, proof: 1, players: Vec::new() }, &mut rx);
        assert!(input(c, 3) > 0);
        assert_eq!(input(c, 2), AEGIS_ERR_TICK_REGRESSED);
        assert_eq!(aegis_client_receive(c, [9u8].as_ptr(), 1, &mut rx), AEGIS_ERR_MALFORMED);
        let event = ServerMsg::Event { tick: 3, kind: EventKind::Hit { shooter: 4, target: 2, damage: 25 } };
        assert_eq!(recv(c, &event, &mut rx), AEGIS_OK);
        assert_eq!(
            rx,
            AegisReceived { tick: 3, kind: AEGIS_RX_EVENT, event: AEGIS_EVENT_HIT, player: 4, other: 2, damage: 25 }
        );
        aegis_client_free(c);

        let keys = mint_connect(&EdgeKey::new([1; 32]), u64::MAX).to_bytes();
        let c = client(b"riw\0", Some(&keys)).unwrap();
        let d = encode(&ServerMsg::Challenge { cookie: 1 });
        assert_eq!(
            aegis_client_receive(c, d.as_ptr(), d.len(), &mut rx),
            AEGIS_ERR_BAD_SEAL,
            "unsealed to a sealed client"
        );
        aegis_client_free(c);
    }
}
