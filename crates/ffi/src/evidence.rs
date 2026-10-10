//! The engine's server side of the C ABI: the evidence the Aegis server
//! measures about each shot, over the engine's own world. Each tick the
//! engine passes its players and a line-of-sight callback; each shot
//! returns its evidence, which the engine writes to a monitor
//! (`aegis_monitor_shot`, `aegis_monitor_glimpse`) once it knows whether
//! the shot hit.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use aegis_protocol::{PlayerId, PlayerState, Vec2};
use aegis_server::{Evidence, World};

use crate::client::AegisPlayer;
use crate::{boundary, AEGIS_ERR_ARG, AEGIS_ERR_NULL, AEGIS_ERR_PANIC, AEGIS_OK};

/// An `aegis_evidence_*` call on a handle already inside one: from the
/// `sees` callback. Refused; the outer call goes on.
pub const AEGIS_ERR_BUSY: i32 = -12;

/// Can a player standing at (fx, fy) see one standing at (tx, ty)? Nonzero:
/// yes. Called from inside `aegis_evidence_begin_tick` and
/// `aegis_evidence_joined` only, on the calling thread, with the `ctx` given
/// there. Must return, not unwind or longjmp. An `int32_t`, not a `bool`:
/// any value a foreign caller returns is defined.
pub type AegisSeesFn = extern "C" fn(ctx: *mut c_void, fx: f32, fy: f32, tx: f32, ty: f32) -> i32;

/// What one shot says. `live` false: no evidence at all (the shooter was
/// dead now, or in the picture it fired on). `has_aim`: write
/// `aegis_monitor_shot` with `aim_err`, `react` (negative: not timed) and
/// `size` once the hit is known. `has_glimpse`: write
/// `aegis_monitor_glimpse` with `has_claimed`, `claimed`, `ahead` — before
/// the shot, as the Aegis server does.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AegisShotEvidence {
    pub live: bool,
    pub has_aim: bool,
    pub has_glimpse: bool,
    pub has_claimed: bool,
    pub enemy: u8,
    pub aim_err: f32,
    pub react: i32,
    pub claimed: f32,
    pub ahead: f32,
    /// The enemy's angular radius in radians, `asin(point_blank / distance)`.
    pub size: f32,
}

impl From<aegis_server::ShotEvidence> for AegisShotEvidence {
    fn from(s: aegis_server::ShotEvidence) -> Self {
        let mut out = Self { live: s.live, react: -1, ..Self::default() };
        if let Some(a) = s.aim {
            out.has_aim = true;
            out.aim_err = a.err;
            out.enemy = a.enemy;
            out.react = a.react.map_or(-1, |r| i32::try_from(r).unwrap_or(i32::MAX));
            out.size = a.size;
        }
        if let Some(g) = s.glimpse {
            out.has_glimpse = true;
            out.has_claimed = g.claimed.is_finite();
            out.claimed = if out.has_claimed { g.claimed } else { 0.0 };
            out.ahead = g.ahead;
        }
        out
    }
}

/// Opaque to C. Only ever borrowed shared, so a `sees` callback that calls
/// back in with the same handle aliases nothing: it finds `ev` borrowed and
/// gets AEGIS_ERR_BUSY.
pub struct AegisEvidence {
    ev: RefCell<Evidence>,
    poisoned: Cell<bool>,
}

/// The engine's world for one call: its players, copied, and its callback.
struct CWorld {
    players: Vec<PlayerState>,
    sees: AegisSeesFn,
    ctx: *mut c_void,
}

impl World for CWorld {
    fn players(&self) -> &[PlayerState] {
        &self.players
    }

    fn sees(&self, from: Vec2, to: Vec2) -> bool {
        (self.sees)(self.ctx, from.x, from.y, to.x, to.y) != 0
    }
}

/// Copy `n` players at `players`; `None` if an id repeats.
fn world(players: *const AegisPlayer, n: usize, sees: AegisSeesFn, ctx: *mut c_void) -> Option<CWorld> {
    let ps: &[AegisPlayer] = if n == 0 {
        &[]
    } else {
        // SAFETY: the caller says `players` holds `n` (checked non-null).
        unsafe { std::slice::from_raw_parts(players, n) }
    };
    let mut ids = [false; 256];
    let mut out = Vec::with_capacity(n);
    for p in ps {
        if std::mem::replace(&mut ids[p.id as usize], true) {
            return None;
        }
        out.push(PlayerState { id: p.id, pos: Vec2::new(p.x, p.y), health: p.health, alive: p.alive != 0 });
    }
    Some(CWorld { players: out, sees, ctx })
}

