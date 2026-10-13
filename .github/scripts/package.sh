#!/usr/bin/env bash
# Packs a release build into dist/aegis-<version>-<platform>.{tar.gz,zip}
# plus a .sha256, in the layout the README's "Getting started" describes.
# Run from the repository root, after:
#   cargo build --release -p aegis-ffi --features noise
#   cargo build --release -p aegis-serve
# Usage: .github/scripts/package.sh linux-x64|windows-x64
set -euo pipefail

platform=${1:?platform: linux-x64 or windows-x64}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)
test -n "$version"
name="aegis-$version-$platform"
out="dist/$name"

rm -rf "$out" "dist/$name".*
mkdir -p "$out/lib" "$out/bin" "$out/include" "$out/examples" "$out/csharp"
case "$platform" in
  windows-*)
    cp target/release/aegis_ffi.dll target/release/aegis_ffi.dll.lib "$out/lib/"
    cp target/release/aegis-serve.exe "$out/bin/"
    ;;
  linux-*)
    cp target/release/libaegis_ffi.so "$out/lib/"
    cp target/release/aegis-serve "$out/bin/"
    ;;
  *) echo "unknown platform $platform" >&2; exit 1 ;;
esac
cp crates/ffi/include/aegis.h "$out/include/"
cp crates/ffi/examples/*.c "$out/examples/"
cp bindings/csharp/Aegis/*.cs bindings/csharp/Aegis/Aegis.csproj bindings/csharp/README.md "$out/csharp/"
cp LICENSE README.md CHANGELOG.md SECURITY.md "$out/"

cd dist
case "$platform" in
  # Windows' own bsdtar writes zip with forward-slash entries; Git Bash's
  # tar is GNU tar, which cannot.
  windows-*) archive="$name.zip"; /c/Windows/System32/tar.exe -a -cf "$archive" "$name" ;;
  *) archive="$name.tar.gz"; tar -czf "$archive" "$name" ;;
esac
sha256sum "$archive" > "$archive.sha256"
echo "dist/$archive"
