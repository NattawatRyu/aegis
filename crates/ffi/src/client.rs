//! The client's side of the C ABI: an engine's game client speaking to an
//! Aegis relay or origin through [`Connection`] — join handshake, session
//! token, input counter, snapshot proofs, sealing. The engine owns its
//! socket and its game; this turns intent into datagrams and datagrams into
//! what happened.

use std::ffi::{c_char, CStr};

use aegis_client_sdk::conn::{ConnError, Connection, Received};
use aegis_client_sdk::interp::View;
use aegis_protocol::movement::Wall;
use aegis_protocol::{ClientKeys, EventKind, PlayerState, Vec2, CLIENT_KEYS_LEN};

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
/// Nothing to read yet: not interpolating or predicting, or no snapshot
/// has landed since it began. (-12 is the evidence's AEGIS_ERR_BUSY.)
pub const AEGIS_ERR_NOT_READY: i32 = -13;

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

impl From<&PlayerState> for AegisPlayer {
    fn from(p: &PlayerState) -> Self {
        Self { x: p.pos.x, y: p.pos.y, id: p.id, health: p.health, alive: p.alive as u8 }
    }
}

/// One frame to draw (`aegis_client_view`): the moment `at`, in ticks; the
/// tick an input chosen on it claims; how many players it holds
/// (`aegis_client_view_player`); and `offset`, how long before `claim` the
/// moment sat in 1/64 ticks (0 down to -128), which
/// `aegis_client_input_drawn` sends.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AegisView {
    pub at: f64,
    pub claim: u32,
    pub players: u32,
    pub offset: i8,
}

/// How steadily the frames drawn so far advanced (`aegis_client_pace`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AegisPace {
    /// Frames judged: every one after the first.
    pub frames: u32,
    /// Advanced by the time that passed, within 10%.
    pub steady: u32,
    /// Advanced by less: waiting for a snapshot.
    pub held: u32,
    /// Advanced by more: a jump.
    pub jumped: u32,
    /// Drawn across a lost or late snapshot.
    pub bridged: u32,
    /// Sum over the frames of ticks behind the newest snapshot.
    pub behind: f64,
}

/// A box nothing walks through, as the server's arena has them.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AegisWall {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

/// How the predictions compared with the server
/// (`aegis_client_prediction_stats`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AegisPredictStats {
    /// Acked inputs checked.
    pub checked: u32,
    /// Of those, predicted to the bit.
    pub exact: u32,
    /// Largest distance between prediction and server.
    pub max_err: f32,
    pub sum_err: f64,
}

/// Opaque to C.
pub struct AegisClient {
    conn: Connection,
    /// The frame `aegis_client_view` drew last.
    view: Option<View>,
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
        let c = Box::new(AegisClient { conn: Connection::new(name, keys), view: None, poisoned: false });
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
            // SAFETY: null checked above.
            unsafe { *out = p.into() };
            AEGIS_OK
        }
        None => AEGIS_ERR_ARG,
    })
}

/// Draw `delay` ticks behind the newest snapshot from now on (clamped to
/// 0..=6; not a finite number: AEGIS_ERR_ARG). Called again, starts over.
#[no_mangle]
pub extern "C" fn aegis_client_interpolate(c: *mut AegisClient, delay: f64) -> i32 {
    with_client(c, |c| {
        if !delay.is_finite() {
            return AEGIS_ERR_ARG;
        }
        c.conn.interpolate(delay);
        c.view = None;
        AEGIS_OK
    })
}

/// `dt` ticks have passed since the last frame (>= 0, finite, else
/// AEGIS_ERR_ARG): the frame to draw now into `*out`. AEGIS_ERR_NOT_READY
/// until interpolating and a snapshot has landed. The frame is kept for
/// `aegis_client_view_player` and `aegis_client_input_drawn`.
#[no_mangle]
pub extern "C" fn aegis_client_view(c: *mut AegisClient, dt: f64, out: *mut AegisView) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        if !dt.is_finite() || dt < 0.0 {
            return AEGIS_ERR_ARG;
        }
        let Some(v) = c.conn.view(dt) else { return AEGIS_ERR_NOT_READY };
        let view = AegisView { at: v.at, claim: v.claim, players: v.players.len() as u32, offset: v.offset };
        c.view = Some(v);
        // SAFETY: null checked above.
        unsafe { *out = view };
        AEGIS_OK
    })
}

