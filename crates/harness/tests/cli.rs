//! The `aegis-harness` binary, run the way the docs run it: every subcommand
//! at its smallest size exits 0 and prints its table. The library under each
//! one is tested on its own; this catches the printing — a header that no
//! longer matches its rows' count, an index past a grid, a panic in a format.
//!
//! The three that take over 25 s in a debug build are `#[ignore]`d; run
//! them with `cargo test -p aegis-harness --test cli -- --ignored`.

use std::process::Command;

/// Run the harness with `args`; its stdout, after asserting it exited 0
/// and printed at least `lines` lines.
fn harness(args: &[&str], lines: usize) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_aegis-harness")).args(args).output().expect("run aegis-harness");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 stdout");
    assert!(out.status.success(), "{args:?}: {}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
    assert!(stdout.lines().count() >= lines, "{args:?} printed {} lines:\n{stdout}", stdout.lines().count());
    stdout
}

#[test]
fn standard_in_process_and_over_udp() {
    for (args, file) in [(&[][..], "standard.jsonl"), (&["--udp"][..], "standard.udp.jsonl")] {
        let out = harness(args, 20);
        assert!(out.contains(&format!("-> scenarios/out/{file}")), "{out}");
    }
}

#[test]
#[ignore = "about 28 s in a debug build"]
fn standard_through_a_relay() {
    let out = harness(&["--relay"], 20);
    assert!(out.contains("never reached the origin"), "{out}");
}

#[test]
fn sweep() {
    harness(&["sweep", "1"], 20);
}

#[test]
fn lag_both_mixes() {
    harness(&["lag", "1", "1"], 20);
    harness(&["lag", "1", "1", "stale"], 20);
}

#[test]
fn net() {
    harness(&["net", "1", "1", "0"], 10);
}

#[test]
fn interp() {
    harness(&["interp", "1", "1", "50"], 10);
}

#[test]
fn foresight() {
    harness(&["foresight", "1", "1"], 10);
}

#[test]
fn far() {
    harness(&["far", "1", "1"], 5);
}

#[test]
fn cull() {
    harness(&["cull", "1", "1"], 5);
}

#[test]
#[ignore = "about 28 s in a debug build"]
fn cull_scale() {
    harness(&["cull-scale", "1"], 5);
}

#[test]
#[ignore = "about 70 s in a debug build; it is meant to run --release"]
fn scale() {
    harness(&["scale", "1"], 5);
}
