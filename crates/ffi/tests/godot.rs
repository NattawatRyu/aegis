//! Aegis inside a real engine: `demos/godot`, a Godot 4 (.NET) game whose
//! own physics answers line of sight, run headless against the library cargo
//! built for this test. Over three seeds, the aimbot must be flagged and no
//! honest bot may be (the demo's own VERDICT, exit 0).
//!
//! Needs the Godot .NET editor: set AEGIS_GODOT to its (console) executable.
//! Unset, the test is skipped.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::deps_dir;

#[test]
fn the_godot_demo_flags_the_aimbot_and_no_honest_bot() {
    let Some(godot) = std::env::var_os("AEGIS_GODOT").map(PathBuf::from) else {
        eprintln!("skipped: AEGIS_GODOT not set");
        return;
    };
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demos/godot");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let dotnet = std::env::var_os("DOTNET").unwrap_or_else(|| "dotnet".into());
    let build = Command::new(dotnet).arg("build").arg("--nologo").current_dir(&project).output().expect("run dotnet");
    assert!(build.status.success(), "dotnet build failed:\n{}{}", text(&build.stdout), text(&build.stderr));
    let import = Command::new(&godot).args(["--headless", "--import", "--path"]).arg(&project).output().unwrap();
    assert!(import.status.success(), "import failed:\n{}", text(&import.stderr));

    for seed in ["1", "2", "3"] {
        let out = Command::new(&godot)
            .args(["--headless", "--fixed-fps", "30", "--path"])
            .arg(&project)
            .args(["--", "--ticks", "9000", "--seed", seed])
            .env("AEGIS_FFI_DIR", deps_dir())
            .output()
            .expect("run godot");
        let stdout = text(&out.stdout);
        assert!(out.status.success() && stdout.contains("VERDICT ok"), "seed {seed}:\n{stdout}{}", text(&out.stderr));
        assert!(stdout.contains("(Aimbot) reaction"), "flagged by reaction, seed {seed}:\n{stdout}");
    }

    // The hardest honest players: pros who anticipate ~30% of engagements.
    // On seeds 2 and 4 the raw-share reaction line flagged one of them
    // (2026-10-10); the Wilson bound must not. The smarter cheats that
    // reaction and aim_exact miss are far_aim's (seeds picked for the pro
    // before far_aim existed). Over 24 seeds far_aim misses the triggerbot
    // in 5 and flags a pro in 2 — README — so this is a guard, not the rate.
    for seed in ["2", "4"] {
        let out = Command::new(&godot)
            .args(["--headless", "--fixed-fps", "30", "--path"])
            .arg(&project)
            .args([
                "--",
                "--ticks",
                "9000",
                "--seed",
                seed,
                "--roster",
                "honest,honest,pro,pro,aimbot,humanized,trigger",
            ])
            .env("AEGIS_FFI_DIR", deps_dir())
            .output()
            .expect("run godot");
        let stdout = text(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("VERDICT ok"),
            "pros, seed {seed}:\n{stdout}{}",
            text(&out.stderr)
        );
        assert!(!stdout.contains("(Pro)"), "a pro flagged, seed {seed}:\n{stdout}");
        for cheat in ["(Humanized) far_aim", "(Trigger) far_aim"] {
            assert!(stdout.contains(cheat), "{cheat} not raised, seed {seed}:\n{stdout}");
        }
    }
}
