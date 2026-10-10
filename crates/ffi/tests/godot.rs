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
}
