# System updates

Ghazal uses RAUC to install signed whole-system updates. GitHub Releases hosts
both installation images (`.img.gz`) and update bundles (`.raucb`). Updating
requires no GitHub account and no management server.

## On the handheld

1. Connect to Wi-Fi and close the running game.
2. Open **Configuration → System update**.
3. Choose **Beta** (the default during development) or **Stable**, then
   **Check for updates**. Only newer versions with a bundle matching this
   device, panel and RAM are offered.
4. Select **Install update**. Connect power or have at least 30% battery.
5. Wait for **Restart into updated OS**, then select it.

Checks and downloads only happen on request. Rendering continues while
working, but navigation and normal power/sleep actions are blocked during the
operation. Do not remove either card during installation. Devices without Wi-Fi
need a supported network adapter for GitHub downloads.

For SSH use `ghazal update check` and `ghazal update install`, followed by
`ghazal ctl reboot`. Installation does not reboot without a separate request.
A failed download can be retried; temporary bundles are held on the active data
card under `.ghazal/updates/` and removed after installation or a handled failure.

## Existing installations need a one-time migration

Beta 4 and earlier have one OS slot. They cannot gain redundant slots by copying
a new executable or writing an update bundle. Back up the entire data volume
(`ROMS`, `BIOS`, `SAVES`, `.ghazal`, including hidden files) and OS settings/SSH
identity before writing an A/B installation image to TF1.

For a two-card installation, leave TF2 intact and reflash only TF1. For a
one-card installation, back up its data to another device first, reflash TF1,
let first boot prepare the data partition, then restore the backup. Provision
Wi-Fi and SSH again, or restore the backed-up persistent settings deliberately.
Never write `.raucb` to the whole card; it is for RAUC, not an image writer.

An early two-partition card with sufficient unallocated space can also be
migrated over SSH after a verified raw backup, preserving its original OS until
the new slot boots. This requires inspecting the actual partition layout and
bootloader; it is not a general updater operation. The supported installation
procedure for existing cards remains backup and reflash.

## Layout and recovery

The MBR layout preserves the Allwinner SPL at 8 KiB. A conventional GPT table
would overlap that SPL location; do not substitute GPT without redesigning boot.

| Partition | Purpose |
|---|---|
| p1 | 128 MiB FAT provisioning volume |
| p2 | 1 GiB rootfs A, including its matching kernel and device tree |
| p3 | exFAT user data, grown to the remaining card capacity |
| p4 | Extended container for the logical system partitions |
| p5 | 1 GiB rootfs B, including its matching kernel and device tree |
| p6 | 256 MiB ext4 persistent `/var`: settings, identities, Wi-Fi and bounded logs |

The extended container is physically before p3. The data partition remains p3
so first-boot growth changes only its MBR size. TF2 is never repartitioned.
Both slots initially contain the same OS. The OS root is read-only; `/var`, the
selected data card and RAM filesystems hold mutable state.

RAUC writes only the inactive rootfs. The selected slot changes after successful
installation; games, saves and persistent system state are outside both slots.
Boot selection uses redundant U-Boot environments at 4 MiB and 4 MiB + 128 KiB,
with three attempts per slot. Each attempt is persisted before loading Linux.
A boot is confirmed through RAUC after the frontend draws a frame, the daemon
socket exists, and the data card is mounted. Confirmation runs in the background.

The health service arms the hardware watchdog and stops feeding it if startup
never reaches those checks. Kernel panics reboot. After three unconfirmed boots,
U-Boot tries the other slot. A kernel hang before userspace arms the watchdog
can still require Reset; bootloader-to-kernel watchdog handover needs physical
validation. If both slots are exhausted, reflash from a computer after backing
up data. Routine updates do not replace SPL/U-Boot or the partition layout.

A missing, invalid or full data volume enters Wi-Fi/SSH recovery rather than
repeatedly rebooting through both slots. Correct the selected card or its
`/var/lib/ghazal/data-card` preference, then reboot. Rolling back OS code cannot
repair a shared data-card fault.

Rollback restores OS code, not previous copies of shared data. Future settings
migrations must retain compatibility with the previous OS; destructive shared
schema migrations would defeat rollback.

## Release signing and CI

- `config/updates/release.pem` is the public trust certificate embedded in images.
- The private key is stored outside the repository and provided to GitHub Actions
  through the `RAUC_SIGNING_KEY` secret. Never put it in Git, Nix inputs, images,
  artifacts or logs. Keep an offline backup; losing it prevents updates to installed
  devices without a separate trust migration/reflash.
- CI produces one device/RAM-specific signed bundle alongside each image. The
  compatibility ID includes `ab1`, the device/panel ID and RAM variant.
- A release build refuses to proceed without its signing key. Pull requests
  without secrets can build installation images but cannot publish trusted bundles.
- A key rotation needs a transition release trusting the old and new public keys
  before bundles are signed exclusively by the new key. Do not just replace the
  certificate and strand already installed devices.

Local signing uses `GHAZAL_UPDATE_KEY=/private/path/release.key` when invoking
`ghazal-image-builder`. The private key is read by the host RAUC bundle tool,
not by Nix derivations or the target image.

## Repeatable observations

`tools/update-lab` uses disposable keys, bundles, file slots and a private D-Bus.
It never opens a physical disk. Run it with the pinned RAUC, OpenSSL,
squashfs-tools, D-Bus and Bash on PATH. With `--install`, run as root on Linux
with loop, squashfs and dm-verity support.

It checks signature rejection, truncated bundles, actual RAUC installation into
an inactive slot, wrong-device rejection, corrupted payload rejection and active
slot preservation. It also executes the production boot-selection script with
mocked load/boot commands to observe trial exhaustion and fallback. This models
script decisions; it does not emulate U-Boot MMC/environment drivers or prove
H700 watchdog, panel or power-loss behavior.

Physical RG40XX-V v2 / DDR4 checks cover both slot boots, signed installation
from TF2 ext4 and TF1 exFAT, retained settings/SSH identities/saves, a missing
inactive kernel, and three failed frontend starts followed by automatic watchdog
resets and fallback. The data volume recovered after those resets. Deliberate
power removal during installation or an environment write, early kernel hangs,
and other hardware variants still need physical validation.
