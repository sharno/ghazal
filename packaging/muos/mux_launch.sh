#!/bin/sh
# HELP: oxmux - Rust frontend prototype
# ICON: retroarch
#
# muOS application entry. muOS stops its own frontend before running this, so
# oxmux gets the framebuffer and gamepad to itself until it quits.

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$APP_DIR" || exit 1
exec ./oxmux --config "$APP_DIR/oxmux.toml" >"$APP_DIR/oxmux.log" 2>&1