/// Player `i` of the last frame drawn, placed at its moment; AEGIS_ERR_ARG
/// past its count, AEGIS_ERR_NOT_READY before a frame.
#[no_mangle]
pub extern "C" fn aegis_client_view_player(c: *mut AegisClient, i: u32, out: *mut AegisPlayer) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        let Some(v) = &c.view else { return AEGIS_ERR_NOT_READY };
        match v.players.get(i as usize) {
            Some(p) => {
                // SAFETY: null checked above.
                unsafe { *out = p.into() };
                AEGIS_OK
            }
            None => AEGIS_ERR_ARG,
        }
    })
}

/// Write an input chosen on the last frame drawn into `buf`: it claims the
/// frame's tick and its offset, so the server judges the shot in the
/// picture on screen. Its length, or a negative status;
/// AEGIS_ERR_NOT_READY before a frame. The offset is the frame's own: the
/// caller cannot set one.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aegis_client_input_drawn(
    c: *mut AegisClient,
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
        let Some(view) = &c.view else { return AEGIS_ERR_NOT_READY };
        match c.conn.input_drawn(view, Vec2::new(move_x, move_y), Vec2::new(aim_x, aim_y), shoot) {
            Ok(d) => emit(&d, buf, cap),
            Err(e) => status(e),
        }
    })
}

/// Write an input chosen on a frame the engine drew on its own, at the
/// moment `at` in ticks (fractional between snapshots), into `buf`: for an
/// engine that interpolates itself. The claim and its offset are worked out
/// from the snapshots received — the first at or after `at` — never taken
/// from the caller. Its length, or a negative status: AEGIS_ERR_ARG if `at`
/// is not finite, AEGIS_ERR_UNKNOWN_TICK if no snapshot kept covers it
/// (past the newest, or before the oldest of the last 16).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aegis_client_input_at(
    c: *mut AegisClient,
    at: f64,
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
        if !at.is_finite() {
            return AEGIS_ERR_ARG;
        }
        match c.conn.input_at(at, Vec2::new(move_x, move_y), Vec2::new(aim_x, aim_y), shoot) {
            Ok(d) => emit(&d, buf, cap),
            Err(e) => status(e),
        }
    })
}

/// How steadily the frames drawn so far advanced; AEGIS_ERR_NOT_READY
/// unless interpolating.
#[no_mangle]
pub extern "C" fn aegis_client_pace(c: *mut AegisClient, out: *mut AegisPace) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        let Some(p) = c.conn.pace() else { return AEGIS_ERR_NOT_READY };
        let p = AegisPace {
            frames: p.frames,
            steady: p.steady,
            held: p.held,
            jumped: p.jumped,
            bridged: p.bridged,
            behind: p.behind,
        };
        // SAFETY: null checked above.
        unsafe { *out = p };
        AEGIS_OK
    })
}

/// Predict this player's position from now on, in an arena with the `n`
/// boxes at `walls` (NULL when `n` is 0) — the server's own. Only a server
/// that moves players with Aegis's movement (`aegis-server`) is predicted
/// to the bit; an engine with its own physics predicts with it. A box not
/// finite or with min > max: AEGIS_ERR_ARG. Called again, starts over.
#[no_mangle]
pub extern "C" fn aegis_client_predict(c: *mut AegisClient, walls: *const AegisWall, n: usize) -> i32 {
    if walls.is_null() && n > 0 {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        let walls = if n == 0 {
            &[][..]
        } else {
            // SAFETY: the caller says `walls` holds `n` boxes; not null.
            unsafe { std::slice::from_raw_parts(walls, n) }
        };
        let ok = |w: &AegisWall| {
            [w.min_x, w.min_y, w.max_x, w.max_y].iter().all(|v| v.is_finite())
                && w.min_x <= w.max_x
                && w.min_y <= w.max_y
        };
        if !walls.iter().all(ok) {
            return AEGIS_ERR_ARG;
        }
        let walls: Vec<Wall> =
            walls.iter().map(|w| Wall::new(Vec2::new(w.min_x, w.min_y), Vec2::new(w.max_x, w.max_y))).collect();
        c.conn.predict_in(&walls);
        AEGIS_OK
    })
}