fn with_evidence(e: *mut AegisEvidence, f: impl FnOnce(&mut Evidence) -> i32) -> i32 {
    // SAFETY: from `aegis_evidence_new`, not freed, one thread at a time
    // (aegis.h); null checked. Shared: a re-entrant call holds another.
    let Some(e) = (unsafe { e.as_ref() }) else { return AEGIS_ERR_NULL };
    if e.poisoned.get() {
        return AEGIS_ERR_PANIC;
    }
    let Ok(mut ev) = e.ev.try_borrow_mut() else { return AEGIS_ERR_BUSY };
    let s = boundary(|| f(&mut ev));
    if s == AEGIS_ERR_PANIC {
        e.poisoned.set(true);
    }
    s
}

/// Evidence for a game whose hitbox radius is `point_blank` (in its units:
/// an enemy that close is no evidence of aim); 0 for the lab's. Negative or
/// not finite: AEGIS_ERR_ARG.
#[no_mangle]
pub extern "C" fn aegis_evidence_new(point_blank: f32, out: *mut *mut AegisEvidence) -> i32 {
    boundary(|| {
        if out.is_null() {
            return AEGIS_ERR_NULL;
        }
        if !point_blank.is_finite() || point_blank < 0.0 {
            return AEGIS_ERR_ARG;
        }
        let ev = if point_blank == 0.0 { Evidence::default() } else { Evidence::with_point_blank(point_blank) };
        // SAFETY: null checked above.
        unsafe { *out = Box::into_raw(Box::new(AegisEvidence { ev: RefCell::new(ev), poisoned: Cell::new(false) })) };
        AEGIS_OK
    })
}

/// NULL is a no-op. Never from inside `sees`.
#[no_mangle]
pub extern "C" fn aegis_evidence_free(e: *mut AegisEvidence) {
    if !e.is_null() {
        // SAFETY: from `aegis_evidence_new`, freed once (aegis.h).
        drop(unsafe { Box::from_raw(e) });
    }
}

/// Start of tick `tick`, positions as this tick's snapshots show them:
/// every player (alive or dead, `n` of them, ids unique — else
/// AEGIS_ERR_ARG; `health` is not read), in an order that stays the same
/// from tick to tick.
#[no_mangle]
pub extern "C" fn aegis_evidence_begin_tick(
    e: *mut AegisEvidence,
    tick: u32,
    players: *const AegisPlayer,
    n: usize,
    sees: Option<AegisSeesFn>,
    ctx: *mut c_void,
) -> i32 {
    let Some(sees) = sees else { return AEGIS_ERR_NULL };
    if players.is_null() && n != 0 {
        return AEGIS_ERR_NULL;
    }
    with_evidence(e, |ev| match world(players, n, sees, ctx) {
        Some(w) => {
            ev.begin_tick(tick, &w);
            AEGIS_OK
        }
        None => AEGIS_ERR_ARG,
    })
}

/// Player `id` was admitted mid-tick: `players` is the world with it in.
#[no_mangle]
pub extern "C" fn aegis_evidence_joined(
    e: *mut AegisEvidence,
    tick: u32,
    players: *const AegisPlayer,
    n: usize,
    sees: Option<AegisSeesFn>,
    ctx: *mut c_void,
    id: u8,
) -> i32 {
    let Some(sees) = sees else { return AEGIS_ERR_NULL };
    if players.is_null() && n != 0 {
        return AEGIS_ERR_NULL;
    }
    with_evidence(e, |ev| match world(players, n, sees, ctx) {
        Some(w) => {
            ev.joined(tick, &w, id as PlayerId);
            AEGIS_OK
        }
        None => AEGIS_ERR_ARG,
    })
}

