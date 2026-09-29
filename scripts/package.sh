#!/usr/bin/env bash
# Builds the static aarch64 binary and lays it out as a muOS application in dist/Oxmux.
# The same folder carries everything needed for stages 2 and 3 (`./oxmux install ...`).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --target aarch64-unknown-linux-musl

out=dist/Oxmux
rm -rf "$out"
mkdir -p "$out"
cp target/aarch64-unknown-linux-musl/release/oxmux "$out/"
cp config/rg40xxv/frontend.toml "$out/frontend.toml"
cp config/rg40xxv/system-app.toml "$out/system.toml"
# As a muOS app, muOS's own hotkey daemon is still running: grab the pad so it doesn't
# also react to menu navigation.
sed 's/^gamepad = "muOS-Keys"$/gamepad = "muOS-Keys"\ngrab = true/' config/rg40xxv/device.toml >"$out/device.toml"
cp packaging/muos/mux_launch.sh "$out/"
chmod +x "$out/oxmux" "$out/mux_launch.sh"
echo "packaged $out"
