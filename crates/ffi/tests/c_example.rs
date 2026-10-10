//! The header is written by hand, so it is held to the library by the only
//! judge that counts: a C compiler. Each example in `examples/` is built
//! with the platform's compiler against `include/aegis.h`, linked to the
//! cdylib cargo built for this test, and run. It must exit 0 (its own
//! checks), and every size, field offset and constant it prints must equal
//! Rust's. A function declared but not exported fails the link.

use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::process::Command;

use aegis_ffi::client::*;
use aegis_ffi::*;

/// `target/<profile>/deps`, beside this test binary: where cargo put the
/// cdylib it built for this test (only a plain `cargo build` copies it up a
/// level).
fn deps_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().to_path_buf()
}

fn target_triple() -> &'static str {
    if cfg!(all(windows, target_env = "msvc", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else {
        panic!("add this platform's triple")
    }
}

/// Build `examples/<name>.c`, run it with `layout`; its stdout.
fn build_and_run(name: &str) -> String {
    let lib_dir = deps_dir();
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ffi-c").join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let triple = target_triple();
    let tool = cc::Build::new()
        .target(triple)
        .host(triple)
        .opt_level(0)
        .cargo_metadata(false)
        .try_get_compiler()
        .expect("a C compiler for the test of the C ABI");
    let src = crate_dir.join("examples").join(format!("{name}.c"));
    let include = crate_dir.join("include");
    let exe = out_dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() });
    let mut cmd = tool.to_command();
    if tool.is_like_msvc() {
        cmd.arg("/nologo").arg("/W4").arg("/WX").arg(format!("/I{}", include.display())).arg(&src);
        cmd.arg(format!("/Fo{}\\", out_dir.display())).arg(format!("/Fe{}", exe.display()));
        cmd.arg("/link").arg(lib_dir.join("aegis_ffi.dll.lib"));
    } else {
        cmd.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]).arg("-I").arg(&include).arg(&src);
        cmd.arg("-o").arg(&exe).arg("-L").arg(&lib_dir).arg("-laegis_ffi");
        cmd.arg(format!("-Wl,-rpath,{}", lib_dir.display())).arg("-lm");
    }
    let out = cmd.output().expect("run the C compiler");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    assert!(out.status.success(), "C build failed:\n{}{}", text(&out.stdout), text(&out.stderr));

    let mut run = Command::new(&exe);
    run.arg("layout");
    if cfg!(windows) {
        // The DLL is found on PATH.
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs = std::iter::once(lib_dir).chain(std::env::split_paths(&path));
        run.env("PATH", std::env::join_paths(dirs).unwrap());
    }
    let out = run.output().expect("run the C example");
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{name} failed:\n{stdout}{}", text(&out.stderr));
    assert!(stdout.lines().any(|l| l == "ok"), "{stdout}");
    stdout
}

/// The `size`, `offset` and `const` lines a C example printed.
fn layout_lines(stdout: &str) -> Vec<&str> {
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

/// An engine's own world, through the evidence and into the monitor: the
/// player that fires the tick an enemy appears is flagged by reaction, the
/// one 15 ticks later by nothing.
#[test]
fn the_engine_example_builds_runs_and_agrees_on_every_layout() {
    let stdout = build_and_run("engine");
    assert!(stdout.contains("player=2 reaction"), "{stdout}");
    assert!(stdout.contains("player 2: 30 shots, 30 timed, 30 fast"), "{stdout}");
    let mut want = Vec::new();
    layout!(want, AegisShotEvidence, live, has_aim, has_glimpse, has_claimed, enemy, aim_err, react, claimed, ahead);
    assert_eq!(layout_lines(&stdout), want);
}

/// The Noise section of the header, linked against a library built with
/// feature `noise` (a declared function it does not export fails the link).
#[cfg(feature = "noise")]
#[test]
fn the_noise_example_builds_runs_and_agrees_on_every_constant() {
    use aegis_ffi::noise::*;
    let stdout = build_and_run("noise");
    let mut want = vec![
        format!("const AEGIS_NOISE_PUBLIC_LEN {AEGIS_NOISE_PUBLIC_LEN}"),
        format!("const AEGIS_NOISE_HELLO_LEN {AEGIS_NOISE_HELLO_LEN}"),
    ];
    consts!(want, AEGIS_NOISE_CHALLENGE, AEGIS_NOISE_WELCOME);
    assert_eq!(layout_lines(&stdout), want);
}

#[test]
fn the_monitor_example_builds_runs_and_agrees_on_every_layout() {
    let stdout = build_and_run("monitor");
    assert!(stdout.contains("refused: foresight.clear_rad"), "{stdout}");
    assert!(stdout.contains("player=2 reaction"), "the instant player is flagged by reaction:\n{stdout}");
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
    assert_eq!(layout_lines(&stdout), want);
}

#[test]
fn the_client_example_builds_runs_and_agrees_on_every_layout_and_constant() {
    let stdout = build_and_run("client");
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
    assert_eq!(layout_lines(&stdout), want);
}
