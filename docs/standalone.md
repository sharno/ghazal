# Standalone RG40XX-V system

Ghazal owns PID 1, the service supervisor, frontend and power policy. The root
filesystem is assembled explicitly; neither muOS nor ROCKNIX's init, systemd manager,
EmulationStation or shell service framework runs.

The repository builds everything the handheld runs from pinned sources:
`nix/uboot.nix` (mainline U-Boot with ROCKNIX's H700 DRAM patches and
defconfig), `nix/h700-kernel.nix` (Linux with ROCKNIX's patch set and H700
config) and `nix/runtime` (the userspace; see [Runtime](#runtime)). ROCKNIX
contributes only its source tree at one commit (the flake's `rocknix` input);
`/etc/ghazal/image.json` records it and the nixpkgs revision.
Release images cover the device catalog with separate DDR3 and DDR4 bootloaders.
Choose the image matching the board, panel and RAM revision; see
[installation](installation.md) and the [device catalog](../config/h700/devices.toml).

Each A/B root filesystem contains its own kernel and board device tree under
`/boot`. U-Boot selects a slot using redundant environment state; the FAT
partition holds provisioning files. See [system updates](updates.md). Every driver the handheld uses is built into the
kernel, so there is no initramfs and no module loading: the kernel mounts the
read-only ext4 root filesystem and runs `/sbin/init`, which is ghazal. Games, BIOS, saves
and data-card settings live on TF2 when present, otherwise on TF1's data
partition. System logs, SSH host keys and OS settings persist on TF1's separate
state partition mounted at `/var`. See [the storage layout](storage-layout.md).

## Boot time

Power-on to the menu's first frame takes about 2.9 s on the RG40XX-V v2
(4.4 s before the October 2026 boot work; 10.7 s with the ROCKNIX kernel,
initramfs and udev-ordered services). It is the sum of:

- SPL and U-Boot: about 2.1 s, mostly reading the 15.6 MB kernel from the card
  at ~21 MB/s. Every MB removed from the kernel saves ~50 ms; options the
  handheld never uses are disabled in `nix/h700-kernel.nix`. An LZ4 kernel
  boots but is 0.14 s slower: U-Boot decompresses at only ~32 MB/s. Built-in
  Bluetooth (core, LE, HID, the RTL UART transport) added 0.53 MB, ~25 ms: the
  one cost every user pays for a feature some never enable.
- Kernel: about 0.35 s to mounting root. Root waits for every driver probe, so
  slow probes are kept out of it: fbcon takes over the display only when the
  console is first written (the menu powers the panel on instead), the Wi-Fi
  SDIO power sequence runs from the MMC rescan work, and the H616 UARTs skip
  the 8250 core's 20 ms FIFO count. `quiet` keeps the boot log off the
  115200-baud serial console; printing it took ~2.5 s. A panic draws the log
  tail over the current frame (`DRM_PANIC`), except over a game's GPU buffers.
  When a slot is retried after an unconfirmed boot, U-Boot passes
  `fbcon=nodefer`, so a panic before the menu is visible too; that boot
  is ~0.3 s slower.
- ghazal: about 0.45 s. Mounts, then the daemon, frontend and audio-init in
  parallel; PID 1 logs to the kernel log, since a console write would make
  fbcon take over and wait for the panel. Everything else starts with `start = "background"` once the first
  frame is drawn (or after `[supervisor] boot.deadline`, so SSH still comes up
  if the menu never does): udev, D-Bus, Wi-Fi, SSH, kernel logging, Syncthing
  and folder creation on the data card. Started earlier, they read ~10 MB of
  libraries while the menu's own code is still being paged in from the card,
  which cost ~0.45 s. The frontend draws before scanning the library.
  Bluetooth is not among them unless the user turned it on: its controller
  stays unpowered and `bluetoothd` unstarted ([Bluetooth](bluetooth.md)).

`ghazal observe boot` prints the first frame's kernel uptime (also in
`/run/ghazal/first-frame-ready` and the kernel log), the firmware time
(the generic timer counts from reset; `CLOCK_BOOTTIME` from kernel start) and
their sum. The supervisor logs every service start and exit to the kernel log,
so `/var/log/ghazal/kernel-log.log` holds each boot's timeline. For per-driver
kernel timings, boot a test kernel built with `CMDLINE_FORCE` and
`initcall_debug` (arm64 cannot append to the bootloader's arguments) on the
inactive slot; KALLSYMS is off, so decode addresses with the build's
`System.map`. Most remaining
userspace time is reading ~6.5 MB of the ghazal binary from the card;
read-ahead sizes from 32 KiB to 1 MiB did not shorten it.

Emulators wait (at most 10 s) for the paths in `frontend.toml`'s
`launch_after`: udev's `udev-settle` oneshot `marks` `/run/ghazal/udev-settled`,
so a Last Game startup cannot launch RetroArch before udev has catalogued the
gamepad.

SSH and kernel logging start independently of the frontend. A frontend crash
must leave remote access alive. BusyBox supplies the shell and diagnostic
commands; it does not manage boot. Network credentials and authorized SSH keys
are local build inputs under ignored `secrets/` and must never be committed.

Hardware acceptance requires a real boot, display and controller input, sound,
RetroArch launch/exit, persistent saves, clean shutdown and repeated deep
suspend/resume from both menu and emulator. QEMU does not establish H700 sleep
support. Record `/sys/power/mem_sleep`, wake sources and post-resume kernel logs,
then measure idle sleep battery drain on the actual board. The
[release checklist](release-checks.md) automates the launch, sound, saves and
quit parts with `scripts/acceptance.sh`.

## Build

For menu layout checks without a handheld, use the desktop snapshot renderer:

```sh
GHAZAL_SNAPSHOT_POWER=charging cargo sim --snapshot /tmp/ghazal-menu ""
GHAZAL_SNAPSHOT_POWER=discharging GHAZAL_SNAPSHOT_SIZE=480x640 cargo sim --snapshot /tmp/ghazal-portrait ""
```

Snapshots use a fixed 50% battery when the power override is supplied. The override
is desktop-only and accepts `charging` or `discharging`; it never changes hardware
readings, daemon policy or update safety checks. Omit it to observe host status.
`GHAZAL_SNAPSHOT_HDMI=disconnected|unused|active|mirror` shows the HDMI entry in
Configuration and Information, and `GHAZAL_SNAPSHOT_NOTICE=1` the screen the
handheld shows while a game runs on HDMI. `GHAZAL_SNAPSHOT_INDICATORS=1` lights
every status icon (Wi-Fi at three of four bars, Bluetooth with a device,
Syncthing syncing, and HDMI as `GHAZAL_SNAPSHOT_HDMI` sets it). The simulator's
Configuration → Syncthing uses a pretend Syncthing (`src/frontend/syncthing/fake.rs`);
`GHAZAL_SYNCTHING_REAL=1` points it at the host's `/mnt/games/.ghazal/syncthing`
instead.

The status bar shows an icon for each service working in the background, next
to the clock: Wi-Fi (bars by signal; dim while not connected), Bluetooth (dim
while on, bright with a device linked), Syncthing (dim while starting or with no
device connected, bright when up to date, accent while syncing, red on folder
errors) and HDMI (dim with a display plugged in, bright while it shows the
picture). An icon is hidden while its service is off or the board lacks it.
They are read from sysfs and /proc every 5 seconds (`src/frontend/indicators.rs`);
Syncthing's comes from its REST API on a background thread at most every 10
seconds. The icons are SVGs in `assets/icons`, rasterized by build.rs and
colored by Slint.

Create a private `secrets/` directory containing `ssid`, `passphrase` and
`authorized_keys` (one SSH public key per line). SSID and passphrase files each
hold one plain UTF-8 line; WPA-Personal networks are supported. Keep the directory
mode 0700 and files mode 0600.

```sh
nix run .#image -- "$PWD" "$PWD/secrets" "$PWD/build/ghazal-rg40xxv.img"
```

The builder refuses to overwrite an existing output. It copies the runtime's
Nix closure and links onto the root filesystem and writes an MBR image with a 128 MiB boot partition, a 1 GiB ext4
root per A/B slot, a 256 MiB persistent state partition and a reserved 128 MiB
data partition (logical system partitions use an MBR extended container). First boot initializes and grows
the data partition to use the remaining card space. Only regular output files
are opened; flashing is a separate operation. The provisioned image is private
because it contains Wi-Fi credentials. Host SSH keys are generated on first boot,
so they are never shared between separately provisioned devices.

The recipe and dependency inputs are pinned. Images are not yet byte-for-byte
deterministic: filesystem UUIDs and creation timestamps vary. Machine IDs and
SSH host keys are generated per device on first boot.

## Runtime

`nix/runtime` builds the device userspace from the flake's nixpkgs for
aarch64-linux. A process cannot load two glibcs, so Mesa, SDL2, RetroArch,
PPSSPP and every core come from that one package set; the system daemons
(udev, D-Bus, iwd, OpenSSH, Syncthing, ALSA tools, RAUC) come from it too.
Stock nixpkgs closures do not fit a 1 GiB slot (Mesa alone is ~1.1 GB with
LLVM), so the graphics stack is rebuilt for this device only:

- Mesa: panfrost, EGL, GBM and GLES on DRM; no LLVM, glvnd, X11, Wayland or
  Vulkan. Panfrost's precompiled kernels use the same Mesa version's tools.
- SDL2 2.32 (as ROCKNIX): KMSDRM, GLES, ALSA, udev.
- RetroArch: KMS/EGL with GLES 3.1, ALSA, udev and RGUI.
- PPSSPP and its libretro core: GLES with ROCKNIX's H700 flags. Standalone
  states are named after the ROM file, as ROCKNIX's build names them.
- Moonlight Embedded on the SDL2 build above, with FFmpeg cut down to the H.264
  decoder (`ffmpeg.nix`; 2.3 MB). Together with libopus and libevdev it adds
  about 3.9 MB to the root; see [game streaming](moonlight.md).
- Cores: `nix/runtime/cores.nix` maps each `core =` in `frontend.toml` to
  ROCKNIX's recipe for it. `rocknix-core.nix` reads the pinned revision, source
  hash, patches and make or CMake options from its `package.mk` and builds with
  ROCKNIX's H700 compiler flags, without nixpkgs' hardening; `cores.nix`
  restates what package.mk sets in shell conditionals and hooks. Evaluation
  fails if `frontend.toml` names a core the table lacks, in a system or in
  its `[[system.alternative]]`s.
- PPSSPP and RetroArch are also built without nixpkgs' hardening flags, which
  cost cores 3-20%.

The image builder copies the runtime's store closure to `/nix/store` on the
root filesystem and the `tree` output's links (`/usr/bin/retroarch`,
`/usr/lib/libretro/*.so`, daemons) beside it. Programs find libraries through
their store RPATHs; nothing is installed in `/usr/lib`. udev applies only
Ghazal's own rules: the stock systemd rules are masked.

These packages are not in cache.nixos.org, so building them needs an aarch64
Linux machine: CI uses `ubuntu-24.04-arm` and caches the closure; locally use
an aarch64 Linux container or a remote builder. The first full build takes
hours; later builds reuse the cache.

Check cores after bumps: a build variable can silently drop a dynarec
(PicoDrive's 32X needs `ARCH=aarch64`) or build a JIT that crashes (melonDS's
with `ARCH=arm64`). For software-rendered cores, RetroArch with
`video_driver = "null"`, `audio_driver = "null"` and `--max-frames=N` times
emulation alone and runs over SSH beside the menu: time a 30-frame and a
30+N-frame run and divide N by the difference. Unthrottled runs on the display
also measure presentation, which differs between RetroArch builds.

## Data and debugging

Audio initialization applies the full codec signal path from `[audio].route`
in `device.toml` before the daemon restores user volume. This includes DAC,
analog Line Out gain/switches, and the speaker amplifier. `audio-init` validates
all named controls and their ALSA types before writing, then checks readback.
PCM startup alone cannot detect a muted analog output.

The built-in observation commands exercise the real handheld:

```sh
/opt/ghazal/ghazal observe audio
/opt/ghazal/ghazal observe frame /tmp/menu.ppm
/opt/ghazal/ghazal observe press menu+x
/opt/ghazal/ghazal observe power 60
/opt/ghazal/ghazal observe plan "/mnt/games/ROMS/GB/Example.gb"
/opt/ghazal/ghazal observe launch "/mnt/games/ROMS/GB/Example.gb"
```

`observe audio` reports PCM state and user volume, reads every fixed routing
control and fails if it differs from the profile. `frame` reads the currently
scanned-out DRM buffer, including emulator and RetroArch menus, without taking
display ownership. It supports linear XRGB8888 and refuses unsupported layouts
or overwriting an observation. Copy the PPM over SSH and convert it on the host
with `ffmpeg -i menu.ppm menu.png`; it also prints `distinct_colors` (capped at
4096), which is 1 or 2 for a blank frame. `plan ROM` runs the menu's library scan
and prints, as `key=value` lines, the ROM's system, launch kind, expected clock
policy and the button path from anywhere in the menu to launching it; `launch ROM`
presses that path on the real gamepad ([release checks](release-checks.md) use
both). `press` validates named buttons against the
actual evdev capabilities, presses the chord for 200 ms and releases it even on
an error. These observations help test the real drivers; they do not establish
physical speaker output or sound quality by themselves. `power` reports the CPU's
time at each clock step over a run on battery, with battery voltage and
charge at both ends. The AXP717 only measures charge current (its
`current_now` barely moves with load), so compare draw as charge lost over
equal long runs.

SD2 partition 1 may be exFAT, FAT32 or ext4. It mounts at `/mnt/games` with:

```
ROMS/<system>/...
BIOS/...
SAVES/file/<RetroArch core name>/...
SAVES/state/<RetroArch core name>/...
```

For migration, copy muOS `MUOS/bios` to `BIOS`, and `MUOS/save/file` and
`MUOS/save/state` into the corresponding `SAVES` folders. Keep original backups:
save-state compatibility across different core builds is not guaranteed.

Connect with `ssh root@<device-ip>`; the device hostname is `ghazal`. Logs persist
under `/var/log/ghazal`, even when the interface crashes. Useful commands:

```sh
/opt/ghazal/ghazal check
/opt/ghazal/ghazal probe-input
/opt/ghazal/ghazal ctl status
cat /sys/power/mem_sleep
tail -n 100 /var/log/ghazal/kernel-log.log
/opt/ghazal/ghazal ctl suspend
/opt/ghazal/ghazal ctl poweroff
```

The CPU runs `schedutil` in the menu. A game's system sets its `cpu` load in
`frontend.toml`, and `[cpu]` in `device.toml` maps it to a clock floor:
`light` (720 MHz) and `moderate` (1008 MHz) use `schedutil` above the floor,
so fast-forward and heavier scenes still reach the top clock. Up to 1008 MHz
the tested RG40XX-V's CPU stays at its lowest voltage (0.9 V, against 1.1 V at
1416 MHz), which is where the saving comes from. Unrated systems run at
`performance`, muOS's policy for every game. Boot keeps the kernel's
`performance` default until the first frame. Realtime interrupt threads (Wi-Fi
SDIO, GPU jobs) wake hundreds of times a second in a game; the kernel's
utilization clamping with a zero realtime floor keeps them from raising the
clock to the top step. The red LED lights at low battery.

The RG40XX-V's stock firmware charges every H700 model at 1.5 A while
running; the device tree now does too (it was 1.024 A). The daemon logs each
change between charging and battery power, with the level, to `daemon.log`.

The codec drives the speaker amplifier and the headphone jack from one
output. The daemon reads the jack switch and turns `Speaker Switch` off while
headphones are plugged in (ROCKNIX leaves this to PipeWire's UCM), so
`audio-init` no longer sets it.

On the RG40XX H/V and RG CubeXX, Configuration → Stick lights sets the analog
stick RGB rings: off (the controller is unpowered), a solid colour, breathing
or rainbow. The daemon keeps the choice in `settings.toml` and turns enabled
rings red at low battery. The rings' microcontroller sits behind UART5
(`[lights].uart`) and is powered through the `rgb:kbd_backlight` GPIO; frames
follow muOS's serial protocol. `ghazal ctl lights next|prev|<preset>` changes
it over SSH.

Native suspend is disabled in this profile: both deep and s2idle trials have
failed to restore a usable device. Automatic sleep is off, the menu hides Sleep,
and a brief Power press leaves the device awake. Instead, 30 minutes without a
button press power the device off cleanly, even with a game running (SSH
sessions postpone it). Hold Power for a clean shutdown.
The daemon rejects `ctl suspend` while disabled. Dimming still works and input
activity restores brightness; an emulator or SSH session inhibits idle actions.
Physical wake, display/audio/input recovery and battery drain require acceptance
before enabling suspend. Launch targets are RetroArch games, standalone PPSSPP
and [PortMaster ports](ports.md); muOS applications and themes are not. Bluetooth
controllers pair from Configuration → Bluetooth once the kernel option is on
([Bluetooth](bluetooth.md)).

Configuration → HDMI sends the menu and games to a connected display, with the
handheld's screen off (Auto) or showing the menu too (Mirror); see [HDMI](hdmi.md).
`ghazal observe hdmi` prints the connector, its modes and the sound card.

## Recovery and logs

A/B boots that fail before the menu reports healthy are retried and then
rolled back to the other OS slot (see [system updates](updates.md)). A missing,
invalid or full data card instead enters a compiled recovery profile with
Wi-Fi and SSH, which does not depend on the editable configuration. Recovery
marks its OS slot good and stops the watchdog: the other slot shares the card
and could not repair it. BusyBox provides an emergency console if recovery
itself fails. Provisioning errors are logged and the boot continues offline.

The supervisor owns service output pipes and rotates logs at 4 MiB, keeping one
previous file per service. Kernel-log restarts skip the already captured boot
backlog. Shutdown stops services and the emulator, normally unmounts data
filesystems, and remounts root read-only. Failed unmounts are reported and their
filesystems are remounted read-only when possible. The shutdown trace in
`/var/log/ghazal/shutdown.log` closes itself before `/var` is unmounted; while
it stayed open, `/var` was never unmounted and every boot replayed its journal.

The kernel starts the hardware watchdog when its driver probes (~0.2 s into
boot) and pets it until `ghazal update health` opens it, for at most 60 s
(`CONFIG_WATCHDOG_OPEN_TIMEOUT`). A kernel or init hang therefore resets the
board and counts as a failed A/B attempt. Recovery mode and the emergency
console stop the watchdog, since nothing pets it there. `update health` stops
it on SIGTERM so a slow shutdown cannot turn a power-off into a reset.

## Options and thumbnails

Configuration on the home screen offers brightness, volume and box art.
Menu → Options also holds the thumbnail setting, kept in
`/var/lib/ghazal/frontend-settings.toml`.
"Thumbnails" shows the selected game's box art beside game lists. Art is
looked up next to the ROM (`Imgs/<game>.png`), then in muOS's catalogue
(`MUOS/info/catalogue/<system>/box`), then in the download cache
(`/mnt/games/.ghazal/thumbnails/<libretro system>/`). It is decoded and scaled
on a worker thread. Applications → Download thumbnails fetches missing box art from
libretro's thumbnail server over plain HTTP in the background, scaled to the
panel; titles the server lacks get a `.missing` marker and aren't retried.
Arcade art isn't downloaded: libretro names it by title, not by romset.

## Favorites, history and emulator recovery

The standalone image owns the H700 Gamepad RetroArch profile in
`config/rg40xxv-rocknix/gamepad.cfg`. Its indices derive from `device.toml`
codes, where the X button sends BTN_WEST and Y sends BTN_NORTH (checked by
reading the real pad: injected events reuse `device.toml` and cannot catch a
wrong code). On the RG40XX-V:

| In a game | Action |
| --- | --- |
| Menu + X | Open/close the RetroArch quick menu |
| Menu + R1 | Save state in the selected slot |
| Menu + L1 | Load state from the selected slot |
| Menu + A | Pause/resume |
| Menu + Y | Toggle the FPS display |
| Menu + Start | Exit to ghazal |

Save slots and other core options are available through the RetroArch quick menu.

The main menu has separate Collection and History entries, alongside Explore
Content, Applications, Information, Configuration, Reboot and Shutdown.
Explore Content opens the systems list. B returns from systems and collections
to the home screen; B from a system's games returns to the systems list.
Press Y on a game to toggle its favorite status. History keeps the latest 100 distinct
launches. Both persist atomically in `/mnt/games/.ghazal/activity.toml`; they
survive replacement of the OS card. Missing games are hidden until available
again. Library rescans run in a worker so navigation remains responsive.
Activity format 2 stores game paths, so changing a system label or core mapping
preserves favorites and history. Existing format 1 files migrate atomically.

### Choosing the emulator

A system in `frontend.toml` runs its games with its own `core` or `command`
(its default emulator) and may list `[[system.alternative]]` emulators (the
comment above the first `[[system]]` has the keys). Press X on a game, in a
system's list, Collection or History, to open its actions, and choose Emulator
(listed only for a system with alternatives) to open the emulator picker; its
title shows the emulator the game uses now.

| Picker row | A (this game) | X (the whole system) |
| --- | --- | --- |
| `Default (<name>)` | The game follows its system's default | The system's default is `frontend.toml`'s again |
| An emulator | Pins the game to it, whatever the default becomes | Makes it the default for games not pinned |

A pinned game shows its emulator's name on the right of its row. A game uses,
in order, its pin, its system's default and the emulator `frontend.toml` lists
first. Launches, Last Game startup and History all use that resolution, and
each emulator's own `cpu` rating sets the clock policy, so a lighter core can
run at a lower floor than the system's default.

Choices persist in `/mnt/games/.ghazal/emulators.toml` (`emulators_file`;
beside `activity.toml` unless set), written atomically, format version `"1"`:

```toml
version = "1"

[systems.SNES]
core = "snes9x2010_libretro.so"

[[games]]
path = "/mnt/games/ROMS/PSP/Game.iso"

[games.emulator]
standalone = "ppsspp"
```

An emulator is named by its core file or standalone `name` (the program's file
name unless `name` is set), never by position, so reordering or adding
alternatives keeps every choice. A choice naming an emulator the system no
longer lists is ignored and kept in the file. A game is identified by its path
and a system by its `name`; renaming a system forgets its default, not its
pins. A damaged file is moved aside as `emulators.toml.invalid-<time>` and the
menu starts with defaults. A file with an unknown `version` (written by a newer
system on the other slot) is left untouched: choices are off, games use their
systems' defaults and the menu says so. A new file version adds a variant to
`Stored` in `src/frontend/emulator_choice.rs` and a conversion; the previous
version's test fixture must keep reading.

RetroArch files saves and states under the core's name
(`SAVES/file/<core>`, `SAVES/state/<core>`), so a game's saves and states do not
follow it to another core; the choice toast says so. The picker never copies
them. Standalone and libretro PPSSPP share game saves, not save states.

Alternatives shipped for the RG40XX-V: GBA mGBA / VBA-Next, NES FCEUmm /
Nestopia, SNES Snes9x / Snes9x 2010, Mega Drive, Master System and Game Gear
Genesis Plus GX / PicoDrive, Mega CD PicoDrive / Genesis Plus GX, PSP standalone
PPSSPP / PPSSPP libretro. Rotated-panel images (RG28XX) keep only the libretro
PSP core. The alternatives' `cpu` floors reuse the default's and are not
measured. ROCKNIX's gpsp and snes9x2010 H700 builds are 32-bit ARM
(`-marm`, 32-bit dynarec); the aarch64 runtime builds snes9x2010 from its
generic target and omits gpsp. No PS1 alternative is built: the recipes exist
(`beetle-psx-lr`, `swanstation-lr`) but neither is known to hold full speed.

Configuration → RetroArch settings opens RetroArch's own menu without launching
a game or adding to History. Open its Settings tab to change video, audio, input
and saving options. Quit RetroArch (or press Menu + Start) to return to Ghazal.
Changes save on exit to `/mnt/games/.ghazal/retroarch/retroarch.cfg`, initialized
from the image's defaults on first use. This file is shared by games and the
settings screen and stays with the data card.

Manual save states (Menu + R1) automatically advance the slot and retain the
latest ten numbered states per game. Menu + L1 loads the selected slot; use the
in-game RetroArch menu (Menu + X) to choose an older slot. The `.state.auto`
resume state is separate from those ten slots. These are defaults, adjustable
under RetroArch Settings → Saving.

Menu + Start exits a running emulator. The daemon identifies it through a
PID file descriptor and verified process start time, sends TERM, then KILL
if it does not exit within 10 seconds. RetroArch saves an automatic state
while quitting, and the next launch of that game resumes from it; this also
happens on idle poweroff and when holding Power. The frontend regains its display and
input ownership when the process exits. Gameplay input is discarded, and held
controls must return to neutral before menu navigation resumes. Automatic resume
requires core save-state support; a failed automatic save is reported on return
to the menu. In-game saves are separate from save states. BIOS-dependent systems still require
the appropriate user-provided files in `/mnt/games/BIOS`. A system with
`requires_any` in `frontend.toml` stays hidden until one of those files is
in `bios_dir`; rescan after adding one. On the RG40XX-V that applies to PC
Engine CD (`syscard3.pce`), Mega CD (`bios_CD_U.bin`/`_E`/`_J`), Saturn
(`saturn_bios.bin`) and Naomi (`dc/naomi.zip`). MSX needs blueMSX's
`Databases/` and its free C-BIOS `Machines/` folders (`MSX - C-BIOS`,
`MSX2 - C-BIOS`, `MSX2+ - C-BIOS`) from libretro's blueMSX-libretro
repository; its `Shared Roms` folder holds copyrighted ROMs and is not
needed. With "Auto", the core falls back to C-BIOS. Dreamcast and Nintendo DS
can run without BIOS files.
Failed launches display the core's diagnostic when available, including missing
firmware names. `ROMS/MAME` uses MAME 2003 Plus; `ROMS/FBNEO` uses FBNeo. Keep ROM
sets matched to the selected core. Dreamcast and Naomi are separate systems;
Naomi needs its own BIOS even when Dreamcast games load without one.

PSP games run in standalone PPSSPP by default (the libretro core is an
alternative; see Choosing the emulator): a system in `frontend.toml` sets either a
RetroArch `core` or a standalone `command` (`{rom}` is the game path).
`nix/runtime/ppsspp.nix` compiles its directories in: settings and cheats in
`/var/lib/ghazal/ppsspp/PSP` (seeded with ROCKNIX's H700 defaults), the memory
stick at `SAVES/file/PPSSPP` so game saves in `PSP/SAVEDATA` are shared with the
libretro core, states in `SAVES/state/PPSSPP-standalone` and screenshots in
`SAVES/screenshots`. SDL maps the pad from the mapping in `frontend.toml`. Standalone PPSSPP has no
automatic state on quit. PPSSPP scans out ARGB8888 buffers whose alpha holds
rendering data; a kernel patch makes the bottom display plane ignore it. Its pad
mapping (`config/rg40xxv-rocknix/ppsspp/controls.ini`) matches RetroArch's
hotkeys: Menu + X pause menu, Menu + R1/L1 save/load state, Menu + A pause.
`ppsspp-overrides.ini` is merged into ROCKNIX's defaults when the image is
built: a smaller interface for 640x480, and no CPU-side depth rasterization. PSP
buttons follow position (Cross bottom, Circle right, Square left, Triangle
top). Per-game settings live in `ppsspp/games/<ID>.ini` and become full
`<ID>_ppsspp.ini` files, since PPSSPP reads every per-game setting from them;
`ppsspp/Cheats/` holds enabled cheats. God of War (Chains of Olympus, and
Ghost of Sparta EU) skips buffer effects, a large speedup, with the CWCheat
database's Dark Overlay HackFix keeping the picture from going black.

In the ghazal menu, Menu and Start act when released, and only when no other
key was pressed while held, so hotkey chords such as Menu + Vol never open it.

## RetroAchievements and netplay

`nix/runtime/retroarch.nix` builds RetroArch with networking, TLS (the
built-in mbedTLS, which carries its own CA bundle, so no system certificates
are read), RetroAchievements and LAN netplay discovery, and fails the build if
the binary lacks any of them. Both features need Wi-Fi. A launch that signs in
to RetroAchievements or joins netplay waits up to `[retroarch] online_wait`
(8 s) for a route, but only when a Wi-Fi network is saved; an offline handheld
never waits.

### RetroAchievements

Configuration → Achievements cycles off, on and hardcore. It needs an account
(Configuration → Achievements account, or `provision.toml`). The row shows the
mode and account: `On, name`, or `signs in at next game` while a password has not
yet been exchanged for a login token. Softcore is the default: hardcore mode
turns off save states, rewind and cheats, so Ghazal's auto-resume and
Menu + R1/L1 do nothing while it is on. Ghazal writes both `cheevos_enable`
and `cheevos_hardcore_mode_enable`, because RetroArch's own default for
hardcore is on.

Configuration → Achievements account starts RetroArch's menu and presses the
keys of `[retroarch.menu_paths] achievements` in `frontend.toml` through
RetroArch's network command port (`network_cmd_enable`), which lands on
Settings → Achievements. RetroArch has no command line option for a menu page,
so the path is data: it counts entries from the end of each list (Up wraps) and
comes from RetroArch 1.22's menu source. Its keys are `up`, `down`, `left`,
`right`, `ok` and `back`: RetroArch's commands are RetroPad buttons, and its
`menu_swap_ok_cancel_buttons` (true by default in 1.22, read from the user
config) makes `MENU_B` confirm and `MENU_A` go back. The helper talks to
RetroArch over `127.0.0.1`, which the `loopback` service raises at boot. If a page differs, the player is still
in RetroArch's menu, and the path is the thing to change. Enter the username
and password there and quit RetroArch. RetroArch signs in when a game loads,
keeps a login token and clears the password; both are written to
`/mnt/games/.ghazal/retroarch/retroarch.cfg` when RetroArch exits, so the token
lives on the data card.

`provision.toml` can carry the account (`[retroarch] cheevos_username` and
`cheevos_password`). The data card is not mounted at that point of boot, so
provisioning writes `/var/lib/ghazal/retroarch-account.cfg` (mode 0600) on the
state partition. The next RetroArch launch merges it into the user config on the
data card, which turns achievements on in softcore mode, and deletes the seed.
The password stays in that config (mode 0600 on ext4; exFAT keeps no modes)
until the first sign-in. Values with a double quote or line break are rejected,
since RetroArch's config format cannot hold them. Nothing logs the password;
check sign-in in `/var/log/ghazal/retroarch.log`. Badges cache under
`thumbnails_directory`, on the data card. Without a connection when a game
loads, RetroArch cannot sign in and achievements stay off for that session.

### Netplay

In a game list, X opens the game's actions: Play, Emulator (systems with
alternatives), and, when the game's emulator is a RetroArch core, Host netplay
and Join netplay. PSP runs in standalone PPSSPP and has no netplay, and neither
does a game pinned to a standalone emulator.

- **Host netplay** starts the game with `--host --port 55435 --nick NAME`. The
  nickname is the hostname; a handheld still called `ghazal` appends the last four
  characters of its machine ID (the first sixteen are zeros) so two of them differ.
- **Join netplay** broadcasts RetroArch's LAN discovery query (UDP port 55435,
  `RANQ`) and lists the hosts that answer with the game they are playing (Y
  scans again, for a host that started late). From a
  game it shows hosts of that game, plus hosts listed in
  `/mnt/games/.ghazal/netplay-hosts.txt` (one `address[:port] [name]` per line,
  for another subnet or a VPN, where a broadcast does not reach). Applications →
  Join netplay lists every host and starts the matching game from the library:
  same file name, preferring a system that has an emulator running the host's
  core. Joining uses the emulator that runs the host's core when the system has
  one (a host's core beats the game's pinned emulator), else the game's own;
  a saved host does not report its core. Joining runs
  `--connect ADDRESS --port PORT --sram-mode noload-nosave`, so the host's save
  never overwrites the joiner's.
- Netplay launches pass `--appendconfig /run/ghazal/retroarch-netplay.cfg`,
  written from `[netplay.retroarch]`: no public lobby listing, relay or UPnP
  port mapping (RetroArch defaults to announcing publicly and to NAT traversal),
  no automatic state load or save, and `config_save_on_exit = "false"`, because
  RetroArch writes appended settings into the user config when it exits.
  Changes made in RetroArch's menu during a session are therefore not kept.

Both handhelds need the same RetroArch version, the same core and the same ROM
file, and the core must support save states. A host with a netplay password must
be joined from RetroArch's own menu (Menu + X, Netplay), which also offers
spectating and the public relay.

Not yet checked on a handheld: Achievements account's key path, sign-in over
Wi-Fi, the first-boot account merge, hosting and joining between two handhelds,
and the wait for Wi-Fi after a Last Game startup.

## Migrating a muOS data card

To migrate a muOS data card, prepare an ext4 destination, copy your backup
to it with `rsync -a`, then run the native migration tool:

```sh
cargo build --release --manifest-path tools/data-migration/Cargo.toml
sudo tools/data-migration/target/release/ghazal-data-migration /mnt/ghazal-data
```

The tool validates a mounted ext4 destination, required directories, existing
sync markers and identity, and both configured sync-folder paths before making
changes. It moves `MUOS/bios` to `BIOS`, `MUOS/save` to `SAVES`, and the existing
Syncthing state to `.ghazal/syncthing`. Saves and catalogue sync keep their folder
IDs and pairing; only local paths change. The original config is retained as
`config.xml.before-ghazal`. Other MUOS files remain available for later cleanup.
The original backup is never modified. This is a one-time conversion and
refuses an already converted destination.

## Syncthing

The image includes Syncthing from nixpkgs, managed from Configuration →
Syncthing: on and off, this handheld's ID as a QR code, pairing and folder
requests, and its web interface on the network with a generated login. See
[Syncthing](syncthing.md).

The supervisor runs it as `ghazal syncthing serve`, which adds the web interface
address the player chose (on the handheld only by default), after the
`syncthing-restore` oneshot has released it at boot; turning it off removes that
marker (`/run/ghazal/syncthing-requested`), and a service waits for its
`wait_for` paths again whenever it exits. It also waits for `/mnt/games/ROMS` on
the prepared data card. Device identity, configuration, database and the
player's choices (`ghazal.toml`) live at `/mnt/games/.ghazal/syncthing`, so an
OS-card reflash preserves pairing. Without SD2 it stays pending.

An SSH tunnel to the local web interface still works:
`ssh -N -L 8385:127.0.0.1:8384 root@DEVICE_IP`, then `http://localhost:8385`.
Shut down games before syncing saves; simultaneous gameplay on two devices can
produce conflicts. Do not sync `.ghazal/syncthing` itself. Automatic binary
upgrades are disabled; Syncthing is updated with the OS image. Logs:
`/var/log/ghazal/syncthing.log`.
