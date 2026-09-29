#!/usr/bin/env bash
# Copies dist/Oxmux to the device over SSH (enable SSH in muOS first), optionally moving
# the install to stage 2 or 3.
#   DEVICE=root@DEVICE_IP scripts/deploy.sh             # stage 1: muOS application
#   DEVICE=root@DEVICE_IP scripts/deploy.sh frontend    # stage 2
#   DEVICE=root@DEVICE_IP scripts/deploy.sh init        # stage 3
set -euo pipefail
cd "$(dirname "$0")/.."

device="${DEVICE:-root@rg40xxv.local}"
dest="${DEST:-/mnt/mmc/MUOS/application}"
stage="${1:-}"

scripts/package.sh
# tar over ssh works with both OpenSSH and dropbear (no sftp needed).
tar -C dist -cf - Oxmux | ssh "$device" "mkdir -p '$dest' && tar -C '$dest' -xf -"
echo "deployed to $device:$dest/Oxmux"

if [ -n "$stage" ]; then
    ssh "$device" "'$dest/Oxmux/oxmux' install '$stage' --dry-run && '$dest/Oxmux/oxmux' install '$stage'"
    echo "reboot the device to boot into stage '$stage'"
fi
