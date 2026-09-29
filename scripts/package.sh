#!/usr/bin/env bash
# Builds the static aarch64 binary and lays it out as a muOS application in dist/Oxmux.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --target aarch64-unknown-linux-musl

out=dist/Oxmux
rm -rf dist
mkdir -p "$out"
cp target/aarch64-unknown-linux-musl/release/oxmux "$out/"
cp config/muos.toml "$out/oxmux.toml"
cp packaging/muos/mux_launch.sh "$out/"
chmod +x "$out/oxmux" "$out/mux_launch.sh"
echo "packaged $out"
