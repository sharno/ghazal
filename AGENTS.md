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
- Bluetooth controllers are implemented (docs/bluetooth.md) but the kernel's
  Bluetooth stack is off in every image (`bluetoothKernel = false` in
  `flake.nix`): see Untested features. Once it is on, verify with real pads:
  pairing and reconnecting a DualShock 4, DualSense, Switch Pro, Xbox (BLE) and
  8BitDo pad, the menu and RetroArch taking them (player order, autoconfig,
  hotkeys), the boot-time cost of the built-in stack (KERNEL +0.53 MB, ~25 ms
  expected; also `bt_init`, the ECDH self-test and the unused `start` attribute
  path) and idle power. Bluetooth audio (A2DP) is a documented plan only. Check
  that `hci_uart_h5`'s `start` attribute survives the RG40XX H and CubeXX device
  trees, and the `JustWorksRepairing = confirm` agent path with a pad that lost
  its keys.
- USB gadget modes have no standalone integration yet. Keep `USB_GADGET`: the
  OTG port's `dr_mode = "otg"` fails to probe in a host-only MUSB build, which
  also loses USB host.
- HDMI output is implemented and documented in docs/hdmi.md but has never run
  with a display attached. Its checklist is the acceptance test. Open points:
  the menu is scaled from the panel's size rather than drawn natively; a game
  keeps running on a pulled cable until Menu + Start. HDMI sound is behind
  `hdmiAudioKernel = false` in `flake.nix` (see Untested features): until a
  kernel with the audio hub has booted, HDMI carries video only and the
  handheld's speaker plays the game.
- Stick lights (RG40XX H/V, RG CubeXX) send muOS's serial frames; the daemon
  and menu path is verified, the visible colours are not yet confirmed on a
  device. Headphone switching was verified with a virtual jack device only.
- RetroAchievements and netplay: see Untested features. RetroArch facts the code
  relies on: `--connect` takes `address|port|session`, not `address:port`;
  `cheevos_hardcore_mode_enable` and `netplay_nat_traversal` default to on and
  `netplay_public_announce` to true; `menu_swap_ok_cancel_buttons` defaults to
  true (OK is the RetroPad B command); an appended config is saved into the user
  config on exit unless `config_save_on_exit` is false in it. Not done: entering
  a netplay password or a relay session from Ghazal's menu (RetroArch's own
  Netplay menu does both), and checking both ROMs' CRC before joining.
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
- Root mounting waits for every probe: keep sleeps out of probe paths. The
  Bluetooth controller's 0.6 s power-up is therefore not in the probe:
  userspace starts it through a `start` attribute (nix/hci-h5-start-attribute.patch).
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
- A release publishes only after `release-tests` and `vm-smoke` pass
  (docs/release-checks.md); a resumed publication must find both in the run.
  Both read the shipped profile, so new cores, services and emulators are covered
  without editing the checker. When a release breaks something they missed, add
  the check before fixing anything else.
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
- Before tagging, run the release checklist in docs/release-checks.md:
  `scripts/acceptance.sh GAMES_FILE` launches each listed game through the menu
  with scratch save paths and checks controls, sound, picture, clock policy, save
  states and quitting. Keep the games file out of the repository. A bind mount of
  a wrapper over the resolved RetroArch store path redirects saves without
  touching the read-only root. `ghazal observe plan|launch ROM` give it the
  menu's own button path; `observe frame` prints `distinct_colors` for blank-frame checks.
