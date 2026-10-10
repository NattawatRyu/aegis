//! Aegis C ABI — the online detector for engines not written in Rust.
//!
//! The detector reads telemetry and nothing else (`aegis_detector`), so an
//! engine with its own simulation can use it today: it writes the same five
//! records the Aegis server writes — an input accepted, rejected, a shot, a
//! glimpse, a player gone — and reads alerts back. Nothing here touches the
//! sim, the protocol or a socket.
//!
//! The header is `include/aegis.h`, written by hand and held to this file by
//! a test that compiles the C example (`examples/monitor.c`) with the
//! platform's C compiler, links it against this library and runs it: every
//! struct's size and every field's offset is printed by C and compared with
//! Rust's.
//!
//! Rules every function keeps, so no Rust behaviour crosses into C:
//!   - status is an `int32_t`: `AEGIS_OK` (0) or a negative `AEGIS_ERR_*`;
//!     functions that count return the count (>= 0) instead of `AEGIS_OK`;
//!   - a null pointer where one is required is `AEGIS_ERR_NULL`, never a
//!     dereference;
//!   - a panic is caught at the boundary (`AEGIS_ERR_PANIC`) and the monitor
//!     it happened in refuses every later call with the same code: its state
//!     is no longer known to be whole, and a verdict from it would be a
//!     guess;
//!   - nothing allocated here is freed by C's `free`, and nothing C
//!     allocated is freed here.
//!
//! Every function here is the C ABI and its contract on pointers is
//! `aegis.h`'s (valid, or null where null is allowed). They are not marked
//! `unsafe` because C has no such marker to read; a Rust caller uses the
//! Rust crates (`aegis-detector`, `aegis-server`, `aegis-client-sdk`)
//! instead.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::VecDeque;
use std::ffi::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

use aegis_detector::detectors::{accuracy, aim_exact, anomaly_rate, foresight, reaction};
use aegis_detector::{Alert, Config, FlagReason, Monitor, PlayerStats};
use aegis_telemetry::{Outcome, Record};

pub mod client;
pub use client::{AegisClient, AegisPlayer, AegisReceived};

pub mod evidence;
pub use evidence::{AegisEvidence, AegisSeesFn, AegisShotEvidence, AEGIS_ERR_BUSY};

#[cfg(feature = "noise")]
pub mod noise;

/// Bumped whenever a signature or a struct in `aegis.h` changes.
pub const AEGIS_ABI_VERSION: u32 = 1;

pub const AEGIS_OK: i32 = 0;
/// A required pointer was null.
pub const AEGIS_ERR_NULL: i32 = -1;
/// The config was refused (`aegis_config_validate` says why).
pub const AEGIS_ERR_CONFIG: i32 = -2;
/// A panic was caught; the monitor it happened in is unusable from now on.
pub const AEGIS_ERR_PANIC: i32 = -3;
/// No live session for that player.
pub const AEGIS_ERR_NO_SESSION: i32 = -4;
/// An argument out of its range (named in each function's doc).
pub const AEGIS_ERR_ARG: i32 = -5;

/// [`FlagReason`] as a stable number. Never renumbered: a reason that goes
/// away leaves its number unused.
pub const AEGIS_REASON_ACCURACY: u8 = 0;
pub const AEGIS_REASON_AIM_EXACT: u8 = 1;
pub const AEGIS_REASON_ANOMALY_RATE: u8 = 2;
pub const AEGIS_REASON_REACTION: u8 = 3;
pub const AEGIS_REASON_FORESIGHT: u8 = 4;

fn reason_code(r: FlagReason) -> u8 {
    match r {
        FlagReason::Accuracy => AEGIS_REASON_ACCURACY,
        FlagReason::AimExact => AEGIS_REASON_AIM_EXACT,
        FlagReason::AnomalyRate => AEGIS_REASON_ANOMALY_RATE,
        FlagReason::Reaction => AEGIS_REASON_REACTION,
        FlagReason::Foresight => AEGIS_REASON_FORESIGHT,
    }
}

