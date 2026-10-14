//! What the docs say about versions is what the code is. README's ABI number
//! went stale once (it said 3 after the ABI became 4); this keeps it, and
//! the CHANGELOG, from doing that again.

use std::fs;
use std::path::Path;

use aegis_ffi::AEGIS_ABI_VERSION;

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_readme_names_the_abi_version_and_the_crate_version() {
    let readme = doc("README.md");
    let abi = format!("`AEGIS_ABI_VERSION`, now {AEGIS_ABI_VERSION})");
    assert!(readme.contains(&abi), "README.md must say {abi}");
    let version = format!("Every crate is {}", env!("CARGO_PKG_VERSION"));
    assert!(readme.contains(&version), "README.md must say {version}");
}

#[test]
fn the_changelog_has_an_entry_for_this_version_and_abi() {
    let log = doc("CHANGELOG.md");
    let head = format!("## [{}]", env!("CARGO_PKG_VERSION"));
    let entry = log.split(&head).nth(1).unwrap_or_else(|| panic!("CHANGELOG.md has no {head}"));
    let entry = entry.split("\n## ").next().unwrap();
    let abi = format!("ABI {AEGIS_ABI_VERSION}");
    assert!(entry.contains(&abi), "the {head} entry must name {abi}");
}

#[test]
fn every_workspace_dependency_asks_for_this_version() {
    // A path dependency's `version` is what crates.io resolves; left behind
    // on a bump, a published crate would pull its siblings' older release.
    let manifest = doc("Cargo.toml");
    let want = format!("version = \"{}\" }}", env!("CARGO_PKG_VERSION"));
    let deps: Vec<&str> = manifest.lines().filter(|l| l.contains("package = \"aegis-netguard-")).collect();
    assert_eq!(deps.len(), 6, "expected six aegis-netguard-* workspace dependencies");
    for line in deps {
        assert!(line.ends_with(&want), "Cargo.toml: {line} must end with {want}");
    }
}

#[test]
fn the_header_and_the_csharp_binding_name_the_same_abi_version() {
    let h = doc("crates/ffi/include/aegis.h");
    assert!(h.contains(&format!("#define AEGIS_ABI_VERSION {AEGIS_ABI_VERSION}u")), "aegis.h");
    let cs = doc("bindings/csharp/Aegis/AegisMonitor.cs");
    assert!(cs.contains(&format!("public const uint AbiVersion = {AEGIS_ABI_VERSION};")), "AegisMonitor.cs");
}