- Reproduce the CI gates with `scripts/release-tests.sh [layout|input|smoke]`
  (on macOS it uses the nixos/nix container; Docker's kernel has `/dev/uinput`).
  The shared Docker disk fills up when several builds run: keep cargo's target
  directory in a tmpfs (`--tmpfs /target`) for Rust checks.

### Release checks

- Unconfirmed on GitHub's `ubuntu-24.04-arm` runners: that `modprobe uinput`
  gives the `release-tests` input step a `/dev/uinput` (otherwise it warns and
  the layout gate is the only runtime gate). `vm-smoke` passes there (about 30 s
  in Docker's QEMU). If the runners lack uinput, run the input test inside the
  QEMU machine instead.
- `scripts/acceptance.sh` ran on the RG40XX-V v2 against a fresh image built from
  main: Game Boy, GBA, SNES, Mega Drive, PlayStation and PSP games, 69 checks
  passed and the PSP save state was skipped by design, in 2 min 7 s. The menu path
  from `observe launch`, the wrapper bind over the store path and the saves under
  `/tmp/ghazal-acceptance` all worked; nothing was left mounted or in `SAVES`
  (only History and the PPSSPP folder's mtime change). A SNES title and a PlayStation
  BIOS frame had 23 distinct colours against the 16 minimum, so a dark intro could
  trip that threshold; judge a failure there by the `--evidence` frame.
- The layout gate reads the shipped profile's `[[system.alternative]]` cores (25
  in all), Moonlight's binary and mapping, and treats `/opt/ghazal/*` and
  `/usr/bin/busybox` as image-provided. PortMaster's runtime is not checked in CI:
  `ghazal ports check` on the handheld does it. Boot services that need a BusyBox
  applet the image has no link for must call `busybox <applet>`.
- `vm-smoke` covers the `ghazal` binary on a stock kernel with `config/qemu`
  only. Booting the real image (H700 kernel, runtime, emulators) in QEMU is not
  possible; hardware acceptance stays the proof for those.

## Untested features

Features merged before hardware validation was possible, or only partly
validated. Remove an entry once it has been checked on a handheld.

- Release gates in CI (`.github/workflows/images.yml`): `vm-smoke` passed on
  GitHub's `ubuntu-24.04-arm` runner. `release-tests` has not run there: it needs
  the `runtime` job, which pushes to main keep cancelling (each push cancels the
  run in progress). The same gates passed locally on main (layout, headless input
  with the real RetroArch, QEMU smoke) and `scripts/acceptance.sh` passed on the
  handheld. Unknown: whether `ubuntu-24.04-arm` has `/dev/uinput`, and whether a
  first tag build gets through `publish` (it needs both jobs).

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
- RetroAchievements and netplay (docs/standalone.md). Never done: signing in to
  RetroAchievements (no account tonight, so no login request left the handheld
  and the TLS/DNS path of RetroArch's rc_client is unproven), an unlock popup, the
  saved token, hardcore turning off save states in a game, the wait for Wi-Fi
  after a Last Game startup with achievements on, and a full netplay session
  between two devices (needs a second handheld or desktop RetroArch 1.22.2 with
  the same core and ROM file). Checked on the RG40XX-V: Configuration shows
  Achievements (with the account) and Achievements account; Achievements cycles
  off, on, hardcore and off in the user config, and refuses without an account;
  a seed in `/var/lib/ghazal/retroarch-account.cfg` is merged into the user
  config at the next launch (mode 0600) and removed; Achievements account opens
  RetroArch on its ACHIEVEMENTS page (its key path is data, reached after two
  fixes: `lo` had to be raised, and OK is RetroArch's B command); Host netplay
  runs `--appendconfig /run/ghazal/retroarch-netplay.cfg --host --port 55435
  --nick ghazal-XXXX` and answers LAN discovery with a 688-byte reply (nickname,
  core, game, port), leaving the solo config and `.state.auto` untouched at exit;
  Join netplay lists a stand-in host (Y rescans; also from Applications, which
  finds the game by file name) and a saved host from
  `/mnt/games/.ghazal/netplay-hosts.txt`, and starts `--connect ADDRESS
  --sram-mode noload-nosave`, which fails to connect to the stand-in and returns
  to the menu with Menu + Start; a pinned core is used when hosting. The Mac's
  firewall hides its broadcast answers: probe from the handheld, not the Mac.
  The key path on a card whose RetroArch menu differs (for example hidden
  advanced settings) is untried. `observe frame` shows RetroArch's GL menu stale
  after a page change (title and footer lag the list): check the footer text.
- Bluetooth controllers (docs/bluetooth.md): nothing of it has run on a handheld,
  and the kernel part is switched off (`bluetoothKernel = false` in `flake.nix`;
  with it off the kernel is byte-for-byte the one that boots today except for its
  own store path embedded in the vDSO, and the menu hides Bluetooth). Why: the
  merged tree built (kernel, BlueZ, runtime tree, image) and passed the unit
  tests, the real-`dbus-daemon` tests, the runtime layout gate and `vm-smoke`, but
  the one image built with the Bluetooth kernel (slot B, `BOOT_B_LEFT=1`) never
  answered on the network in the 12 minutes after its first boot. That may be the
  kernel, Wi-Fi, a low battery or nothing to do with it: it was not determined.
  `vm-smoke` uses a stock kernel, so no check has run the H700 kernel with
  Bluetooth. To test it, in this order, because the kernel is what changed: set
  `bluetoothKernel = true`, build and flash slot B with `BOOT_B_LEFT=1`; check it
  boots and the boot time (~2.95 s expected, +25 ms from the bigger kernel); that
  `dmesg` shows only the core registrations and no `hci0`;
  `/sys/bus/serial/drivers/hci_uart_h5/serial0-0/start` reads 0 and no
  `bluetoothd` runs; then a game, the menu pad and Menu + Start (the udev
  `input_id` rule and the menu's hotplug code touch the built-in pad). Only then
  `ghazal bluetooth on`, the first time the RTL8821CS powers up and loads its
  firmware (a driver fault there is a kernel oops, and `panic=10` freezes this
  handheld). After that: scan, pair, reconnect and play with the pads in the
  hardware coverage entry. When it works, make the flag `true`, drop this entry
  and restore the README bullet.
- HDMI output (docs/hdmi.md): it has never run with a display, and the merged
  tree has not run on a handheld at all (the handheld was unreachable when it
  was integrated), so even the no-cable behaviour (boot time ~2.95 s, menu,
  game launch, Menu + Start, audio, the 2 s connector poll after the first
  frame) is only covered by unit tests and builds. HDMI sound's kernel part is
  switched off (`hdmiAudioKernel = false` in `flake.nix`): with it off the
  kernel carries none of the audio hub driver, configfs overlays or DT changes,
  `ghazal-hdmi-audio.dtbo` is absent from the image, and a request for HDMI
  sound fails cleanly to the speaker (`daemon.log`). Why: the kernel with the
  hub driver (ROCKNIX's vendor port plus three fixes for 7.2, never run) has not
  booted on a handheld. To test, in this order: (1) the default image, no
  cable: the regression list above and Configuration → HDMI showing "No
  display"; (2) the checklist in docs/hdmi.md with a display, video only, sound
  on the speaker; (3) set `hdmiAudioKernel = true`, build and flash slot B with
  `BOOT_B_LEFT=1`; check it boots, boot time (the Image grows by 2 KiB),
  `/proc/asound/cards` lists only the codec and no `ahub` platform device
  exists; then, attended, `ghazal ctl output panel=off sound=hdmi` applies the
  overlay (the first run of the hub driver on 7.2: check `dmesg`, then
  `/proc/asound/cards` for `HDMI` at index 1, then listen on the display).
  When it works, make the flag `true` and drop this entry.

## Keeping this backlog accurate

Remove completed items rather than carrying historical gaps forward. Legacy
installer stages are not the standalone roadmap. Missing optional features and
unvalidated hardware must be distinguished from missing implementations.
Keep installation, storage and beta-limitations docs aligned with actual behavior.
For documentation-only changes, check links and claims against the source; do
not rebuild firmware or run implementation tests just to edit prose.
