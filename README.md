# Ghazal

<img src="assets/ghazal.svg" width="96" alt="Ghazal gazelle logo">

Ghazal is a standalone Linux gaming system for Anbernic H700 handhelds.

## H700 beta images

[Downloads](https://github.com/sharno/ghazal/releases) ·
[Install on one or two SD cards](docs/installation.md) ·
[File structure](docs/storage-layout.md) ·
[Supported device and panel variants](config/h700/devices.toml)

The release workflow builds every catalog entry with DDR3 and DDR4 bootloaders.
Images select their device tree automatically and use TF2 for data when present,
otherwise the TF1 data partition. Public images contain no personal credentials.
Only the RG40XX-V v2 / DDR4 target has physical acceptance so far;
[beta limitations](docs/beta-release.md) distinguish build coverage from hardware tests.

## Standalone RG40XX-V image

The standalone target now assembles its own ext4 root filesystem and boots
ghazal as PID 1, from a repository-built U-Boot and kernel with every driver
built in (no initramfs, no modules). A checksum-pinned ROCKNIX H700 bundle
supplies firmware, RetroArch/cores and their library dependencies. No muOS scripts or
EmulationStation are used. SSH, Wi-Fi with DHCP/DNS and persistent service logs
are included. See [the standalone build and device runbook](docs/standalone.md).

```sh
nix run .#image -- "$PWD" "$PWD/secrets" "$PWD/build/ghazal-rg40xxv.img"
```

This targets DDR4 boards and has booted on an RG40XX-V with the v2 panel.
Display, controls, networking, sample emulator launches and persistent save states
have been checked on that device. Native suspend is disabled pending reliable wake.
The older muOS stages below are retained as legacy development tools.

### Brightness and volume

Volume Up/Down adjusts sound. Hold Menu while pressing Volume Up/Down to
adjust brightness. In the ghazal frontend, both levels appear as percentage
labels and bars for three seconds after a change, and remain visible while
the menu is open. These indicators do not overlay a running emulator.
The standalone profile reads the kernel's backlight range automatically.

### Main menu

The home screen follows the muOS navigation structure: Explore Content,
Collection, History, Applications, Information, Configuration, Reboot and
Shutdown. Explore Content lists systems; Collection contains favorites and
History contains recent games. Applications contains library rescan and
thumbnail download tools. Configuration offers box art, brightness and volume;
Information shows the software, kernel, LAN address and library totals.
Power actions appear when the daemon is available. B returns from a category;
on the home screen it selects Explore Content.

### Startup

In **Configuration → Startup**, choose **Main Menu** (default) or **Last Game**.
The choice is saved on the OS card and applies on the next boot. Last Game
starts the most recently launched game from the active data card's history;
it does not automatically load a save state. Exiting the emulator returns to
Main Menu. Missing games, missing BIOS/cores and launch failures leave the menu
available, and frontend restarts do not repeat the automatic launch that boot.
Interacting with the menu while its initial library scan runs cancels autostart.
To disable it from a computer, set `startup = "menu"` in
`/var/lib/ghazal/frontend-settings.toml` on the OS partition (`GHAZALROOT`).

A Rust userland for the Anbernic RG40XXV (Allwinner H700), built as a muOS alternative.
It replaces muOS's frontend and its ~21,600 lines of boot and runtime shell scripts with
one static binary and three declarative TOML files. The standalone image builds its
bootloader and kernel from ROCKNIX's pinned H700 sources and reuses its runtime bundle. The legacy muOS migration tools retain
their vendor kernel, drivers and emulator binaries.

## Stages

You can move a muOS install forward one stage at a time and back again.

| Stage | What runs | Install | Undo |
|---|---|---|---|
| 1 | ghazal as a muOS **application**; muOS runs everything else | `scripts/deploy.sh` | delete the app folder |
| 2 | muOS boots, but **S99muos.sh** starts `ghazal supervise` (daemon + frontend) instead of muOS's frontend, hotkey, battery and low-power scripts | `ghazal install frontend` | `ghazal uninstall` |
| 3 | busybox init's **sysinit** runs `ghazal init`; no muOS script runs at all | `ghazal install init` | `ghazal uninstall` |
| 4 | a standalone **SD image** where ghazal owns PID 1 | `nix run .#image -- ...` | reflash |

### Safety nets

- **Preflight.** `install` checks that every config parses and every service binary
  exists before it changes anything. It refuses if one is missing.
- **Backups.** Replaced files are kept as `*.ghazal-orig`, and `ghazal uninstall` restores them.
- **Rescue.** The standalone image records boot attempts durably and resets the counter
  after the menu draws its first frame. Three incomplete boots enter a built-in Wi-Fi/SSH
  recovery supervisor. Startup configuration and supervisor errors enter recovery directly.
  Legacy muOS installs retain their original-init recovery path.
- **Fallback frontend.** In stage 2, if the ghazal frontend crash-loops (5 times in a
  minute), the supervisor runs muOS's own frontend instead.

## What replaces what

| muOS | ghazal |
|---|---|
| `init/sysinit`, `init/S*.sh`: mounts, modules, governor, zram, bind mounts, udev | `ghazal init`, boot plan in `system.toml` (`[[init.step]]`) |
| `var/process.sh`, `mux/frontend.sh` restart loop | `ghazal supervise`: dependency-ordered services with backoff and fallbacks |
| `muhotkey` + `mux/hotkey.sh`, `device/bright.sh`, `device/audio.sh` | `ghazal daemon`: hotkeys from `[[daemon.hotkey]]`, backlight via dispdbg, volume via ALSA ioctls |
| `system/suspend.sh` + `mususpend`, `system/halt.sh` | daemon: tap power to sleep, hold to power off, RTC auto-poweroff after long sleeps |
| `mubattery`, `system/lowpower.sh`, `mux/idle.sh` | daemon: low-battery LED, clean poweroff at critical, idle dim and sleep |
| `muinput` (C) | `ghazal input-bridge`: same `muOS-Keys` identity and mapping, including rumble |
| `muxfrontend` (C/LVGL) | `ghazal frontend` (Slint) |
| GET_VAR/SET_VAR store (one file per key) | `device.toml`, `frontend.toml`, `system.toml` |
| PipeWire (in stage 3+) | not needed: RetroArch's ALSA output goes straight to the codec |

External programs still used in stage 3+: `udevd`/`udevadm` (RetroArch's udev joypad driver
needs its database), `alsactl` (restores mixer routing once), and optionally `wpa_supplicant`,
`dhcpcd` and `sshd`.

## Layout

- `src/frontend/`: Slint UI (`ui/app.slint`), ROM library, RetroArch launching, and its
  fbdev, evdev and desktop backends
- `src/daemon/`: hotkeys, power, idle, battery, and the control socket (`ghazal ctl`)
- `src/supervisor.rs`: service supervision
- `src/init/`: boot plan, module loading (modules.dep + finit_module), zram
- `src/input_bridge.rs`: muinput replacement (uinput)
- `src/hw/`: backlight, ALSA control ioctls, battery, LEDs, rumble, suspend
- `src/install.rs`: stage installer
- `config/rg40xxv/`: device profile, frontend config, and the per-stage `system-*.toml` profiles

Configs are found in `--config-dir`, `$GHAZAL_CONFIG_DIR`, `etc/` next to the binary, the
binary's own directory, or `/etc/ghazal`, with built-in RG40XXV defaults otherwise.

## Develop

Keep personal provisioning, game inventories and device-session logs outside
this repository. Documentation and release attachments describe shared behavior.
Use the ignored `secrets/` directory for local image provisioning inputs.

```bash
nix develop -c cargo sim                      # simulator window (keyboard = gamepad)
cargo test                                    # includes "every shipped config parses"
cargo run --features desktop -- --config-dir dev/etc frontend --snapshot /tmp/snap "a,down,menu"
```

In the simulator: arrows = d-pad, Z/Enter = A, X/Backspace = B, Q/W = L1/R1, Esc/Tab = Menu.

## Device runbook

Enable SSH in muOS, then:

```bash
DEVICE=root@<ip> scripts/deploy.sh            # stage 1, then start "Ghazal" from Applications
ssh root@<ip> /mnt/mmc/MUOS/application/Ghazal/ghazal probe-input   # confirm button codes
DEVICE=root@<ip> scripts/deploy.sh frontend   # stage 2 (dry run first, then install); reboot
DEVICE=root@<ip> scripts/deploy.sh init       # stage 3; reboot
```

On the device, `ghazal status` shows the current stage, `ghazal check` validates configs,
`ghazal ctl status` talks to the daemon, and logs are in `/run/ghazal/log/`.

The legacy muOS-based image builder needs an official muOS RG40XXV image. It is
separate from the standalone `nix run .#image` recipe:

```bash
nix develop -c scripts/build-image.sh MustardOS_RG40XXV_<version>.img.xz dist/ghazal-rg40xxv.img
```

## Nix image (mainline track)

The flake builds a muOS-free system from nixpkgs, with ghazal as PID 1:

```bash
nix build .#ghazal     # static aarch64 binary (prebuilt musl std from rust-overlay)
nix run .#vm          # boot it in QEMU aarch64: KMS on virtio-gpu, keyboard as gamepad
nix build .#uboot     # mainline U-Boot + TF-A for H700 (anbernic_rg35xx_h700_defconfig)
```

The VM exercises the same userland on virtual hardware. It uses the stock nixpkgs kernel
(prebuilt on cache.nixos.org), a static busybox for a serial shell, and ghazal's own driver
loading (`coldplug`: modalias → modules.alias) instead of udev. Its profile is in
`config/qemu/`. `GHAZAL_VM_HEADLESS=1 GHAZAL_VM_MONITOR=/tmp/mon nix run .#vm` runs it without
a window, for scripted `sendkey`/`screendump` tests.

The standalone handheld image supplies the following hardware components:
- **Kernel:** mainline has no RG40XX device tree and no H700 display pipeline. ROCKNIX runs
  7.2 with patches for the DE33/TCON display pipeline, the PWM backlight, the RG40XX panels
  (firmware init sequences), GPU OPPs and suspend. Their `rg40xx-v.dts` builds on mainline's
  `rg35xx-plus`.
- **Bootloader:** mainline U-Boot and TF-A are enough for LPDDR4 units. ROCKNIX also
  builds an LPDDR3 variant.
- **Userland changes:** the display output becomes `kms`, the backlight moves to
  `/sys/class/backlight`, and battery and input names change. This is a second
  `device.toml`, with no code changes.

### Suspend policy

ghazal's suspend path is kernel-agnostic: `/sys/power/state` = `mem` with the wakeup_count
handshake, plus hooks. So the same userland sleeps on whichever kernel can.

- **Vendor kernel (4.9, from muOS):** legacy stages 1–3 and the legacy image retain
  muOS's vendor suspend path. These were not part of the standalone acceptance run.
- **Mainline:** needs ROCKNIX's suspend patches (display and codec suspend, OPPs) plus their
  `h700-suspend-stub`, a PSCI SYSTEM_SUSPEND stub in SRAM. How well it works on the RG40XXV
  needs testing. Getting H700 suspend upstream (TF-A/crust + the driver patches) is the
  long-term route.

## Known gaps

- The standalone image runs on an RG40XX-V with the v2 panel: menu, controls,
  Wi-Fi, SSH, SD2 discovery and Syncthing are confirmed. Native suspend remains
  unresolved and is disabled
  in the standalone profile, including automatic sleep and the Power-button tap.
- Stage 2 skips muOS's charge-only boot mode and factory-reset hook, both of which live in S99muos.sh.
- RetroArch save-state-on-sleep (muOS sends `SAVE_STATE` before suspending) isn't done yet.
- Bluetooth, the RGB LEDs (serial MCU on ttyS5), HDMI switching and the USB gadget are not
  ported yet.
- The standalone rootfs uses a checksum-pinned ROCKNIX hardware/runtime bundle,
  with ghazal as PID 1 and its own service plan. The legacy stage-4 script remains
  available for muOS migration.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). Slint is used under its GPLv3 option.

Fonts: DejaVu Sans (Bitstream Vera / DejaVu license, free to redistribute).
