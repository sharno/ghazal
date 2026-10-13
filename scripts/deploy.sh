#!/usr/bin/env bash
# Copies dist/Ghazal to the device over SSH (enable SSH in muOS first), optionally moving
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
tar -C dist -cf - Ghazal | ssh "$device" "mkdir -p '$dest' && tar -C '$dest' -xf -"
echo "deployed to $device:$dest/Ghazal"

if [ -n "$stage" ]; then
    ssh "$device" "'$dest/Ghazal/ghazal' install '$stage' --dry-run && '$dest/Ghazal/ghazal' install '$stage'"
    echo "reboot the device to boot into stage '$stage'"
fi
