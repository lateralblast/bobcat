#!/bin/sh
# Runs inside the docker/windows image (started by `./build.sh --target windows --docker`): builds the
# NSIS installer and bobcat-cli.exe for x86_64-pc-windows-gnu into /src/windows/out/cross.
set -eu
T=x86_64-pc-windows-gnu
out=/src/windows/out/cross
rm -rf "$out"; mkdir -p "$out"
cd /src/gui/src-tauri
cargo tauri build --target $T --bundles nsis --features vendored
cp target/$T/release/bundle/nsis/*-setup.exe "$out/"
if [ "${1:-}" != --no-cli ]; then
  cd /src
  cargo build --release --target $T --features vendored
  cp target/$T/release/bobcat-cli.exe "$out/"
fi
