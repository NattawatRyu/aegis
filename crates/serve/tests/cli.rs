//! The `aegis-serve` binary as an operator runs it: it says where it
//! listens and which key a client pins, writes that key where a client can
//! read it, plays the ticks it was given and prints who played; a flag it
//! does not know stops it before it binds anything.

use std::process::{Command, Output};

fn serve(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aegis-serve")).args(args).output().expect("run aegis-serve")
}

#[test]
fn it_prints_the_key_writes_it_out_and_tallies_every_bot() {
    let key_file = std::env::temp_dir().join(format!("aegis-serve-{}.key", std::process::id()));
    let log = std::env::temp_dir().join(format!("aegis-serve-{}.jsonl", std::process::id()));
    let out = serve(&[
        "--log",
        log.to_str().unwrap(),
        "--listen",
        "127.0.0.1:0",
        "--ticks",
        "60",
        "--bots",
        "honest,camper",
        "--key-out",
        key_file.to_str().unwrap(),
    ]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    let first = stdout.lines().next().unwrap();
    let key = first.split("relay-key ").nth(1).unwrap_or_else(|| panic!("no key in {first:?}"));
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(std::fs::read_to_string(&key_file).unwrap(), key, "the file holds what was printed");
    let _ = std::fs::remove_file(&key_file);
    // Every record the detector saw, one JSON object a line: two bots over
    // 60 ticks accept at least an input a tick each.
    let records = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    let n = records.lines().inspect(|l| assert!(l.starts_with("{\"tick\":"), "{l}")).count();
    assert!(n >= 100, "{n} records logged");
    for who in ["honest", "camper"] {
        assert!(stdout.lines().any(|l| l.contains(who)), "{who} missing from the tally:\n{stdout}");
    }
}

#[test]
fn direct_says_it_is_unsealed() {
    let out = serve(&["--listen", "127.0.0.1:0", "--ticks", "30", "--bots", "honest", "--direct"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.lines().next().unwrap().ends_with("direct (unsealed)"), "{stdout}");
    assert!(stdout.lines().any(|l| l.contains("honest")), "{stdout}");
}

#[test]
fn a_bad_flag_is_refused_with_status_2() {
    for args in [&["--bots", "wallhack"][..], &["--ticks"][..], &["--nope", "1"][..], &["--listen", "here"][..]] {
        let out = serve(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("aegis-serve: "), "{args:?}");
    }
}