/// Every line of [`Config`], flat. Angles in radians; `fast_ticks` in the
/// game's ticks; shares in [0, 1).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AegisConfig {
    pub accuracy_min_shots: u32,
    pub accuracy_threshold: f32,
    pub aim_exact_rad: f32,
    pub aim_exact_min_shots: u32,
    pub aim_exact_threshold: f32,
    pub anomaly_min_inputs: u32,
    pub anomaly_threshold: f32,
    pub reaction_fast_ticks: u32,
    pub reaction_min_timed: u32,
    pub reaction_threshold: f32,
    pub foresight_fit_rad: f32,
    pub foresight_clear_rad: f32,
    pub foresight_min_foreseen: u32,
    pub foresight_threshold: f32,
}

impl From<Config> for AegisConfig {
    fn from(c: Config) -> Self {
        Self {
            accuracy_min_shots: c.accuracy.min_shots,
            accuracy_threshold: c.accuracy.threshold,
            aim_exact_rad: c.aim_exact.exact_rad,
            aim_exact_min_shots: c.aim_exact.min_shots,
            aim_exact_threshold: c.aim_exact.threshold,
            anomaly_min_inputs: c.anomaly_rate.min_inputs,
            anomaly_threshold: c.anomaly_rate.threshold,
            reaction_fast_ticks: c.reaction.fast_ticks,
            reaction_min_timed: c.reaction.min_timed,
            reaction_threshold: c.reaction.threshold,
            foresight_fit_rad: c.foresight.fit_rad,
            foresight_clear_rad: c.foresight.clear_rad,
            foresight_min_foreseen: c.foresight.min_foreseen,
            foresight_threshold: c.foresight.threshold,
        }
    }
}

impl From<AegisConfig> for Config {
    fn from(c: AegisConfig) -> Self {
        Self {
            accuracy: accuracy::Config { min_shots: c.accuracy_min_shots, threshold: c.accuracy_threshold },
            aim_exact: aim_exact::Config {
                exact_rad: c.aim_exact_rad,
                min_shots: c.aim_exact_min_shots,
                threshold: c.aim_exact_threshold,
            },
            anomaly_rate: anomaly_rate::Config { min_inputs: c.anomaly_min_inputs, threshold: c.anomaly_threshold },
            reaction: reaction::Config {
                fast_ticks: c.reaction_fast_ticks,
                min_timed: c.reaction_min_timed,
                threshold: c.reaction_threshold,
            },
            foresight: foresight::Config {
                fit_rad: c.foresight_fit_rad,
                clear_rad: c.foresight_clear_rad,
                min_foreseen: c.foresight_min_foreseen,
                threshold: c.foresight_threshold,
            },
        }
    }
}

/// One alert: the record's tick, the player, the detector, the measured
/// value, the line it crossed and the samples it was measured over.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AegisAlert {
    pub tick: u32,
    pub player: u8,
    /// An `AEGIS_REASON_*`.
    pub reason: u8,
    pub value: f32,
    pub threshold: f32,
    pub samples: u32,
}

impl From<&Alert> for AegisAlert {
    fn from(a: &Alert) -> Self {
        Self {
            tick: a.tick,
            player: a.flag.player,
            reason: reason_code(a.flag.reason),
            value: a.flag.value,
            threshold: a.flag.threshold,
            samples: a.flag.samples,
        }
    }
}

/// A player's counts ([`PlayerStats`]).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AegisStats {
    pub accepted: u32,
    pub anomalies: u32,
    pub shots: u32,
    pub hits: u32,
    pub exact: u32,
    pub timed: u32,
    pub fast: u32,
    pub glimpsed: u32,
    pub foreseen: u32,
}

impl From<&PlayerStats> for AegisStats {
    fn from(s: &PlayerStats) -> Self {
        Self {
            accepted: s.accepted,
            anomalies: s.anomalies,
            shots: s.shots,
            hits: s.hits,
            exact: s.exact,
            timed: s.timed,
            fast: s.fast,
            glimpsed: s.glimpsed,
            foreseen: s.foreseen,
        }
    }
}

