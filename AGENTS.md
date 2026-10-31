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
- Bluetooth pairing/audio, HDMI hotplug with display/audio switching and USB
  gadget modes have no standalone integration yet. Bluetooth is also trimmed
  from the production kernel; adding it needs an explicit kernel and userspace
  plan that preserves startup performance. HDMI audio needs the disabled H616
  AHUB driver. Keep `USB_GADGET`: the OTG port's `dr_mode = "otg"` fails to
  probe in a host-only MUSB build, which also loses USB host.
- Stick lights (RG40XX H/V, RG CubeXX) send muOS's serial frames; the daemon
  and menu path is verified, the visible colours are not yet confirmed on a
  device. Headphone switching was verified with a virtual jack device only.
- RetroAchievements and netplay are implemented but unvalidated on a handheld
  (docs/standalone.md, RetroAchievements and netplay). Check: sign-in over Wi-Fi
  and an unlock toast in a game; the first-boot account merge from
  `provision.toml`; Configuration → Achievements account lands on RetroArch's
  Achievements page (the key path in `[retroarch.menu_paths]` was derived from
  1.22's menu source: Restart must be listed, else one Up fewer); hosting and
  joining between two handhelds by broadcast discovery, by `netplay-hosts.txt`
  and from Applications; the wait for Wi-Fi after a Last Game startup. RetroArch
  facts the code relies on: `--connect` takes `address|port|session`, not
  `address:port`; `cheevos_hardcore_mode_enable` and `netplay_nat_traversal`
  default to on and `netplay_public_announce` to true; an appended config is
  saved into the user config on exit unless `config_save_on_exit` is false in it.
  Not done: entering a netplay password or a relay session from Ghazal's menu
  (RetroArch's own Netplay menu does both), and checking both ROMs' CRC before joining.
- Validate charge-only startup on hardware. U-Boot already includes the AXP717
  charge-only patch (`nix/uboot.nix`); do not describe it as wholly unimplemented.
- Loopback: nothing raised `lo`, so 127.0.0.1 failed with `Cannot assign
  requested address` (sshd `-R`, the Achievements account helper). A background
  `loopback` oneshot (`ip link set lo up`, system.toml) now does it; confirm
  after a reboot that `ip addr show lo` has 127.0.0.1 and `ssh -R` works.
- There is no user-facing factory-reset flow. If added, define which OS settings
  it resets and preserve games, saves and identities unless explicitly selected.

### Moonlight game streaming

- The whole flow is implemented (docs/moonlight.md) and its menu side has run
  on a handheld against `tools/fake-host`, but no stream has run: there was no
  host. Pair with a real Sunshine host, then check the SDL KMSDRM window and
  letterboxed scaling, audio (Moonlight's 4096-sample buffer is ~85 ms), the pad
  mapping, Menu + Start ending a stream cleanly, and whether Sunshine answers
  the mDNS browse (it was found by the handheld's own browse here: an unpaired
  `sharno-desktop`, but a multicast-only responder would not be).
- Decoding is software H.264 on the four A53 cores. Measured on the handheld
  (top clock, `decode-bench`, four slice threads): 640x360@60 takes 2.9 ms per
  frame (p99 3.6), 720p60 7.0 ms (p99 8.5), against a 16.7 ms budget; with a
  single slice per frame 720p60 takes 21 ms and cannot hold. Keep the 640x360
  default until a real stream shows the YV12 upload, audio and Wi-Fi do not eat
  the headroom; then try 1280x720 and whether Sunshine sends four slices.
- Hardware decoding is not available here: `MEDIA_SUPPORT` is off and the H700
  device tree has no Cedrus node. Revisit it only if measured software
  decoding cannot hold the target; it needs the kernel driver, a DT node, V4L2
  request decoding and a DRM-PRIME output in Moonlight's SDL path.
- Moonlight and its FFmpeg add ~3.9 MB to the root (`nix path-info` against
  the device's `/nix/store`; measured 582.4 to 586.1 MB used); check the root's
  free space after bumps.

### PortMaster

- PortMaster support (docs/ports.md, `src/ports`, `nix/runtime/ports.nix`,
  `config/*/ports`) has run on an RG40XX-V v2: install over SSH and from the menu,
  PortMaster 2026.10's GUI drawing and taking D-pad and button input over SDL
  KMSDRM, Menu + Start leaving nothing running, a port script run from Ports with
  SDL2/GLES, gptokeyb's virtual keyboard, a tone (PCM `RUNNING`), squashfs runtimes
  (gzip, zstd and xz all mount), Love2D's runtime drawing PortMaster's splash with
  sound, and the `[ERROR]` toast for a failing script. No port from PortMaster's
  catalogue has run, and a person has not judged the controls. Still to check on
  the handheld: download and run a small SDL2/GLES port, a gptokeyb mouse port, a
  mono or Godot runtime port and a Love2D port; PortMaster's downloads and Options
  (including its disclaimer); `ANALOG_STICKS` for this board (ROCKNIX's
  table says 1) and whether face buttons feel right in ports.
- Facts learned on the device, keep them in mind when bumping PortMaster:
  `pm_launch.txt` starts a Love2D splash that holds the DRM master, which makes
  pugwash's SDL fail with "No available video device", so `mod_Ghazal.txt` drops
  that one `env` call; PortMaster's zip stores `gptokeyb` 0644 (install and every
  run make files executable); bash's `<(...)` needs `/dev/fd` (linked at boot); the
  Love2D runtime needs freetype, OpenAL, mpg123, Ogg, Vorbis and Theora from the
  system. libtheora 1.2 names its decoder `libtheoradec.so.2` and Love links `.so.1`:
  it is aliased, and Love's `.ogv` video playback is untested.
- Not supported, by design: 32-bit (armhf) ports (kernel `COMPAT` is off; needs
  an armhf userland), X11/Wayland/sway ports, Java, box86/box64, desktop OpenGL
  without the port's own gl4es, rotated panels (RG28XX).
- Size: root used went from 586.1 to 660.3 MB of 973 (245.9 MB free), the runtime
  closure is 657.9 MiB. Nearly all of the growth is Python 3.14 (68 MB, built from
  source: no LTO, no SQLite/readline/gdbm/tests), plus SDL2_image/SDL2_mixer and the
  Love2D libraries. Boot stays at 2.92 s with PortMaster installed, since nothing
  starts at boot.
- A port that needs a library the compat directory lacks: find it with
  `ghazal ports ldd`, add it to `nix/runtime/ports.nix` and `SONAMES` in
  `src/ports/check.rs`.

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
- Emulator choice (X in a game list, docs/standalone.md) is checked on the
  RG40XX-V v2 for pin, system default, restore, persistence across a reboot,
  Last Game startup with a pinned core, History and Collection lists, damaged
  and unknown-version `emulators.toml`, and one launch with every shipped
  alternative that has games on the dev card (frame, PCM running, Menu+Start
  back). Boot stayed 2.92 to 2.95 s. The alternatives' `cpu` floors reuse their
  default's and are unmeasured: time each with the null-video method before
  trusting them (PicoDrive ran at the moderate floor and reached 1104 MHz in
  an intro). Saves and states are filed per core name (`SAVES/file/VBA Next`,
  not `mGBA`), so switching cores starts without the game's battery save;
  copying `.srm` files between compatible cores is not done. Not built: gpsp
  (ROCKNIX's H700 build is 32-bit ARM with a 32-bit dynarec; a generic aarch64
  build has no dynarec) and PS1 alternatives (`beetle-psx-lr`, `swanstation-lr`).
  The picker shows `Default (PPSSPP (standalone))` with nested parentheses.
- With a menu overlay open (the Start menu or the emulator picker), `ghazal
  observe frame` shows the whole screen shifted about 26 px left and wrapped.
  The commit before emulator choice (ca9352d) does it too. `present()` copies
  rows at the damage's x, so look at Slint's output and the scanout buffer
  before blaming the panel; check whether the panel itself shows it.
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
  Wayland, LLVM and Vulkan, and check the root's size after bumps. FFmpeg is
  only the H.264 decoder (`nix/runtime/ffmpeg.nix`): FFmpeg's configure
  applies options in order, so `--enable-*` must follow `--disable-everything`.
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
- In a nix container, `nix develop` makes rustup unpack a ~1.5 GB toolchain
  into every fresh container, and parallel runs fill the shared Docker disk
  (builds then fail with ENOSPC, even `docker run`). Keep `CARGO_HOME` and the
  target dir in named volumes and take the toolchain from nix instead:
  `nix shell --inputs-from . nixpkgs#cargo nixpkgs#rustc nixpkgs#clippy
  nixpkgs#gcc -c cargo test`, with `CARGO_PROFILE_DEV_DEBUG=0` to keep the
  target small. Tests and both clippy runs pass that way (CI uses rustup's
  stable). `iwctl` blocks while iwd is down, so time-limit any probe of it.
- Test on the inactive slot: mount it (or remount the booted B root
  read-write and rename files into place), then `fw_setenv BOOT_ORDER "B A"`.
  Keep slot A as the known-good fallback and use `BOOT_B_LEFT=1` for kernels.
- An open SSH session postpones idle poweroff, so keep one open during long
  unattended work. BusyBox lacks many applet links (`sed`, `pidof`,
  `readlink`, `od`): use `busybox <applet>`.

## Untested features

Features merged before hardware validation was possible, or only partly
validated. Remove an entry once it has been checked on a handheld.

- Emulator choice: the Mega CD alternative (the dev card has no `bios_CD_*.bin`,
  so the system is hidden), the rotated-panel image profile that keeps only the
  libretro PSP core (unit test only, no RG28XX), and the sound and frame pacing
  of every alternative beyond one captured intro frame with PCM running. The
  Snes9x 2010, VBA-Next and Nestopia cores were built from ROCKNIX recipes and
  have only been run for a few seconds each.
- Moonlight game streaming: no stream against a real Sunshine or GeForce host
  (needs a PC and a person to type the PIN): pairing itself, the SDL window and
  GLES YV12 renderer, audio, pad mapping, letterboxing, Menu + Start during a
  stream, idle-poweroff inhibition during a stream and 720p performance. Checked
  on the handheld with `tools/fake-host`: mDNS search, add host by IP, the PIN
  screen and `-pin` helper (and B killing it), the host's app list in order,
  every stream setting persisting, a failed stream
  returning to a working menu, SDL's platform initialization on KMSDRM, Menu +
  Start ending a stream in under a second, and the governor returning.
  Unpair and Remove from list (against a real pairing) and keyboard and mouse
  passthrough for the Desktop app are untried.
- PortMaster: the install flow, GUI, runner, gptokeyb, squashfs and Love2D runtime
  were exercised with PortMaster's own tools and small test scripts, never with a
  port from its catalogue. Not tried: any real port, PortMaster's downloads and
  Options (including accepting its disclaimer), Love2D video (`.ogv`), the controller
  feel and `ANALOG_STICKS` (1 from ROCKNIX's table). The card's old stock-OS scripts in
  `ROMS/PORTS` list under Ports and fail when launched (they name `/mnt/vendor/...`).
- Wi-Fi setup (Configuration → Wi-Fi, docs/wifi.md, with the reusable
  on-screen keyboard in `ui/keyboard.slint`). Checked on the RG40XX-V over its
  own SSH link (only that one saved network, two neighbours' networks in range,
  so nothing was joined): the list with bars and WPA/Open, scan (`Scanning...`
  then a refreshed list), the keyboard for an unsaved network (dots, reveal,
  shift layer, "at least 8 characters", Menu cancels), the hidden-name keyboard,
  the forget and disconnect confirms (default No, B leaves them alone), the
  worker thread existing only while the screen is open, Menu opening the Start
  menu from the list, a game launching afterwards, and iwd stopped with SIGSTOP
  (the link survives; opening the screen shows "Wi-Fi service is not running"
  after 8 s and Try again recovers; with the screen already open a scan only
  toasts "Scan failed: Wi-Fi did not answer in time" and keeps the list).
  Never run on hardware: joining (open or WPA), a refused password (iwd's
  wording for it is still the guess "Operation failed"), cancelling mid-connect,
  forgetting and disconnecting for real, a hidden network that exists, and a
  powered-off adapter. iwctl on the device says `Invalid network name 'X'` for
  an unknown name and `No Agent registered` when a passphrase is needed but
  `--dont-ask` is set (both mapped). Not done: enterprise networks, a radio
  switch, non-ASCII input.

## Keeping this backlog accurate

Remove completed items rather than carrying historical gaps forward. Legacy
installer stages are not the standalone roadmap. Missing optional features and
unvalidated hardware must be distinguished from missing implementations.
Keep installation, storage and beta-limitations docs aligned with actual behavior.
For documentation-only changes, check links and claims against the source; do
not rebuild firmware or run implementation tests just to edit prose.
