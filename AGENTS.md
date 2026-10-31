# Ghazal development

## Direction and current architecture

Ghazal is standalone: it assembles its own root filesystem and owns PID 1,
service supervision, the menu, input handling and power policy. Everything on
the device is built from pinned sources: U-Boot and the kernel
(`nix/uboot.nix`, `nix/h700-kernel.nix`) and the userspace runtime of Mesa,
RetroArch, cores, PPSSPP and system daemons (`nix/runtime`) from the flake's
nixpkgs. ROCKNIX's source tree (the flake's `rocknix` input) supplies kernel
and U-Boot patches, panel descriptions, PPSSPP's H700 defaults and the libretro
core recipes; no ROCKNIX binaries are used.

Prioritize fast boot, responsive menus, emulator performance and reliable saves.
Keep the code lean and configuration ownership explicit. Measure changes rather
than adding speculative tuning. Keep README.md focused on the product; put
technical procedures in docs/ and update this file as remaining work changes.
Never commit personal provisioning, game inventories, credentials or device-session logs.

## Remaining work

### Reliable native suspend and wake

- Suspend is unresolved and intentionally disabled in the standalone profile:
  `[power].suspend = "disabled"` and `[daemon.idle].sleep_after = 0` in
  `config/rg40xxv-rocknix/system.toml`. Idle poweroff is enabled instead.
- Establish repeated deep suspend/resume on real hardware, from both menu and
  emulator. Verify display, controls, audio, Wi-Fi, wake sources and sleep battery
  drain. A lit screen or surviving menu frame alone is not a successful resume.
- Add emulator save-state coordination before enabling sleep. Saving on normal
  emulator exit is implemented; it does not cover entering suspend.
- Implement and validate lid behavior for clamshell models before offering lid sleep.
- Use docs/kernel-debugging.md for diagnostics. Keep suspend disabled until wake
  and restored device functionality are demonstrated.
- A Linux RAM reservation does not protect logs from U-Boot's allocator. Keep
  persistent logs outside bootloader working memory, enable error correction,
  and verify a readable marker after restart; driver registration alone is
  insufficient. Validate physical Reset separately from a software restart.

### Hardware coverage and optional device features

- Charging stability is unresolved on the mainline RG40XX-V v2 configuration.
  Compare the stock PMIC/USB initialization and verify sustained USB presence
  and battery recovery; an icon or a brief `Charging` reading is insufficient.
  Do not present the charging indicator as a charging-driver fix. Collect a
  continuous sample over SSH (USB online, VBUS, status, capacity every 20 s).
  A 30-minute sample with the AXP717 fixes stayed online and charging
  (63→80% over 70 minutes, constant-voltage taper from ~69%); an overnight
  run then reached full and held it while on. The charge current now matches
  the vendor's 1.5 A; its 4.1 V target is not copied (mainline charges to
  4.2 V). A full AXP717 reports `Not charging` (never `Full`), so external
  power comes from the USB supply's `online`; `daemon.log` records when it
  comes and goes.
- USB-C-to-USB-C chargers cannot charge the RG40XX-V v2; this is hardware.
  The board shows no Rd on CC, so a USB-C source never turns VBUS on. The
  AXP717's CC pins do not reach the port: the vendor's CC setup (`0x0b`
  bit 3, `0xe3` sink or DRP) reports no attach, and with it on and the
  handheld's VBUS blocked a fresh plug-in still got no VBUS. Charge from
  USB-A through an A-to-C cable.
- The OTG ID line (PI4) reads a USB-C source's Rp as a USB device, and
  driving VBUS into the source drops it again. Without
  `nix/sun4i-usb-phy-typec-source.patch` the port toggles the 5 V boost about
  once a second while a C-to-C charger is plugged in. With it, a charger gets
  one ~0.1 s pulse per plug-in (verified on the RG40XX-V v2). Not yet
  checked: an OTG device still gets VBUS (it must keep ID asserted for the
  first second).
- CI builds all 19 catalog variants with both RAM bootloaders, but physical
  validation currently covers RG40XX-V v2 / DDR4. Validate other boards and
  panels: boot, display, controls, audio, storage, networking where present,
  game launch/exit, save retention and clean shutdown.