/// Opaque to C. Alerts wait in `pending` until polled, so none is lost to
/// a buffer too small.
pub struct AegisMonitor {
    monitor: Monitor,
    pending: VecDeque<Alert>,
    poisoned: bool,
}

/// Run `f` with panics caught. A panic is `AEGIS_ERR_PANIC`, never an
/// unwind into C.
fn boundary(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(AEGIS_ERR_PANIC)
}

/// Run `f` on a live monitor; a panic poisons it.
fn with_monitor(m: *mut AegisMonitor, f: impl FnOnce(&mut AegisMonitor) -> i32) -> i32 {
    // SAFETY: the caller passes a pointer from `aegis_monitor_new`, not yet
    // freed, used by one thread at a time (aegis.h); null is checked.
    let Some(m) = (unsafe { m.as_mut() }) else { return AEGIS_ERR_NULL };
    if m.poisoned {
        return AEGIS_ERR_PANIC;
    }
    let status = boundary(|| f(m));
    if status == AEGIS_ERR_PANIC {
        m.poisoned = true;
    }
    status
}

/// Feed one record; the number of alerts it raised.
fn observe(m: *mut AegisMonitor, tick: u32, player: u8, outcome: Outcome) -> i32 {
    with_monitor(m, |m| {
        let alerts = m.monitor.observe(&Record { tick, player, outcome });
        let n = alerts.len() as i32;
        m.pending.extend(alerts);
        n
    })
}

/// Write `s` into `buf` (capacity `len`), cut to fit, NUL-terminated.
/// Nothing is written when `buf` is null or `len` is 0.
fn write_message(s: &str, buf: *mut c_char, len: usize) {
    if buf.is_null() || len == 0 {
        return;
    }
    let n = s.len().min(len - 1);
    // SAFETY: the caller says `buf` holds `len` bytes; n + 1 <= len.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr().cast::<c_char>(), buf, n);
        *buf.add(n) = 0;
    }
}

#[no_mangle]
pub extern "C" fn aegis_abi_version() -> u32 {
    AEGIS_ABI_VERSION
}

/// The lab's defaults (30 Hz).
#[no_mangle]
pub extern "C" fn aegis_config_default(out: *mut AegisConfig) -> i32 {
    boundary(|| {
        // SAFETY: null checked; the caller owns `out`.
        let Some(out) = (unsafe { out.as_mut() }) else { return AEGIS_ERR_NULL };
        *out = Config::DEFAULT.into();
        AEGIS_OK
    })
}

/// The defaults for a server ticking `hz` times a second. `hz` 0 is
/// `AEGIS_ERR_ARG`.
#[no_mangle]
pub extern "C" fn aegis_config_at_tick_rate(hz: u32, out: *mut AegisConfig) -> i32 {
    boundary(|| {
        // SAFETY: null checked; the caller owns `out`.
        let Some(out) = (unsafe { out.as_mut() }) else { return AEGIS_ERR_NULL };
        if hz == 0 {
            return AEGIS_ERR_ARG;
        }
        *out = Config::at_tick_rate(hz).into();
        AEGIS_OK
    })
}

/// `AEGIS_OK`, or `AEGIS_ERR_CONFIG` with "field: problem" written to `msg`
/// (capacity `len`, cut to fit, NUL-terminated; `msg` may be null).
#[no_mangle]
pub extern "C" fn aegis_config_validate(cfg: *const AegisConfig, msg: *mut c_char, len: usize) -> i32 {
    boundary(|| {
        // SAFETY: null checked; read only.
        let Some(cfg) = (unsafe { cfg.as_ref() }) else { return AEGIS_ERR_NULL };
        match Config::from(*cfg).validate() {
            Ok(()) => {
                write_message("", msg, len);
                AEGIS_OK
            }
            Err(e) => {
                write_message(&e.to_string(), msg, len);
                AEGIS_ERR_CONFIG
            }
        }
    })
}