/// Where this player is once every input sent so far is folded, into
/// `*x`, `*y`; AEGIS_ERR_NOT_READY unless predicting and a snapshot has
/// shown it.
#[no_mangle]
pub extern "C" fn aegis_client_predicted(c: *mut AegisClient, x: *mut f32, y: *mut f32) -> i32 {
    if x.is_null() || y.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        let Some(p) = c.conn.predicted() else { return AEGIS_ERR_NOT_READY };
        // SAFETY: null checked above.
        unsafe {
            *x = p.x;
            *y = p.y;
        }
        AEGIS_OK
    })
}

/// How the predictions compared with the server so far;
/// AEGIS_ERR_NOT_READY unless predicting.
#[no_mangle]
pub extern "C" fn aegis_client_prediction_stats(c: *mut AegisClient, out: *mut AegisPredictStats) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_client(c, |c| {
        let Some(s) = c.conn.prediction_stats() else { return AEGIS_ERR_NOT_READY };
        let s = AegisPredictStats { checked: s.checked, exact: s.exact, max_err: s.max_err, sum_err: s.sum_err };
        // SAFETY: null checked above.
        unsafe { *out = s };
        AEGIS_OK
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
        recv(c, &ServerMsg::Snapshot { tick: 2, proof: 1, ack: 0, players: Vec::new() }, &mut rx);
        recv(c, &ServerMsg::Snapshot { tick: 3, proof: 1, ack: 0, players: Vec::new() }, &mut rx);
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

    fn recv(c: *mut AegisClient, m: &ServerMsg) -> i32 {
        let d = encode(m);
        let mut rx = AegisReceived::default();
        aegis_client_receive(c, d.as_ptr(), d.len(), &mut rx)
    }

    /// Snapshot `t`: this client (4) at x = t, player 2 at x = -t.
    fn snap(t: u32) -> ServerMsg {
        let p = |id, x| PlayerState { id, pos: Vec2::new(x, 0.0), health: 100, alive: true };
        ServerMsg::Snapshot { tick: t, proof: t * 7 + 1, ack: 0, players: vec![p(4, t as f32), p(2, -(t as f32))] }
    }

    /// Every new call at its edges: nothing to read before it is switched
    /// on and before a snapshot lands; arguments out of range refused
    /// without touching the clock.
    #[test]
    fn view_pace_and_prediction_are_not_ready_until_they_are() {
        let c = client(b"riw\0", None).unwrap();
        let (mut v, mut p, mut pace, mut st) =
            (AegisView::default(), AegisPlayer::default(), AegisPace::default(), AegisPredictStats::default());
        let (mut x, mut y) = (0f32, 0f32);
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let drawn =
            |c, buf: &mut [u8]| aegis_client_input_drawn(c, 1.0, 0.0, 1.0, 0.0, true, buf.as_mut_ptr(), buf.len());

        // Not switched on.
        assert_eq!(aegis_client_view(c, 1.0, &mut v), AEGIS_ERR_NOT_READY);
        assert_eq!(aegis_client_view_player(c, 0, &mut p), AEGIS_ERR_NOT_READY);
        assert_eq!(drawn(c, &mut buf), AEGIS_ERR_NOT_READY);
        assert_eq!(aegis_client_pace(c, &mut pace), AEGIS_ERR_NOT_READY);
        assert_eq!(aegis_client_predicted(c, &mut x, &mut y), AEGIS_ERR_NOT_READY);
        assert_eq!(aegis_client_prediction_stats(c, &mut st), AEGIS_ERR_NOT_READY);

        // Switched on, nothing landed.
        assert_eq!(aegis_client_interpolate(c, f64::NAN), AEGIS_ERR_ARG);
        assert_eq!(aegis_client_interpolate(c, f64::INFINITY), AEGIS_ERR_ARG);
        assert_eq!(aegis_client_interpolate(c, 1.0), AEGIS_OK);
        assert_eq!(aegis_client_predict(c, std::ptr::null(), 0), AEGIS_OK, "an open arena");
        assert_eq!(aegis_client_predict(c, std::ptr::null(), 1), AEGIS_ERR_NULL);
        let bad = [
            AegisWall { min_x: 1.0, min_y: 0.0, max_x: 0.0, max_y: 1.0 },
            AegisWall { min_x: 0.0, min_y: 0.0, max_x: f32::NAN, max_y: 1.0 },
        ];
        for w in &bad {
            assert_eq!(aegis_client_predict(c, w, 1), AEGIS_ERR_ARG, "{w:?}");
        }
        let point = AegisWall { min_x: 9.0, min_y: 9.0, max_x: 9.0, max_y: 9.0 };
        assert_eq!(aegis_client_predict(c, &point, 1), AEGIS_OK, "min == max is a box");
        assert_eq!(aegis_client_view(c, 1.0, &mut v), AEGIS_ERR_NOT_READY);
        assert_eq!(aegis_client_pace(c, &mut pace), AEGIS_OK);
        assert_eq!(pace, AegisPace::default());
        assert_eq!(aegis_client_predicted(c, &mut x, &mut y), AEGIS_ERR_NOT_READY, "no snapshot showed it");
        assert_eq!(aegis_client_prediction_stats(c, &mut st), AEGIS_OK);

        // Landed.
        assert_eq!(recv(c, &ServerMsg::Joined { player_id: 4, token: 7, tick: 1 }), AEGIS_OK);
        assert_eq!(drawn(c, &mut buf), AEGIS_ERR_NOT_READY, "joined, no frame yet");
        for t in [2, 3, 4] {
            recv(c, &snap(t));
        }
        for dt in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(aegis_client_view(c, dt, &mut v), AEGIS_ERR_ARG, "{dt}");
        }
        assert_eq!(aegis_client_view(c, 0.0, &mut v), AEGIS_OK);
        assert_eq!(v, AegisView { at: 3.0, claim: 3, players: 2, offset: 0 }, "one behind the newest");
        assert_eq!(aegis_client_view(c, 0.5, &mut v), AEGIS_OK);
        assert!(v.at > 3.0 && v.at < 4.0 && v.claim == 4 && v.offset < 0, "{v:?}");
        assert_eq!(aegis_client_view_player(c, 0, &mut p), AEGIS_OK);
        assert!(p.id == 4 && p.x > 3.0 && p.x < 4.0, "blended: {p:?}");
        assert_eq!(aegis_client_view_player(c, 2, &mut p), AEGIS_ERR_ARG);
        assert!(drawn(c, &mut buf) > 0);
        assert_eq!(aegis_client_predicted(c, &mut x, &mut y), AEGIS_OK);
        assert_eq!((x, y), (4.0 + 5.0, 0.0), "the newest showed it at 4, one step east since");
        assert_eq!(aegis_client_pace(c, &mut pace), AEGIS_OK);
        assert_eq!(pace.frames, 1);

        // Switched on again: starts over, the frame is gone.
        assert_eq!(aegis_client_interpolate(c, 2.0), AEGIS_OK);
        assert_eq!(aegis_client_view_player(c, 0, &mut p), AEGIS_ERR_NOT_READY);
        assert_eq!(drawn(c, &mut buf), AEGIS_ERR_NOT_READY);
        aegis_client_free(c);
    }

    /// Driven alike, the C client and the Rust `Connection` it wraps send
    /// the same bytes: the offset that crosses is the frame's own.
    #[test]
    fn an_input_drawn_is_the_rust_clients_to_the_byte() {
        let c = client(b"riw\0", None).unwrap();
        let mut rust = Connection::new("riw", None);
        assert_eq!(aegis_client_interpolate(c, 1.5), AEGIS_OK);
        rust.interpolate(1.5);
        let both = |rust: &mut Connection, m: &ServerMsg| {
            assert_eq!(recv(c, m), AEGIS_OK);
            rust.receive(&mut encode(m)).unwrap();
        };
        both(&mut rust, &ServerMsg::Joined { player_id: 4, token: 7, tick: 1 });
        let mut offsets = Vec::new();
        for t in 2..20 {
            if t % 5 != 0 {
                both(&mut rust, &snap(t)); // every fifth lost
            }
            for (i, dt) in [0.3, 0.7].into_iter().enumerate() {
                let mut v = AegisView::default();
                assert_eq!(aegis_client_view(c, dt, &mut v), AEGIS_OK);
                let r = rust.view(dt).unwrap();
                assert_eq!((v.at, v.claim, v.offset), (r.at, r.claim, r.offset));
                let shoot = i == 1;
                let mut buf = [0u8; AEGIS_SEND_MAX];
                let n = aegis_client_input_drawn(c, 1.0, 0.5, 0.0, 1.0, shoot, buf.as_mut_ptr(), buf.len());
                let want = rust.input_drawn(&r, Vec2::new(1.0, 0.5), Vec2::new(0.0, 1.0), shoot).unwrap();
                assert_eq!(&buf[..n as usize], &want[..], "tick {t} frame {i}");
                offsets.push(v.offset);
            }
        }
        assert!(offsets.iter().any(|&o| o < 0), "some frames sat between snapshots: {offsets:?}");
        aegis_client_free(c);
    }

    /// A C client told only the moment drawn sends what the Rust client
    /// drawing with its own Interpolator sends; a moment no snapshot covers
    /// is refused and counts nothing.
    #[test]
    fn an_input_at_is_the_interpolating_rust_clients_to_the_byte() {
        let c = client(b"riw\0", None).unwrap();
        let mut rust = Connection::new("riw", None);
        rust.interpolate(1.5);
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let at =
            |c, at, buf: &mut [u8]| aegis_client_input_at(c, at, 1.0, 0.5, 0.0, 1.0, true, buf.as_mut_ptr(), buf.len());
        assert_eq!(at(c, 1.0, &mut buf), AEGIS_ERR_NOT_JOINED);
        let both = |rust: &mut Connection, m: &ServerMsg| {
            assert_eq!(recv(c, m), AEGIS_OK);
            rust.receive(&mut encode(m)).unwrap();
        };
        both(&mut rust, &ServerMsg::Joined { player_id: 4, token: 7, tick: 1 });
        assert_eq!(at(c, 1.0, &mut buf), AEGIS_ERR_UNKNOWN_TICK, "no snapshot yet");
        let mut offsets = Vec::new();
        for t in 2..20 {
            if t % 5 != 0 {
                both(&mut rust, &snap(t)); // every fifth lost
            }
            for dt in [0.3, 0.7] {
                let r = rust.view(dt).unwrap();
                let want = rust.input_drawn(&r, Vec2::new(1.0, 0.5), Vec2::new(0.0, 1.0), true).unwrap();
                let n = at(c, r.at, &mut buf);
                assert_eq!(&buf[..n as usize], &want[..], "tick {t} at {}", r.at);
                offsets.push(r.offset);
            }
        }
        assert!(offsets.iter().any(|&o| o < 0), "some frames sat between snapshots: {offsets:?}");
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(at(c, bad, &mut buf), AEGIS_ERR_ARG, "{bad}");
        }
        assert_eq!(at(c, 19.01, &mut buf), AEGIS_ERR_UNKNOWN_TICK, "past the newest");
        assert_eq!(at(c, 3.0, &mut buf), AEGIS_ERR_UNKNOWN_TICK, "older than the last 16");
        // Refusals counted nothing: the next input is the Rust client's next.
        let want = rust.input_at(19.0, Vec2::new(1.0, 0.5), Vec2::new(0.0, 1.0), true).unwrap();
        let n = at(c, 19.0, &mut buf);
        assert_eq!(&buf[..n as usize], &want[..]);
        aegis_client_free(c);
    }

    /// Every pointer every call takes, null: AEGIS_ERR_NULL, nothing read
    /// or written, and the client still works after.
    #[test]
    fn every_call_refuses_a_null() {
        let nc: *mut AegisClient = null_mut();
        let mut out = null_mut();
        assert_eq!(aegis_client_new(null(), null(), &mut out), AEGIS_ERR_NULL);
        assert_eq!(aegis_client_new(c"riw".as_ptr(), null(), null_mut()), AEGIS_ERR_NULL);
        aegis_client_free(nc);

        let (mut rx, mut p, mut v, mut pace, mut st) = (
            AegisReceived::default(),
            AegisPlayer::default(),
            AegisView::default(),
            AegisPace::default(),
            AegisPredictStats::default(),
        );
        let (mut x, mut y) = (0f32, 0f32);
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let (b, n) = (buf.as_mut_ptr(), buf.len());
        let d = [0u8; 4];
        let w = AegisWall::default();
        let null_client = [
            aegis_client_join(nc, b, n),
            aegis_client_input(nc, AEGIS_TICK_NEWEST, 0.0, 0.0, 1.0, 0.0, false, b, n),
            aegis_client_receive(nc, d.as_ptr(), d.len(), &mut rx),
            aegis_client_player_id(nc),
            aegis_client_player_count(nc),
            aegis_client_player(nc, 0, &mut p),
            aegis_client_interpolate(nc, 1.0),
            aegis_client_view(nc, 1.0, &mut v),
            aegis_client_view_player(nc, 0, &mut p),
            aegis_client_input_drawn(nc, 0.0, 0.0, 1.0, 0.0, false, b, n),
            aegis_client_input_at(nc, 1.0, 0.0, 0.0, 1.0, 0.0, false, b, n),
            aegis_client_pace(nc, &mut pace),
            aegis_client_predict(nc, &w, 1),
            aegis_client_predicted(nc, &mut x, &mut y),
            aegis_client_prediction_stats(nc, &mut st),
        ];
        assert_eq!(null_client, [AEGIS_ERR_NULL; 15]);

        let c = client(b"riw\0", None).unwrap();
        assert_eq!(aegis_client_interpolate(c, 1.0), AEGIS_OK);
        assert_eq!(aegis_client_predict(c, null(), 0), AEGIS_OK);
        let null_arg = [
            aegis_client_join(c, null_mut(), n),
            aegis_client_input(c, AEGIS_TICK_NEWEST, 0.0, 0.0, 1.0, 0.0, false, null_mut(), n),
            aegis_client_receive(c, null(), 0, &mut rx),
            aegis_client_receive(c, d.as_ptr(), d.len(), null_mut()),
            aegis_client_player(c, 0, null_mut()),
            aegis_client_view(c, 1.0, null_mut()),
            aegis_client_view_player(c, 0, null_mut()),
            aegis_client_input_drawn(c, 0.0, 0.0, 1.0, 0.0, false, null_mut(), n),
            aegis_client_input_at(c, 1.0, 0.0, 0.0, 1.0, 0.0, false, null_mut(), n),
            aegis_client_pace(c, null_mut()),
            aegis_client_predicted(c, null_mut(), &mut y),
            aegis_client_predicted(c, &mut x, null_mut()),
            aegis_client_prediction_stats(c, null_mut()),
        ];
        assert_eq!(null_arg, [AEGIS_ERR_NULL; 13]);
        assert!(aegis_client_join(c, b, n) > 0, "still works");
        aegis_client_free(c);
    }

    /// A panic inside a call is a status, and the client stays refused.
    #[test]
    fn a_panic_is_a_status_and_poisons_the_client() {
        let c = client(b"riw\0", None).unwrap();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let status = with_client(c, |_| panic!("inside"));
        std::panic::set_hook(hook);
        assert_eq!(status, AEGIS_ERR_PANIC);
        let mut buf = [0u8; AEGIS_SEND_MAX];
        assert_eq!(aegis_client_join(c, buf.as_mut_ptr(), buf.len()), AEGIS_ERR_PANIC);
        assert_eq!(aegis_client_player_count(c), AEGIS_ERR_PANIC);
        aegis_client_free(c);
    }

    /// Every event kind lands in the fields aegis.h names for it, and the
    /// fields it does not name are 0.
    #[test]
    fn every_event_maps_to_its_fields() {
        let c = client(b"riw\0", None).unwrap();
        let cases = [
            (EventKind::Hit { shooter: 4, target: 2, damage: 25 }, (AEGIS_EVENT_HIT, 4, 2, 25)),
            (EventKind::Death { player: 2, by: 4 }, (AEGIS_EVENT_DEATH, 2, 4, 0)),
            (EventKind::Join { player: 7 }, (AEGIS_EVENT_JOIN, 7, 0, 0)),
            (EventKind::Leave { player: 9 }, (AEGIS_EVENT_LEAVE, 9, 0, 0)),
        ];
        for (kind, (event, player, other, damage)) in cases {
            let d = encode(&ServerMsg::Event { tick: 5, kind });
            let mut rx = AegisReceived::default();
            assert_eq!(aegis_client_receive(c, d.as_ptr(), d.len(), &mut rx), AEGIS_OK);
            assert_eq!(rx, AegisReceived { tick: 5, kind: AEGIS_RX_EVENT, event, player, other, damage });
        }
        aegis_client_free(c);
    }
}
