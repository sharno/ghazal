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
