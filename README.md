# oxmux

A Rust userland for the Anbernic RG40XXV (Allwinner H700), built as a muOS alternative.
It replaces muOS's frontend and its ~21,600 lines of boot and runtime shell scripts with
one static binary and three declarative TOML files. The vendor kernel, bootloader, drivers,
RetroArch, and cores are reused from muOS.

## Stages

You can move a muOS install forward one stage at a time and back again.

| Stage | What runs | Install | Undo |
|---|---|---|---|
| 1 | oxmux as a muOS **application**; muOS runs everything else | `scripts/deploy.sh` | delete the app folder |
| 2 | muOS boots, but **S99muos.sh** starts `oxmux supervise` (daemon + frontend) instead of muOS's frontend, hotkey, battery and low-power scripts | `oxmux install frontend` | `oxmux uninstall` |
| 3 | busybox init's **sysinit** runs `oxmux init`; no muOS script runs at all | `oxmux install init` | `oxmux uninstall` |
| 4 | an **SD image** where oxmux is `/init` (PID 1) | `scripts/build-image.sh` | reflash |

### Safety nets

- **Preflight.** `install` checks that every config parses and every service binary
  exists before it changes anything. It refuses if one is missing.
- **Backups.** Replaced files are kept as `*.oxmux-orig`, and `oxmux uninstall` restores them.
- **Rescue.** In stages 3 and 4, a boot counter is reset only once the menu has drawn its
  first frame. After 3 boots that never reach the menu, oxmux hands over to the init it
  replaced (muOS's sysinit, or `/init.muos`). The same happens if its config fails to load.
- **Fallback frontend.** In stage 2, if the oxmux frontend crash-loops (5 times in a
  minute), the supervisor runs muOS's own frontend instead.

## What replaces what

| muOS | oxmux |
|---|---|
| `init/sysinit`, `init/S*.sh`: mounts, modules, governor, zram, bind mounts, udev | `oxmux init`, boot plan in `system.toml` (`[[init.step]]`) |
| `var/process.sh`, `mux/frontend.sh` restart loop | `oxmux supervise`: dependency-ordered services with backoff and fallbacks |
| `muhotkey` + `mux/hotkey.sh`, `device/bright.sh`, `device/audio.sh` | `oxmux daemon`: hotkeys from `[[daemon.hotkey]]`, backlight via dispdbg, volume via ALSA ioctls |
| `system/suspend.sh` + `mususpend`, `system/halt.sh` | daemon: tap power to sleep, hold to power off, RTC auto-poweroff after long sleeps |
| `mubattery`, `system/lowpower.sh`, `mux/idle.sh` | daemon: low-battery LED, clean poweroff at critical, idle dim and sleep |
| `muinput` (C) | `oxmux input-bridge`: same `muOS-Keys` identity and mapping, including rumble |
| `muxfrontend` (C/LVGL) | `oxmux frontend` (Slint) |
| GET_VAR/SET_VAR store (one file per key) | `device.toml`, `frontend.toml`, `system.toml` |
| PipeWire (in stage 3+) | not needed: RetroArch's ALSA output goes straight to the codec |

External programs still used in stage 3+: `udevd`/`udevadm` (RetroArch's udev joypad driver
needs its database), `alsactl` (restores mixer routing once), and optionally `wpa_supplicant`,
`dhcpcd` and `sshd`.

## Layout

- `src/frontend/`: Slint UI (`ui/app.slint`), ROM library, RetroArch launching, and its
  fbdev, evdev and desktop backends
- `src/daemon/`: hotkeys, power, idle, battery, and the control socket (`oxmux ctl`)
- `src/supervisor.rs`: service supervision
- `src/init/`: boot plan, module loading (modules.dep + finit_module), zram
- `src/input_bridge.rs`: muinput replacement (uinput)
- `src/hw/`: backlight, ALSA control ioctls, battery, LEDs, rumble, suspend
- `src/install.rs`: stage installer
- `config/rg40xxv/`: device profile, frontend config, and the per-stage `system-*.toml` profiles

Configs are found in `--config-dir`, `$OXMUX_CONFIG_DIR`, `etc/` next to the binary, the
binary's own directory, or `/etc/oxmux`, with built-in RG40XXV defaults otherwise.

## Develop

```bash
nix develop -c cargo sim                      # simulator window (keyboard = gamepad)
cargo test                                    # includes "every shipped config parses"
cargo run --features desktop -- --config-dir dev/etc frontend --snapshot /tmp/snap "a,down,menu"
```

In the simulator: arrows = d-pad, Z/Enter = A, X/Backspace = B, Q/W = L1/R1, Esc/Tab = Menu.

## Device runbook

Enable SSH in muOS, then:

```bash
DEVICE=root@<ip> scripts/deploy.sh            # stage 1, then start "Oxmux" from Applications
ssh root@<ip> /mnt/mmc/MUOS/application/Oxmux/oxmux probe-input   # confirm button codes
DEVICE=root@<ip> scripts/deploy.sh frontend   # stage 2 (dry run first, then install); reboot
DEVICE=root@<ip> scripts/deploy.sh init       # stage 3; reboot
```

On the device, `oxmux status` shows the current stage, `oxmux check` validates configs,
`oxmux ctl status` talks to the daemon, and logs are in `/run/oxmux/log/`.

Stage 4 needs an official muOS RG40XXV image:

```bash
nix develop -c scripts/build-image.sh MustardOS_RG40XXV_<version>.img.xz dist/oxmux-rg40xxv.img
```

## Nix image (mainline track)

The flake builds a muOS-free system from nixpkgs, with oxmux as PID 1:

```bash
nix build .#oxmux     # static aarch64 binary (prebuilt musl std from rust-overlay)
nix run .#vm          # boot it in QEMU aarch64: KMS on virtio-gpu, keyboard as gamepad
nix build .#uboot     # mainline U-Boot + TF-A for H700 (anbernic_rg35xx_h700_defconfig)
```

The VM image is the base the device image grows from. It uses the stock nixpkgs kernel
(prebuilt on cache.nixos.org), a static busybox for a serial shell, and oxmux's own driver
loading (`coldplug`: modalias → modules.alias) instead of udev. Its profile is in
`config/qemu/`. `OXMUX_VM_HEADLESS=1 OXMUX_VM_MONITOR=/tmp/mon nix run .#vm` runs it without
a window, for scripted `sendkey`/`screendump` tests.

What the RG40XXV needs beyond that (from upstream Linux, U-Boot and ROCKNIX as of 2026-09):
- **Kernel:** mainline has no RG40XX device tree and no H700 display pipeline. ROCKNIX runs
  7.2 with patches for the DE33/TCON display pipeline, the PWM backlight, the RG40XX panels
  (firmware init sequences), GPU OPPs and suspend. Their `rg40xx-v.dts` builds on mainline's
  `rg35xx-plus`.
- **Bootloader:** mainline U-Boot and TF-A are enough for LPDDR4 units. ROCKNIX also
  builds an LPDDR3 variant.
- **Userland changes:** the display output becomes `kms`, the backlight moves to
  `/sys/class/backlight`, and battery and input names change. This is a second
  `device.toml`, with no code changes.

### Sleep on either kernel

oxmux's suspend path is kernel-agnostic: `/sys/power/state` = `mem` with the wakeup_count
handshake, plus hooks. So the same userland sleeps on whichever kernel can.

- **Vendor kernel (4.9, from muOS):** suspend-to-RAM works today, and muOS uses it. Stages
  1–3 and the muOS-based stage 4 image run this kernel. A Nix image could reuse it too,
  taking the kernel, DTB and modules from the muOS image as a fixed-output input.
- **Mainline:** needs ROCKNIX's suspend patches (display and codec suspend, OPPs) plus their
  `h700-suspend-stub`, a PSCI SYSTEM_SUSPEND stub in SRAM. How well it works on the RG40XXV
  needs testing. Getting H700 suspend upstream (TF-A/crust + the driver patches) is the
  long-term route.

## Known gaps

- Nothing has run on real hardware yet. The device paths and codes come from the muOS
  source and still need confirming on a unit.
- Stage 2 skips muOS's charge-only boot mode and factory-reset hook, both of which live in S99muos.sh.
- RetroArch save-state-on-sleep (muOS sends `SAVE_STATE` before suspending) isn't done yet.
- Bluetooth, the RGB LEDs (serial MCU on ttyS5), HDMI switching and the USB gadget are not
  ported yet.
- Stage 4 still uses muOS's rootfs userland (udev, glibc, RetroArch). A rootfs built from
  scratch (e.g. with Nix) is the next step after that.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). Slint is used under its GPLv3 option.

Fonts: DejaVu Sans (Bitstream Vera / DejaVu license, free to redistribute).