/// A shot by `shooter` at (`aim_x`, `aim_y`), chosen on the snapshot of
/// tick `seen` — proven, at most 16 ticks old, never going back — and
/// resolved on `tick`. `alive`: the shooter is alive now. Once per shot, in
/// the order the game resolves them.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aegis_evidence_shot(
    e: *mut AegisEvidence,
    tick: u32,
    seen: u32,
    shooter: u8,
    alive: bool,
    aim_x: f32,
    aim_y: f32,
    out: *mut AegisShotEvidence,
) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_evidence(e, |ev| {
        let s = ev.shot(tick, seen, shooter, alive, Vec2::new(aim_x, aim_y));
        // SAFETY: null checked above.
        unsafe { *out = s.into() };
        AEGIS_OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;

    /// Sight ends in fog at `*ctx` units.
    extern "C" fn fog(ctx: *mut c_void, fx: f32, fy: f32, tx: f32, ty: f32) -> i32 {
        // SAFETY: every test passes a live f32.
        let r = unsafe { *ctx.cast::<f32>() };
        ((tx - fx).hypot(ty - fy) < r) as i32
    }

    struct Fog(Vec<PlayerState>, f32);

    impl World for Fog {
        fn players(&self) -> &[PlayerState] {
            &self.0
        }
        fn sees(&self, from: Vec2, to: Vec2) -> bool {
            (to.x - from.x).hypot(to.y - from.y) < self.1
        }
    }

    fn xorshift(s: &mut u32) -> u32 {
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        *s
    }

    /// The C path equals the Rust one, field for field, over a random
    /// game: 12 players walking in and out of each other's fog, dying and
    /// coming back, firing on snapshots up to 5 ticks old, one leaving and
    /// a newcomer joining mid-tick.
    #[test]
    fn evidence_through_c_equals_evidence_in_rust() {
        let mut radius = 25.0f32;
        let ctx = (&mut radius as *mut f32).cast::<c_void>();
        let mut e = null_mut();
        assert_eq!(aegis_evidence_new(0.0, &mut e), AEGIS_OK);
        let mut rust = Evidence::default();
        let mut s = 0x9e37_79b9u32;
        let mut ps: Vec<AegisPlayer> = (1..=12)
            .map(|id| AegisPlayer { x: (id * 7 % 50) as f32, y: (id * 13 % 50) as f32, id, health: 100, alive: 1 })
            .collect();
        let state = |ps: &[AegisPlayer]| -> Vec<PlayerState> {
            ps.iter()
                .map(|p| PlayerState { id: p.id, pos: Vec2::new(p.x, p.y), health: p.health, alive: p.alive != 0 })
                .collect()
        };
        let mut compared = (0, 0, 0);
        for tick in 1..=400u32 {
            for p in ps.iter_mut() {
                p.x = (p.x + (xorshift(&mut s) % 5) as f32 - 2.0).clamp(0.0, 60.0);
                p.y = (p.y + (xorshift(&mut s) % 5) as f32 - 2.0).clamp(0.0, 60.0);
                if xorshift(&mut s).is_multiple_of(40) {
                    p.alive ^= 1;
                }
            }
            assert_eq!(aegis_evidence_begin_tick(e, tick, ps.as_ptr(), ps.len(), Some(fog), ctx), AEGIS_OK);
            rust.begin_tick(tick, &Fog(state(&ps), 25.0));
            if tick == 200 {
                ps.retain(|p| p.id != 5);
                ps.push(AegisPlayer { x: 30.0, y: 30.0, id: 5, health: 100, alive: 1 });
                let n = ps.len();
                assert_eq!(aegis_evidence_joined(e, tick, ps.as_ptr(), n, Some(fog), ctx, 5), AEGIS_OK);
                rust.joined(tick, &Fog(state(&ps), 25.0), 5);
            }
            for p in ps.clone() {
                if !xorshift(&mut s).is_multiple_of(3) {
                    continue;
                }
                let seen = tick.saturating_sub(xorshift(&mut s) % 6).max(1);
                let a = (xorshift(&mut s) % 628) as f32 / 100.0;
                let mut c = AegisShotEvidence::default();
                let alive = p.alive != 0;
                assert_eq!(aegis_evidence_shot(e, tick, seen, p.id, alive, a.cos(), a.sin(), &mut c), AEGIS_OK);
                let r = AegisShotEvidence::from(rust.shot(tick, seen, p.id, alive, Vec2::new(a.cos(), a.sin())));
                assert_eq!(c, r, "tick {tick} player {}", p.id);
                compared.0 += c.has_aim as u32;
                compared.1 += (c.react >= 0) as u32;
                compared.2 += c.has_glimpse as u32;
            }
        }
        assert!(compared.0 > 500 && compared.1 > 20 && compared.2 > 20, "{compared:?}");
        aegis_evidence_free(e);
    }

    #[test]
    fn a_repeated_id_or_a_bad_radius_is_refused() {
        let mut e = null_mut();
        assert_eq!(aegis_evidence_new(-1.0, &mut e), AEGIS_ERR_ARG);
        assert_eq!(aegis_evidence_new(f32::NAN, &mut e), AEGIS_ERR_ARG);
        assert_eq!(aegis_evidence_new(0.5, &mut e), AEGIS_OK);
        let p = AegisPlayer { id: 3, alive: 1, ..Default::default() };
        let mut r = 1.0f32;
        let ctx = (&mut r as *mut f32).cast();
        assert_eq!(aegis_evidence_begin_tick(e, 1, [p, p].as_ptr(), 2, Some(fog), ctx), AEGIS_ERR_ARG);
        assert_eq!(aegis_evidence_begin_tick(e, 1, [p].as_ptr(), 1, None, ctx), AEGIS_ERR_NULL);
        assert_eq!(aegis_evidence_begin_tick(e, 1, std::ptr::null(), 0, Some(fog), ctx), AEGIS_OK, "an empty world");
        aegis_evidence_free(e);
    }

    struct Reenter {
        e: *mut AegisEvidence,
        got: Vec<i32>,
    }

    /// A callback that calls back in on the handle it is called from.
    extern "C" fn reenter(ctx: *mut c_void, _: f32, _: f32, _: f32, _: f32) -> i32 {
        // SAFETY: the test passes a live Reenter.
        let r = unsafe { &mut *ctx.cast::<Reenter>() };
        let mut out = AegisShotEvidence::default();
        r.got.push(aegis_evidence_begin_tick(r.e, 9, std::ptr::null(), 0, Some(reenter), ctx));
        r.got.push(aegis_evidence_shot(r.e, 9, 9, 1, true, 1.0, 0.0, &mut out));
        1
    }

    /// From inside `sees`, the same handle is refused, never aliased; the
    /// outer call finishes and the handle goes on working.
    #[test]
    fn a_callback_calling_back_in_is_refused_and_the_handle_survives() {
        let mut e = null_mut();
        assert_eq!(aegis_evidence_new(0.0, &mut e), AEGIS_OK);
        let ps = [
            AegisPlayer { x: 0.0, y: 0.0, id: 1, health: 100, alive: 1 },
            AegisPlayer { x: 9.0, y: 0.0, id: 2, health: 100, alive: 1 },
        ];
        let mut r = Reenter { e, got: Vec::new() };
        let ctx = (&mut r as *mut Reenter).cast();
        assert_eq!(aegis_evidence_begin_tick(e, 1, ps.as_ptr(), 2, Some(reenter), ctx), AEGIS_OK);
        assert!(!r.got.is_empty() && r.got.iter().all(|&s| s == AEGIS_ERR_BUSY), "{:?}", r.got);
        let mut radius = 25.0f32;
        let ctx = (&mut radius as *mut f32).cast();
        assert_eq!(aegis_evidence_begin_tick(e, 2, ps.as_ptr(), 2, Some(fog), ctx), AEGIS_OK);
        let mut out = AegisShotEvidence::default();
        assert_eq!(aegis_evidence_shot(e, 2, 2, 1, true, 1.0, 0.0, &mut out), AEGIS_OK);
        assert!(out.live && out.has_aim && out.enemy == 2, "{out:?}");
        aegis_evidence_free(e);
    }

    /// `alive` and the callback's answer are any nonzero value, as a C
    /// caller (or a C# one, whose BOOL is 4 bytes) may hand over.
    #[test]
    fn any_nonzero_is_alive_and_any_nonzero_sees() {
        extern "C" fn sees_by_seven(_: *mut c_void, _: f32, _: f32, _: f32, _: f32) -> i32 {
            7
        }
        let shot = |alive: u8| {
            let mut e = null_mut();
            assert_eq!(aegis_evidence_new(0.0, &mut e), AEGIS_OK);
            let ps = [
                AegisPlayer { x: 0.0, y: 0.0, id: 1, health: 100, alive },
                AegisPlayer { x: 9.0, y: 0.0, id: 2, health: 100, alive: 1 },
            ];
            assert_eq!(aegis_evidence_begin_tick(e, 1, ps.as_ptr(), 2, Some(sees_by_seven), null_mut()), AEGIS_OK);
            let mut out = AegisShotEvidence::default();
            assert_eq!(aegis_evidence_shot(e, 1, 1, 1, true, 1.0, 0.0, &mut out), AEGIS_OK);
            aegis_evidence_free(e);
            out
        };
        assert_eq!(shot(2), shot(1));
        assert!(shot(255).live && shot(255).has_aim);
        assert!(!shot(0).live, "0 is dead");
    }
}
