#!/bin/sh
# HELP: oxmux - Rust frontend for muOS
# ICON: retroarch
#
# Stage 1: run oxmux as a muOS application. muOS stops its own frontend before running
# this, so oxmux gets the screen and gamepad until it quits. Configs sit next to the
# binary (device.toml, frontend.toml, system.toml).

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$APP_DIR" || exit 1
exec ./oxmux frontend >"$APP_DIR/oxmux.log" 2>&1