- Bluetooth pairing/audio and USB gadget modes have no standalone integration
  yet. Bluetooth is also trimmed from the production kernel; adding it needs an
  explicit kernel and userspace plan that preserves startup performance. Keep
  `USB_GADGET`: the OTG port's `dr_mode = "otg"` fails to probe in a host-only
  MUSB build, which also loses USB host.
- HDMI output is implemented and documented in docs/hdmi.md but has never run
  with a display attached. Its checklist is the acceptance test. Open points:
  the menu is scaled from the panel's size rather than drawn natively; the
  HDMI sound card comes from ROCKNIX's vendor audio-hub driver, ported to 7.2
  only far enough to build, and is created on first use through a configfs
  device-tree overlay (kernel patches in `nix/`) so it cannot take card 0 or
  add boot time; a game keeps running on a pulled cable until Menu + Start.
  The kernel Image grew by 2 KiB (15,550,472 to 15,552,520 bytes); measure the
  boot time with `ghazal observe boot` on a clean-shutdown boot.
- Stick lights (RG40XX H/V, RG CubeXX) send muOS's serial frames; the daemon
  and menu path is verified, the visible colours are not yet confirmed on a
  device. Headphone switching was verified with a virtual jack device only.
- Validate charge-only startup on hardware. U-Boot already includes the AXP717
  charge-only patch (`nix/uboot.nix`); do not describe it as wholly unimplemented.
- There is no user-facing factory-reset flow. If added, define which OS settings
  it resets and preserve games, saves and identities unless explicitly selected.

### Boot time and game performance

- `ghazal observe boot` reports firmware, kernel-to-frame and total time; the
  kernel log holds each service's start. See docs/standalone.md for the
  breakdown. Measure on a boot whose previous shutdown was clean: a journal
  replay on `/var` or the root adds ~100 ms.
- Firmware time is mostly U-Boot reading the kernel; size it with object sizes
  from a kept build tree (`ar t built-in.a | xargs size`), not thin archives.
  U-Boot's own ~1.4 s (SPL, DRAM, environment) and its slow decompression look
  like caches being off; changing U-Boot needs a spare card.
- Root mounting waits for every probe: keep sleeps out of probe paths.
  Userspace time is mostly paging in the ghazal binary (~6.5 MB of 10.6);
  shrinking it or improving code locality is the next lever. Read-ahead tuning
  did not help.
- Anything not needed for the first frame belongs in the supervisor's
  background phase. Emulators must wait for what they need through
  `launch_after` rather than assume boot ordering.
- Build repeatable observations for menu latency, game launch/exit, frame pacing,
  audio and save/resume. Use the desktop simulator and `ghazal observe` for fast
  iteration, then confirm on-device behavior. QEMU cannot prove H700 driver or
  suspend correctness. Document benchmark conditions instead of universal speed claims.
- Per-system CPU floors (`cpu` in `frontend.toml`) come from one game's intro
  per system: unthrottled speed and null-video emulation time (RetroArch's
  `core_run` perf counters stay empty). On the RG40XX-V, GB, NES, MD, SNES
  (including SuperFX Doom), GBA and MAME 2003-Plus held 60 fps at their floors
  and spent ~80% of the time there instead of at 1416 MHz. The tested unit
  keeps 0.9 V up to 1008 MHz and needs 1.1 V at 1320–1416 MHz; other speed
  bins differ (`vdd-cpu` microvolts per step). Without `UCLAMP_TASK`,
  schedutil jumps to the top clock whenever a realtime task is runnable: in a
  game the Wi-Fi SDIO and Panfrost job IRQ threads wake ~280 times a second,
  which kept 13–22% of the time at 1416 MHz. A zero RT clamp floor
  (system.toml) cut that to 0% with 60 fps held for GB, NES, SNES and GBA.
  The AXP717 has no discharge current reading, so measure draw as charge lost
  over equal long runs (`ghazal observe power`).
- Check status icons with the desktop snapshot power override at normal and
  compact sizes (docs/standalone.md). The software renderer has no Path support
  unless explicitly enabled; embedded-font symbols avoid adding that dependency.

### Updates and build independence

