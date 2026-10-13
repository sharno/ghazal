#!/bin/sh
# HELP: ghazal - Rust frontend for muOS
# ICON: retroarch
#
# Stage 1: run ghazal as a muOS application. muOS stops its own frontend before running
# this, so ghazal gets the screen and gamepad until it quits. Configs sit next to the
# binary (device.toml, frontend.toml, system.toml).

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$APP_DIR" || exit 1
exec ./ghazal frontend >"$APP_DIR/ghazal.log" 2>&1