/// A monitor with the standard detectors under `cfg` (null: the defaults).
/// On `AEGIS_OK`, `*out` is a handle for `aegis_monitor_free`; otherwise
/// `*out` is left alone.
#[no_mangle]
pub extern "C" fn aegis_monitor_new(cfg: *const AegisConfig, out: *mut *mut AegisMonitor) -> i32 {
    boundary(|| {
        if out.is_null() {
            return AEGIS_ERR_NULL;
        }
        // SAFETY: read only; null means the defaults.
        let cfg = unsafe { cfg.as_ref() }.map_or(Config::DEFAULT, |c| Config::from(*c));
        let Ok(monitor) = Monitor::with_config(cfg) else { return AEGIS_ERR_CONFIG };
        let m = Box::new(AegisMonitor { monitor, pending: VecDeque::new(), poisoned: false });
        // SAFETY: null checked above.
        unsafe { *out = Box::into_raw(m) };
        AEGIS_OK
    })
}

/// Free a monitor. Null is a no-op; freeing twice is undefined, as `free`.
#[no_mangle]
pub extern "C" fn aegis_monitor_free(m: *mut AegisMonitor) {
    if !m.is_null() {
        // SAFETY: from `aegis_monitor_new`, freed once (aegis.h). Dropping
        // a Monitor does not panic, and if it did the abort is the right
        // end for a process whose allocator state is unknown.
        drop(unsafe { Box::from_raw(m) });
    }
}

/// An input reached the game's simulation. `anomaly`: it passed, but a
/// check found it suspicious (a clamped move, say). Returns the alerts it
/// raised (>= 0).
#[no_mangle]
pub extern "C" fn aegis_monitor_accepted(m: *mut AegisMonitor, tick: u32, player: u8, anomaly: bool) -> i32 {
    observe(m, tick, player, Outcome::Accepted { anomaly })
}

/// An input was dropped by a check. Counts nothing; starts the player's
/// session if it had none.
#[no_mangle]
pub extern "C" fn aegis_monitor_rejected(m: *mut AegisMonitor, tick: u32, player: u8) -> i32 {
    observe(m, tick, player, Outcome::Rejected { reason: "ffi" })
}

/// A shot, resolved by the game. `aim_err`: radians between the aim and the
/// bearing to the nearest enemy, from the server's positions. `react`: ticks
/// from that enemy coming into sight to this shot, on the first shot of an
/// engagement that was not prefire; negative when the shot is not timed.
#[no_mangle]
pub extern "C" fn aegis_monitor_shot(
    m: *mut AegisMonitor,
    tick: u32,
    player: u8,
    hit: bool,
    aim_err: f32,
    react: i32,
) -> i32 {
    let react = u32::try_from(react).ok();
    observe(m, tick, player, Outcome::Shot { hit, aim_err, react })
}

/// A shot whose aim can be held against an enemy only a snapshot newer than
/// the one the input claims had shown. `ahead`: radians to the nearest such
/// enemy. `claimed`: radians to the nearest enemy the claimed snapshot
/// showed, read only when `has_claimed`.
#[no_mangle]
pub extern "C" fn aegis_monitor_glimpse(
    m: *mut AegisMonitor,
    tick: u32,
    player: u8,
    has_claimed: bool,
    claimed: f32,
    ahead: f32,
) -> i32 {
    let claimed = has_claimed.then_some(claimed);
    observe(m, tick, player, Outcome::Glimpse { claimed, ahead })
}

/// The player's session ended; whoever gets its id next starts from zero.
#[no_mangle]
pub extern "C" fn aegis_monitor_left(m: *mut AegisMonitor, tick: u32, player: u8) -> i32 {
    observe(m, tick, player, Outcome::Left)
}

/// Take the oldest unread alert: 1 and `*out` written, or 0 when none is
/// waiting.
#[no_mangle]
pub extern "C" fn aegis_monitor_poll(m: *mut AegisMonitor, out: *mut AegisAlert) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_monitor(m, |m| match m.pending.pop_front() {
        Some(a) => {
            // SAFETY: null checked above; the caller owns `out`.
            unsafe { *out = (&a).into() };
            1
        }
        None => 0,
    })
}