- RAUC A/B updates are implemented; see docs/updates.md for completed hardware
  checks and migration requirements. Still validate deliberate power removal
  during installation/environment writes and other boards. A controlled SSH
  migration requires a verified raw backup and a compatible actual disk layout;
  it must never become a blind automatic repartitioning step.
- Publish existing release tags with `gh release create --verify-tag`, without
  `--target`: a stale target that differs from main's workflows can require
  workflow-write permission unavailable to GITHUB_TOKEN. Resume asset uploads
  only on drafts; never overwrite an already published release in CI.
- Classify user-volume failures separately from OS faults. A missing/full data
  card is shared by both slots: retain Wi-Fi/SSH recovery instead of exhausting
  both boot budgets. Keep this distinction through anyhow boot-step context.
- Panics and emergency restarts (`echo b > /proc/sysrq-trigger`) freeze the
  RG40XX-V v2 with the last frame on screen: neither `panic=10` nor the 16 s
  watchdog the health service feeds resets it, while normal reboots work.
  Both skip the reboot notifiers and `device_shutdown`; the boot cards run at
  3.3 V high speed, so it is not a UHS card left at 1.8 V. Find what the
  shutdown path does that the emergency path needs (the PSCI reset in TF-A,
  or the watchdog's reboot notifier) with a debug kernel and the Reset button.
- The kernel starts the watchdog at driver probe (~0.2 s) and resets the board
  if userspace never opens it within a minute; this was proven on hardware.
  U-Boot still leaves it off, so hangs before the probe need Reset. Arming it
  in U-Boot is a bootloader change: test it on a spare card first.
- Keep shared settings migrations compatible with the previous OS slot.
- A process cannot mix two glibcs: Mesa, SDL2, RetroArch, PPSSPP and every
  core must come from the one nixpkgs in `nix/runtime`, and programs must find
  libraries through their store RPATHs, never `/usr/lib`. Stock nixpkgs
  graphics closures do not fit the 1 GiB slot; keep overrides free of X11,
  Wayland, LLVM, Vulkan and FFmpeg, and check the root's size after bumps.
  Cores follow ROCKNIX's recipes (`nix/runtime/rocknix-core.nix`): after a
  `rocknix` bump, restate changed package.mk conditionals and hooks in
  `cores.nix`, launch every system and compare core speed against the previous
  build (docs/standalone.md, Runtime).
- Exercise application writes with the root mounted read-only. RetroArch core
  options, its home and caches belong on the selected data card; `/root` must
  not be assumed writable just because launching and saving a game succeeded.
- The Allwinner SPL at 8 KiB overlaps a conventional primary GPT entry array.
  Preserve the MBR/extended layout unless bootloader placement is redesigned.
- Check resolved U-Boot Kconfig and the on-device environment reader together.
  U-Boot 2026 uses `ENV_REDUNDANT`; the old `SYS_REDUNDAND_ENVIRONMENT` fragment
  is silently ignored and produces a different environment header. Both the
  bootloader derivation and image builder must reject mismatched bank settings.
- Make image packaging byte-reproducible if required: filesystem UUIDs and
  timestamps currently vary despite pinned inputs. Keep machine identity and
  SSH host keys generated per device on first boot.

### Development loop on a handheld

- Build the static binary on any host with `cargo build --release --target
  aarch64-unknown-linux-musl` (rust-lld; see .cargo/config.toml). Unit tests and
  the desktop build need Linux.
- Test on the inactive slot: mount it (or remount the booted B root
  read-write and rename files into place), then `fw_setenv BOOT_ORDER "B A"`.
  Keep slot A as the known-good fallback and use `BOOT_B_LEFT=1` for kernels.
- An open SSH session postpones idle poweroff, so keep one open during long
  unattended work. BusyBox lacks many applet links (`sed`, `pidof`,
  `readlink`, `od`): use `busybox <applet>`.

## Keeping this backlog accurate

Remove completed items rather than carrying historical gaps forward. Legacy
installer stages are not the standalone roadmap. Missing optional features and
unvalidated hardware must be distinguished from missing implementations.
Keep installation, storage and beta-limitations docs aligned with actual behavior.
For documentation-only changes, check links and claims against the source; do
not rebuild firmware or run implementation tests just to edit prose.
