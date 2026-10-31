#!/usr/bin/env bash
# The release gates CI runs on every build, for a machine you control.
#
#   scripts/release-tests.sh [layout|input|smoke|all]     (default: all)
#
#   layout  the built runtime tree against the shipped profile: gamepad profile,
#           cores, commands, PPSSPP assets, SDL mapping
#   input   the runtime's RetroArch (null video and audio) driven by a virtual
#           H700 gamepad: configured from the profile, hotkeys, save and resume
#           (needs root and /dev/uinput)
#   smoke   Ghazal as PID 1 on a stock kernel in QEMU draws its menu and answers a key
#
# The runtime is aarch64, so run this on an aarch64 Linux machine with Nix. On
# macOS it re-runs itself in a privileged nixos/nix container that shares the Nix
# store volume (GHAZAL_NIX_VOLUME, default oxmux-nix); Docker's Linux kernel
# provides /dev/uinput. Exit status 1 when a gate fails.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:-all}
case "$mode" in
  layout | input | smoke | all) ;;
  *) echo "usage: $0 [layout|input|smoke|all]" >&2; exit 2 ;;
esac

if [ "$(uname -s)" = Darwin ]; then
  mounts=(-v "$PWD:$PWD")
  # In a git worktree the repository lives elsewhere; Nix needs it to read the flake.
  common=$(cd "$(git rev-parse --git-common-dir)" && pwd)
  [ "$common" = "$PWD/.git" ] || mounts+=(-v "$(dirname "$common"):$(dirname "$common"):ro")
  exec docker run --rm --privileged --net=host -v "${GHAZAL_NIX_VOLUME:-oxmux-nix}:/nix" "${mounts[@]}" -w "$PWD" nixos/nix:latest "$PWD/scripts/release-tests.sh" "$mode"
fi

tree=$(nix build .#runtime --no-link --print-out-paths)
tool=$(nix build .#release-check --no-link --print-out-paths)/bin/ghazal-release-check
failed=0

if [ "$mode" = layout ] || [ "$mode" = all ]; then
  "$tool" layout "$tree" config/rg40xxv-rocknix || failed=1
fi

if [ "$mode" = input ] || [ "$mode" = all ]; then
  run=("$tool")
  [ "$(id -u)" -eq 0 ] || run=(sudo "$tool")
  status=0
  "${run[@]}" input "$tree" config/rg40xxv-rocknix || status=$?
  case "$status" in
    0) ;;
    77) echo "The input test was skipped: this machine has no /dev/uinput." ;;
    *) failed=1 ;;
  esac
fi

if [ "$mode" = smoke ] || [ "$mode" = all ]; then
  nix run .#vm-smoke || failed=1
fi
exit "$failed"
