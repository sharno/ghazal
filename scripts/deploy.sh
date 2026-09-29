#!/usr/bin/env bash
# Copies dist/Oxmux to the device over SSH (enable SSH in muOS first).
#   DEVICE=root@DEVICE_IP scripts/deploy.sh
set -euo pipefail
cd "$(dirname "$0")/.."

device="${DEVICE:-root@rg40xxv.local}"
dest="${DEST:-/mnt/mmc/MUOS/application}"

[ -d dist/Oxmux ] || scripts/package.sh
# tar over ssh works with both OpenSSH and dropbear (no sftp needed).
tar -C dist -cf - Oxmux | ssh "$device" "mkdir -p '$dest' && tar -C '$dest' -xf -"
echo "deployed to $device:$dest/Oxmux"
