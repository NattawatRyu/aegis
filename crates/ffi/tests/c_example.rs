//! The header is written by hand, so it is held to the library by the only
//! judge that counts: a C compiler. Each example in `examples/` is built
//! with the platform's compiler against `include/aegis.h`, linked to the
//! cdylib cargo built for this test, and run. It must exit 0 (its own
//! checks), and every size, field offset and constant it prints must equal
//! Rust's. A function declared but not exported fails the link.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;

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

/// An engine's own world, through the evidence and into the monitor: the
/// player that fires the tick an enemy appears is flagged by reaction, the
/// one 15 ticks later by nothing.
#[test]
fn the_engine_example_builds_runs_and_agrees_on_every_layout() {
    let stdout = build_and_run("engine");
    assert!(stdout.contains("player=2 reaction"), "{stdout}");
    assert!(stdout.contains("player 2: 30 shots, 30 timed, 30 fast"), "{stdout}");
    assert_eq!(layout_lines(&stdout), evidence_layout());
}

/// The Noise section of the header, linked against a library built with
/// feature `noise` (a declared function it does not export fails the link).
#[cfg(feature = "noise")]
#[test]
fn the_noise_example_builds_runs_and_agrees_on_every_constant() {
    let stdout = build_and_run("noise");
    assert_eq!(layout_lines(&stdout), noise_consts());
}

#[test]
fn the_monitor_example_builds_runs_and_agrees_on_every_layout() {
    let stdout = build_and_run("monitor");
    assert!(stdout.contains("refused: foresight.clear_rad"), "{stdout}");
    assert!(stdout.contains("player=2 reaction"), "the instant player is flagged by reaction:\n{stdout}");
    assert_eq!(layout_lines(&stdout), monitor_layout());
}

#[test]
fn the_client_example_builds_runs_and_agrees_on_every_layout_and_constant() {
    let stdout = build_and_run("client");
    assert_eq!(layout_lines(&stdout), client_layout_and_consts());
}
