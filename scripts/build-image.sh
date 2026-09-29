#!/usr/bin/env bash
# Stage 4: build an SD card image that boots straight into oxmux (PID 1), no muOS scripts.
#
#   scripts/build-image.sh MustardOS_RG40XXV_<version>.img[.gz|.xz] [out.img]
#
# Like MustardOS's own tooling (tool/rootfs/*.sh), this patches a released image rather
# than building from scratch: the vendor kernel (4.9.170), bootloader, kernel modules,
# wifi firmware, Mali libraries, RetroArch and cores all come from the base image. Only
# the rootfs (GPT partition 5, ext4) changes:
#   - oxmux goes to /opt/oxmux with its configs,
#   - /init (the kernel runs init=/init) becomes a symlink to oxmux,
#   - muOS's /init is kept as /init.muos; oxmux hands over to it after 3 boots that
#     never reach the menu, or if its config fails to load.
# The ext4 filesystem is edited with debugfs, so no root, loop devices or mounts are needed.
# Tools: sfdisk, debugfs, e2fsck, jq (all in `nix develop`).
set -euo pipefail
cd "$(dirname "$0")/.."

base=${1:?usage: $0 BASE_IMAGE [OUT_IMAGE]}
out=${2:-dist/oxmux-rg40xxv.img}
rootfs_part=${ROOTFS_PART:-5}
bin=${OXMUX_BIN:-target/aarch64-unknown-linux-musl/release/oxmux}

for tool in sfdisk debugfs e2fsck jq; do
    command -v "$tool" >/dev/null || { echo "missing $tool (run inside nix develop)" >&2; exit 1; }
done
if [ -z "${OXMUX_BIN:-}" ]; then
    cargo build --release --target aarch64-unknown-linux-musl
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$(dirname "$out")"

echo "==> copying base image to $out"
case "$base" in
    *.xz) xz -dc "$base" >"$out" ;;
    *.gz) gzip -dc "$base" >"$out" ;;
    *) cp --reflink=auto "$base" "$out" ;;
esac

echo "==> locating rootfs (partition $rootfs_part)"
table=$(sfdisk -J "$out")
sector=$(jq -r '.partitiontable.sectorsize // 512' <<<"$table")
read -r start size < <(jq -r --arg n "$rootfs_part" \
    '.partitiontable.partitions[] | select(.node | endswith($n)) | "\(.start) \(.size)"' <<<"$table")
[ -n "${start:-}" ] || { echo "partition $rootfs_part not found in $base" >&2; exit 1; }
echo "    sector $start, $size sectors of $sector bytes"

rootfs="$work/rootfs.ext4"
dd if="$out" of="$rootfs" bs="$sector" skip="$start" count="$size" status=none
debugfs -R "stat /init" "$rootfs" 2>/dev/null | grep -q "Type:" \
    || { echo "partition $rootfs_part has no /init; is this an H700 muOS image?" >&2; exit 1; }

sed 's|@RESCUE_EXEC@|/init.muos|' config/rg40xxv/system-standalone.toml >"$work/system.toml"
cp config/rg40xxv/device.toml config/rg40xxv/frontend.toml "$work/"

# Rebuilding an already-patched image just refreshes oxmux and its configs.
already=no
if debugfs -R "stat /init.muos" "$rootfs" 2>/dev/null | grep -q "Type:"; then
    already=yes
fi

exists() { debugfs -R "stat $1" "$rootfs" 2>/dev/null | grep -q "Type:"; }

echo "==> installing oxmux into the rootfs (already patched: $already)"
{
    # debugfs's mkdir on an existing name leaks an inode, so only create missing dirs.
    for dir in /opt /opt/oxmux /opt/oxmux/etc /opt/oxmux/state; do
        exists "$dir" || echo "mkdir $dir"
    done
    install_file() { # local, target, mode
        echo "rm $2"
        echo "write $1 $2"
        echo "sif $2 mode $3"
        echo "sif $2 uid 0"
        echo "sif $2 gid 0"
    }
    install_file "$bin" /opt/oxmux/oxmux 0100755
    for f in device.toml frontend.toml system.toml; do
        install_file "$work/$f" "/opt/oxmux/etc/$f" 0100644
    done
    if [ "$already" = no ]; then
        # Rename /init to /init.muos: a second directory entry, then drop the first.
        echo "ln /init /init.muos"
        echo "unlink /init"
    else
        echo "rm /init"
    fi
    echo "symlink /init /opt/oxmux/oxmux"
    echo "sif /init uid 0"
    echo "sif /init gid 0"
} >"$work/cmds"
# debugfs reports "file not found" for the rm of a fresh install; that's expected.
debugfs -w -f "$work/cmds" "$rootfs" >"$work/debugfs.log" 2>&1 || true
grep -iE "error|cannot|couldn't" "$work/debugfs.log" | grep -v "File not found by ext2_lookup" && {
    echo "debugfs reported errors (see above)" >&2; exit 1; } || true

echo "==> checking the filesystem"
e2fsck -fn "$rootfs" >"$work/fsck.log" 2>&1 || { cat "$work/fsck.log" >&2; echo "e2fsck found problems" >&2; exit 1; }
[ "$(debugfs -R "stat /init" "$rootfs" 2>/dev/null | grep -c 'Fast link dest: "/opt/oxmux/oxmux"')" = 1 ] \
    || { echo "/init is not the oxmux symlink" >&2; exit 1; }

echo "==> writing the rootfs back"
dd if="$rootfs" of="$out" bs="$sector" seek="$start" conv=notrunc status=none
sync
echo "done: $out"
echo "flash it with e.g.: sudo dd if=$out of=/dev/sdX bs=4M conv=fsync status=progress"
