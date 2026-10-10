//! The C# binding (`bindings/csharp`) against the library cargo built for
//! this test: `Aegis.Check` is built with the .NET SDK and run. It must exit
//! 0 (its own checks: the engine loop of `examples/engine.c`, the client's
//! statuses, a re-entrant callback refused, a throwing one rethrown) and
//! every size, field offset and constant it prints must equal Rust's — a C#
//! `bool` field where C has one byte would move every offset after it.
//!
//! No `dotnet`, no test: skipped, unless AEGIS_REQUIRE_DOTNET is set (CI),
//! where a missing SDK is a failure rather than a silent pass.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;

fn dotnet() -> Option<PathBuf> {
    let name = if cfg!(windows) { "dotnet.exe" } else { "dotnet" };
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join(name)).collect::<Vec<_>>())
        .unwrap_or_default();
    let installed = [PathBuf::from(r"C:\Program Files\dotnet\dotnet.exe"), PathBuf::from("/usr/share/dotnet/dotnet")];
    on_path.into_iter().chain(installed).find(|p| p.is_file())
}

#[test]
fn the_csharp_binding_builds_runs_and_agrees_on_every_layout_and_constant() {
    let Some(dotnet) = dotnet() else {
        assert!(std::env::var_os("AEGIS_REQUIRE_DOTNET").is_none(), "AEGIS_REQUIRE_DOTNET is set and no dotnet found");
        eprintln!("skipped: no dotnet");
        return;
    };
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bindings/csharp/Aegis.Check");
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("csharp");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let build = Command::new(&dotnet)
        .args(["build", "-c", "Release", "--nologo", "-o"])
        .arg(&out_dir)
        .arg(&project)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .output()
        .expect("run dotnet build");
    assert!(build.status.success(), "dotnet build failed:\n{}{}", text(&build.stdout), text(&build.stderr));

    let lib_dir = deps_dir();
    let mut run = Command::new(&dotnet);
    run.arg(out_dir.join("Aegis.Check.dll")).arg("layout");
    if cfg!(feature = "noise") {
        run.arg("noise");
    }
    let (var, sep) = if cfg!(windows) {
        ("PATH", ";")
    } else if cfg!(target_os = "macos") {
        ("DYLD_LIBRARY_PATH", ":")
    } else {
        ("LD_LIBRARY_PATH", ":")
    };
    let old = std::env::var(var).unwrap_or_default();
    run.env(var, format!("{}{sep}{old}", lib_dir.display()));
    let out = run.output().expect("run Aegis.Check");
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "Aegis.Check failed:\n{stdout}{}", text(&out.stderr));
    assert!(stdout.lines().any(|l| l == "ok"), "{stdout}");
    assert!(stdout.contains("player 2: 30 shots, 30 timed, 30 fast"), "{stdout}");

    // C# prints the Noise constants whatever the build; Rust has them only
    // with the feature. Enum order is .NET's, so compare as sets.
    let mut got: Vec<&str> = layout_lines(&stdout);
    if !cfg!(feature = "noise") {
        got.retain(|l| !l.starts_with("const AEGIS_NOISE_"));
    }
    let mut want: Vec<String> = [monitor_layout(), client_layout_and_consts(), evidence_layout()].concat();
    #[cfg(feature = "noise")]
    want.extend(noise_consts());
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
}
