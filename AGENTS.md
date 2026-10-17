# Ghazal development

## Direction and current architecture

Ghazal is already standalone: it assembles its own root filesystem and owns
PID 1, service supervision, the menu, input handling and power policy. The
repository builds U-Boot and the kernel from pinned sources. Firmware,
emulators and runtime libraries currently come from the checksum-pinned
ROCKNIX bundle recorded in `hardware.lock`; its operating-system service
framework does not run. Reusing those components does not make Ghazal a
frontend that needs another installed OS.

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

### Hardware coverage and optional device features

- CI builds all 19 catalog variants with both RAM bootloaders, but physical
  validation currently covers RG40XX-V v2 / DDR4. Validate other boards and
  panels: boot, display, controls, audio, storage, networking where present,
  game launch/exit, save retention and clean shutdown.
- Bluetooth pairing/audio, HDMI hotplug with display/audio switching, USB gadget
  modes and model-specific RGB LED control have no standalone integration yet.
  Bluetooth is also trimmed from the production kernel; adding it needs an
  explicit kernel and userspace plan that preserves startup performance.
- Validate charge-only startup on hardware. U-Boot already includes the AXP717
  charge-only patch (`nix/uboot.nix`); do not describe it as wholly unimplemented.
- There is no user-facing factory-reset flow. If added, define which OS settings
  it resets and preserve games, saves and identities unless explicitly selected.

### Boot time and game performance

- Track power-on to first usable menu separately from kernel uptime to first
  frame. Existing instrumentation writes `/run/ghazal/first-frame-ready`; see
  docs/standalone.md for the boot breakdown and measurement method.
- Reduce bootloader/kernel loading and probing costs using measured results.
  Preserve recovery and keep library scanning and background services from
  delaying usable navigation.
- Build repeatable observations for menu latency, game launch/exit, frame pacing,
  audio and save/resume. Use the desktop simulator and `ghazal observe` for fast
  iteration, then confirm on-device behavior. QEMU cannot prove H700 driver or
  suspend correctness. Document benchmark conditions instead of universal speed claims.

### Updates and build independence

- OS updates currently require reflashing. Design a recoverable update path
  that preserves user data and settings, including one-card installations;
  automatic updates and in-place OS upgrades are not implemented.
- Further build independence means packaging emulators and runtime libraries
  from pinned sources rather than extracting the binary bundle. Preserve firmware
  provenance and licensing, and validate performance before replacing working binaries.
  This is future supply-chain work, not a missing standalone boot stage.
- Make image packaging byte-reproducible if required: filesystem UUIDs and
  timestamps currently vary despite pinned inputs. Keep machine identity and
  SSH host keys generated per device on first boot.

## Keeping this backlog accurate

Remove completed items rather than carrying historical gaps forward. Legacy
installer stages are not the standalone roadmap. Missing optional features and
unvalidated hardware must be distinguished from missing implementations.
Keep installation, storage and beta-limitations docs aligned with actual behavior.
For documentation-only changes, check links and claims against the source; do
not rebuild firmware or run implementation tests just to edit prose.
