#!/usr/bin/env bash
# Package a release bundle from an already built `cargo build --release`.
#
#   tools/package-release.sh <tag> <platform>      e.g. tools/package-release.sh v0.2.0 windows-x64
#
# Writes dist/DumbEngine-<tag>-<platform>.zip (Windows) or .tar.gz (others). The bundle holds the
# editor and player executables plus the engine sources, because game scripts are Rust crates
# that compile against them, and a rust-toolchain.toml pinning the compiler the executables were
# built with (scripts must use the same one). Used by .github/workflows/release.yml.
set -euo pipefail

tag="${1:?usage: package-release.sh <tag> <platform>}"
platform="${2:?usage: package-release.sh <tag> <platform>}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

exe=""
case "$platform" in windows*) exe=".exe" ;; esac
for bin in dumb-editor dumb-player; do
    [[ -f "target/release/$bin$exe" ]] || { echo "target/release/$bin$exe is missing; run: cargo build --release --bin dumb-editor --bin dumb-player" >&2; exit 1; }
done

name="DumbEngine-$tag-$platform"
out="dist/$name"
rm -rf "$out" "dist/$name.zip" "dist/$name.tar.gz"
mkdir -p "$out"

cp "target/release/dumb-editor$exe" "target/release/dumb-player$exe" "$out/"
cp Cargo.toml Cargo.lock README.md "$out/"
cp -r crates docs tools "$out/"
cp -r project "$out/project"
# Build output and per-user state never ship.
rm -rf "$out/project/Scripts/target" "$out/project/Library" "$out/tools/__pycache__"
find "$out" -name target -type d -prune -exec rm -rf {} +

rustc_version="$(rustc --version | cut -d' ' -f2)"
cat > "$out/rust-toolchain.toml" <<EOF
# The editor in this bundle was built with Rust $rustc_version. Game scripts are loaded as
# native libraries and must be compiled by the same compiler, so the editor builds them with
# this toolchain. Install it once with: rustup toolchain install $rustc_version
[toolchain]
channel = "$rustc_version"
EOF

cat > "$out/START_HERE.txt" <<EOF
Dumb Engine $tag ($platform)

1. Install Rust (https://rustup.rs), then: rustup toolchain install $rustc_version
2. Run dumb-editor$exe. It opens the sample project in project/ (or pick your own).
3. Press "Build scripts" once to compile the sample game's scripts.

Requirements: a GPU with Vulkan 1.3. Blender is optional (for .blend/.fbx/.obj import).
Keep this folder together: the editor compiles game scripts against crates/.
Docs: README.md, docs/ (AI agents: docs/MCP.md).
EOF

cd dist
case "$platform" in
    windows*)
        if command -v 7z >/dev/null; then 7z a -bso0 "$name.zip" "$name"; else zip -qr "$name.zip" "$name"; fi
        echo "dist/$name.zip"
        ;;
    *)
        tar czf "$name.tar.gz" "$name"
        echo "dist/$name.tar.gz"
        ;;
esac
