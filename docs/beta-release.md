# H700 beta

This beta provides 38 flashable images: 19 device/panel variants, each with
DDR3 and DDR4 bootloaders. Every image supports one-card and two-card setups.
Choose the model, panel and actual RAM revision before flashing.

- [Installation](https://github.com/sharno/oxmux/blob/main/docs/installation.md)
- [Files and storage](https://github.com/sharno/oxmux/blob/main/docs/storage-layout.md)
- [Device catalog](https://github.com/sharno/oxmux/blob/main/config/h700/devices.toml)

Images include the oxmux menu, RetroArch cores, standalone PPSSPP on unrotated
devices, SSH/SFTP, Wi-Fi setup and Syncthing. Public images contain no personal
credentials, ROMs or proprietary BIOS files. Each handheld creates its own
machine ID and SSH host key. SHA256SUMS covers every compressed image.

CI builds both bootloaders, all device trees and the common kernel from pinned
sources, runs Rust checks, audits public provisioning, checks both image
filesystems and verifies the complete artifact inventory before publication.
A disposable loop-device test grows p3 while p1/p2 are mounted, kills a
formatter after its first write, recovers that transaction, checks save retention
and proves unknown data is refused. Provisioning errors are reported on the
boot volume and the device can continue offline; a retry-boot request lets
users leave recovery without SSH.
Kernel/U-Boot and the emulator runtime reuse the pinned ROCKNIX source/bundle;
their source provenance is recorded in hardware.lock and image.json.

**Hardware acceptance:** RG40XX-V v2 with DDR4 has been exercised on a physical
device. Other models, panel revisions and DDR3 targets are experimental; a
successful CI build does not prove their display, controls, sound or Wi-Fi.
Test on a spare card and report the exact image and hardware revision.

Native suspend/wake remains disabled. Clamshell lid suspend is not implemented.
Some systems require user-supplied BIOS files and stay hidden until available.
RG28XX uses rotated menu output and the libretro PSP core; art previews are
hidden on its compact screen. RG28XX and RG35XX 2024 have no built-in Wi-Fi.
Automatic updates and an in-place upgrade preserving TF1 data are not included:
back up before reflashing. Prefer TF2 for data you want to retain across OS changes.
