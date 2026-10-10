//! What every foreign-language check of the ABI is compared with: Rust's own
//! sizes, field offsets and constants, as `size` / `offset` / `const` lines.

#![allow(dead_code)]

use std::mem::{offset_of, size_of};
use std::path::PathBuf;

use aegis_ffi::client::*;
use aegis_ffi::*;

/// `target/<profile>/deps`, beside this test binary: where cargo put the
/// cdylib it built for this test (only a plain `cargo build` copies it up a
/// level).
pub fn deps_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().to_path_buf()
}

/// The `size`, `offset` and `const` lines a check printed.
pub fn layout_lines(stdout: &str) -> Vec<&str> {
    stdout.lines().filter(|l| ["size ", "offset ", "const "].iter().any(|p| l.starts_with(p))).collect()
}

macro_rules! layout {
    ($want:ident, $t:ty, $($f:ident),+) => {
        $want.push(format!("size {} {}", stringify!($t), size_of::<$t>()));
        $($want.push(format!("offset {}.{} {}", stringify!($t), stringify!($f), offset_of!($t, $f)));)+
    };
}

macro_rules! consts {
    ($want:ident, $($c:ident),+) => {
        $($want.push(format!("const {} {}", stringify!($c), i64::from($c)));)+
    };
}

pub fn monitor_layout() -> Vec<String> {
    let mut want = Vec::new();
    layout!(
        want,
        AegisConfig,
        accuracy_min_shots,
        accuracy_threshold,
        aim_exact_rad,
        aim_exact_min_shots,
        aim_exact_threshold,
        anomaly_min_inputs,
        anomaly_threshold,
        reaction_fast_ticks,
        reaction_min_timed,
        reaction_threshold,
        foresight_fit_rad,
        foresight_clear_rad,
        foresight_min_foreseen,
        foresight_threshold
    );
    layout!(want, AegisAlert, tick, player, reason, value, threshold, samples);
    layout!(want, AegisStats, accepted, anomalies, shots, hits, exact, timed, fast, glimpsed, foreseen);
    want
}

pub fn client_layout_and_consts() -> Vec<String> {
    let mut want = Vec::new();
    layout!(want, AegisReceived, tick, kind, event, player, other, damage);
    layout!(want, AegisPlayer, x, y, id, health, alive);
    consts!(
        want,
        AEGIS_ABI_VERSION,
        AEGIS_OK,
        AEGIS_ERR_NULL,
        AEGIS_ERR_CONFIG,
        AEGIS_ERR_PANIC,
        AEGIS_ERR_NO_SESSION,
        AEGIS_ERR_ARG,
        AEGIS_ERR_BUFFER,
        AEGIS_ERR_NOT_JOINED,
        AEGIS_ERR_UNKNOWN_TICK,
        AEGIS_ERR_TICK_REGRESSED,
        AEGIS_ERR_BAD_SEAL,
        AEGIS_ERR_MALFORMED,
        AEGIS_ERR_BUSY,
        AEGIS_REASON_ACCURACY,
        AEGIS_REASON_AIM_EXACT,
        AEGIS_REASON_ANOMALY_RATE,
        AEGIS_REASON_REACTION,
        AEGIS_REASON_FORESIGHT
    );
    want.push(format!("const AEGIS_CLIENT_KEYS_LEN {AEGIS_CLIENT_KEYS_LEN}"));
    want.push(format!("const AEGIS_NAME_MAX {AEGIS_NAME_MAX}"));
    want.push(format!("const AEGIS_SEND_MAX {AEGIS_SEND_MAX}"));
    consts!(
        want,
        AEGIS_TICK_NEWEST,
        AEGIS_RX_JOINED,
        AEGIS_RX_CHALLENGE,
        AEGIS_RX_SNAPSHOT,
        AEGIS_RX_EVENT,
        AEGIS_EVENT_HIT,
        AEGIS_EVENT_DEATH,
        AEGIS_EVENT_JOIN,
        AEGIS_EVENT_LEAVE
    );
    want
}

pub fn evidence_layout() -> Vec<String> {
    let mut want = Vec::new();
    layout!(want, AegisShotEvidence, live, has_aim, has_glimpse, has_claimed, enemy, aim_err, react, claimed, ahead);
    want
}

#[cfg(feature = "noise")]
pub fn noise_consts() -> Vec<String> {
    use aegis_ffi::noise::*;
    let mut want = vec![
        format!("const AEGIS_NOISE_PUBLIC_LEN {AEGIS_NOISE_PUBLIC_LEN}"),
        format!("const AEGIS_NOISE_HELLO_LEN {AEGIS_NOISE_HELLO_LEN}"),
    ];
    consts!(want, AEGIS_NOISE_CHALLENGE, AEGIS_NOISE_WELCOME);
    want
}