/// A live player's counts: over its whole session (`window` false) or over
/// the monitor's last 100 samples of each kind (`window` true).
#[no_mangle]
pub extern "C" fn aegis_monitor_stats(m: *mut AegisMonitor, player: u8, window: bool, out: *mut AegisStats) -> i32 {
    if out.is_null() {
        return AEGIS_ERR_NULL;
    }
    with_monitor(m, |m| {
        let s = if window { m.monitor.window(player) } else { m.monitor.stats(player) };
        match s {
            Some(s) => {
                // SAFETY: null checked above; the caller owns `out`.
                unsafe { *out = s.into() };
                AEGIS_OK
            }
            None => AEGIS_ERR_NO_SESSION,
        }
    })
}

/// A static, NUL-terminated name for an `AEGIS_REASON_*` ("unknown" for any
/// other number). Never freed.
#[no_mangle]
pub extern "C" fn aegis_reason_label(reason: u8) -> *const c_char {
    let s: &'static [u8] = match reason {
        AEGIS_REASON_ACCURACY => b"accuracy\0",
        AEGIS_REASON_AIM_EXACT => b"aim_exact\0",
        AEGIS_REASON_ANOMALY_RATE => b"anomaly_rate\0",
        AEGIS_REASON_REACTION => b"reaction\0",
        AEGIS_REASON_FORESIGHT => b"foresight\0",
        _ => b"unknown\0",
    };
    s.as_ptr().cast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::ptr::{null, null_mut};

    fn new(cfg: Option<&AegisConfig>) -> *mut AegisMonitor {
        let mut m = null_mut();
        assert_eq!(aegis_monitor_new(cfg.map_or(null(), |c| c), &mut m), AEGIS_OK);
        m
    }

    #[test]
    fn config_roundtrips_and_the_labels_match_the_detector() {
        let mut c = AegisConfig::from(Config::DEFAULT);
        assert_eq!(aegis_config_default(&mut c), AEGIS_OK);
        assert_eq!(Config::from(c), Config::DEFAULT);
        assert_eq!(aegis_config_at_tick_rate(60, &mut c), AEGIS_OK);
        assert_eq!(Config::from(c), Config::at_tick_rate(60));
        assert_eq!(aegis_config_at_tick_rate(0, &mut c), AEGIS_ERR_ARG);
        for r in [FlagReason::Accuracy].into_iter().chain(FlagReason::STANDARD) {
            // SAFETY: a static NUL-terminated string.
            let label = unsafe { CStr::from_ptr(aegis_reason_label(reason_code(r))) };
            assert_eq!(label.to_str().unwrap(), r.label());
        }
        // SAFETY: as above.
        assert_eq!(unsafe { CStr::from_ptr(aegis_reason_label(5)) }.to_str().unwrap(), "unknown");
    }

    /// The refusal reaches C with its reason, cut to the buffer at its edge:
    /// exactly the message's length leaves room for all but the last byte.
    #[test]
    fn a_refused_config_says_why_into_any_buffer() {
        let mut c = AegisConfig::from(Config::DEFAULT);
        c.foresight_clear_rad = c.foresight_fit_rad;
        let full = Config::from(c).validate().unwrap_err().to_string();
        let mut buf = [0x7f as c_char; 128];
        assert_eq!(aegis_config_validate(&c, buf.as_mut_ptr(), buf.len()), AEGIS_ERR_CONFIG);
        // SAFETY: NUL-terminated by the call.
        assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(), full);
        let mut buf = vec![0x7f as c_char; full.len()];
        assert_eq!(aegis_config_validate(&c, buf.as_mut_ptr(), buf.len()), AEGIS_ERR_CONFIG);
        // SAFETY: as above.
        assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(), &full[..full.len() - 1]);
        assert_eq!(aegis_config_validate(&c, null_mut(), 0), AEGIS_ERR_CONFIG, "no buffer is fine");
        let mut m = null_mut();
        assert_eq!(aegis_monitor_new(&c, &mut m), AEGIS_ERR_CONFIG);
        assert!(m.is_null(), "out untouched on refusal");
    }

    #[test]
    fn nulls_are_refused_not_dereferenced() {
        assert_eq!(aegis_config_default(null_mut()), AEGIS_ERR_NULL);
        assert_eq!(aegis_config_validate(null(), null_mut(), 0), AEGIS_ERR_NULL);
        assert_eq!(aegis_monitor_new(null(), null_mut()), AEGIS_ERR_NULL);
        assert_eq!(aegis_monitor_accepted(null_mut(), 1, 1, false), AEGIS_ERR_NULL);
        assert_eq!(aegis_monitor_poll(null_mut(), &mut AegisAlert::from(&alert())), AEGIS_ERR_NULL);
        let m = new(None);
        assert_eq!(aegis_monitor_poll(m, null_mut()), AEGIS_ERR_NULL);
        assert_eq!(aegis_monitor_stats(m, 1, false, null_mut()), AEGIS_ERR_NULL);
        aegis_monitor_free(m);
        aegis_monitor_free(null_mut());
    }

    fn alert() -> Alert {
        Alert {
            tick: 0,
            flag: aegis_detector::Flag {
                player: 0,
                reason: FlagReason::Reaction,
                value: 0.0,
                threshold: 0.0,
                samples: 0,
            },
        }
    }

    /// The C path and the Rust monitor reach the same alerts on the same
    /// stream: a triggerbot's instant shots, then its session ends and the
    /// id starts over.
    #[test]
    fn alerts_through_c_equal_the_monitors() {
        let m = new(None);
        let mut rust = Monitor::standard();
        let mut want = Vec::new();
        for t in 0..40u32 {
            let rec =
                Record { tick: t, player: 3, outcome: Outcome::Shot { hit: true, aim_err: 0.01, react: Some(1) } };
            want.extend(rust.observe(&rec));
            assert!(aegis_monitor_shot(m, t, 3, true, 0.01, 1) >= 0);
        }
        let mut got = Vec::new();
        let mut a = AegisAlert::from(&alert());
        while aegis_monitor_poll(m, &mut a) == 1 {
            got.push(a);
        }
        assert!(!want.is_empty());
        assert_eq!(got, want.iter().map(AegisAlert::from).collect::<Vec<_>>());
        let mut s = AegisStats::default();
        assert_eq!(aegis_monitor_stats(m, 3, false, &mut s), AEGIS_OK);
        assert_eq!(s, rust.stats(3).unwrap().into());
        assert_eq!(aegis_monitor_left(m, 40, 3), 0);
        assert_eq!(aegis_monitor_stats(m, 3, false, &mut s), AEGIS_ERR_NO_SESSION);
        aegis_monitor_free(m);
    }

    /// A shot's negative react is "not timed"; 0 is timed and fast.
    #[test]
    fn react_below_zero_is_untimed_zero_is_timed() {
        let m = new(None);
        aegis_monitor_shot(m, 1, 1, false, 1.0, -1);
        aegis_monitor_shot(m, 2, 1, false, 1.0, 0);
        let mut s = AegisStats::default();
        aegis_monitor_stats(m, 1, false, &mut s);
        assert_eq!((s.shots, s.timed, s.fast), (2, 1, 1));
        aegis_monitor_glimpse(m, 3, 1, false, 0.0, 0.0);
        aegis_monitor_glimpse(m, 4, 1, true, 0.0, 0.0);
        aegis_monitor_stats(m, 1, true, &mut s);
        assert_eq!((s.glimpsed, s.foreseen), (2, 1), "has_claimed false is None, not 0.0");
        aegis_monitor_free(m);
    }

    /// A panic inside a call is a status, and the monitor stays refused.
    #[test]
    fn a_panic_is_a_status_and_poisons_the_monitor() {
        let m = new(None);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let status = with_monitor(m, |_| panic!("inside"));
        std::panic::set_hook(hook);
        assert_eq!(status, AEGIS_ERR_PANIC);
        assert_eq!(aegis_monitor_accepted(m, 1, 1, false), AEGIS_ERR_PANIC);
        aegis_monitor_free(m);
    }
}
