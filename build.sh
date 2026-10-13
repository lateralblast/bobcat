#!/usr/bin/env bash
# Build Bobcat installers.
#   ./build.sh --target mac [--arch arm64|x64|universal]
#   ./build.sh --target linux
#   ./build.sh --target windows      (run on Windows, e.g. in Git Bash)
# A target can only be built on its own platform (the GUI links the system web view).
# Results are copied to ./dist/. Needs the Rust toolchain and tauri-cli
# (cargo install tauri-cli --version '^2'). License: CC BY-NC-SA 4.0 (LICENSE.md).
set -euo pipefail

usage() {
  echo "usage: $0 --target mac|linux|windows [--arch arm64|x64|universal] [--no-cli]"
  exit "${1:-1}"
}

target="" arch="" cli=1
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --arch) arch="${2:-}"; shift 2 ;;
    --no-cli) cli=0; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[ -n "$target" ] || usage

root=$(cd "$(dirname "$0")" && pwd)
dist="$root/dist"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/gui/src-tauri/Cargo.toml" | head -1)

host=$(uname -s)
case "$target" in
  mac|macos)   want=Darwin; bundles=dmg ;;
  linux)       want=Linux;  bundles=deb,appimage ;;
  windows|win) want=MINGW;  bundles=nsis ;;
  *) echo "unknown target: $target (mac, linux, windows)" >&2; exit 1 ;;
esac
case "$host" in
  $want*) ;;
  *) echo "the $target installer must be built on $target (this is $host)" >&2; exit 1 ;;
esac

command -v cargo >/dev/null || { echo "cargo not found: install Rust (https://rustup.rs)" >&2; exit 1; }
cargo tauri --version >/dev/null 2>&1 || { echo "tauri-cli not found: cargo install tauri-cli --version '^2'" >&2; exit 1; }

triple_args=()
if [ "$target" = mac ] || [ "$target" = macos ]; then
  case "${arch:-native}" in
    native) ;;
    arm64) triple_args=(--target aarch64-apple-darwin) ;;
    x64) triple_args=(--target x86_64-apple-darwin) ;;
    universal) triple_args=(--target universal-apple-darwin) ;;
    *) echo "unknown --arch: $arch" >&2; exit 1 ;;
  esac
  if [ -n "${triple_args[1]:-}" ]; then
    # make sure the Rust standard library for the requested architecture is installed
    case "${triple_args[1]}" in
      universal-apple-darwin) rustup target add aarch64-apple-darwin x86_64-apple-darwin ;;
      *) rustup target add "${triple_args[1]}" ;;
    esac
  fi
elif [ -n "$arch" ]; then
  echo "--arch only applies to --target mac" >&2; exit 1
fi

mkdir -p "$dist"
echo "== building the GUI installer ($target, $bundles) =="
(cd "$root/gui/src-tauri" && cargo tauri build --bundles "$bundles" --features vendored ${triple_args[@]+"${triple_args[@]}"})

# Tauri puts bundles under target/[<triple>/]release/bundle/<kind>/ (the GUI crate has its own target dir)
found=0
while IFS= read -r f; do
  cp -R "$f" "$dist/"
  echo "  $dist/$(basename "$f")"
  found=1
done < <(find "$root/gui/src-tauri/target" -path '*/release/bundle/*' -type f \
  \( -name '*.dmg' -o -name '*.deb' -o -name '*.AppImage' -o -name '*-setup.exe' \) -newer "$root/gui/src-tauri/Cargo.toml" 2>/dev/null)
[ "$found" = 1 ] || { echo "no installer found under gui/src-tauri/target" >&2; exit 1; }

if [ "$cli" = 1 ]; then
  echo "== building bobcat-cli =="
  (cd "$root" && cargo build --release --features vendored ${triple_args[@]+"${triple_args[@]}"})
  bin=$(find "$root/target" -path '*/release/bobcat-cli*' -type f ! -name '*.d' -perm -u+x -newer "$root/Cargo.toml" | head -1)
  if [ -n "$bin" ]; then
    cp "$bin" "$dist/bobcat-cli-$version-$target"
    echo "  $dist/bobcat-cli-$version-$target"
  fi
fi
echo "done: $dist"
