# Standalone RG40XX-V system

Ghazal owns PID 1, the service supervisor, frontend and power policy. The root
filesystem is assembled explicitly; neither muOS nor ROCKNIX's init, systemd manager,
EmulationStation or shell service framework runs.

The repository builds the bootloader and kernel from pinned sources:
`nix/uboot.nix` (mainline U-Boot with ROCKNIX's H700 DRAM patches and
defconfig) and `nix/h700-kernel.nix` (Linux with ROCKNIX's patch set and H700
config). A pinned ROCKNIX image supplies firmware, emulator binaries and their
library dependencies. `hardware.lock` records its digest and source revision.
The initial profile targets the LPDDR4 ("DDR4") RG40XX-V; DDR3 boards need a
U-Boot built with the DDR3 DRAM settings.

The boot partition contains the kernel, the board device tree and
`extlinux/extlinux.conf`. Every driver the handheld uses is built into the
kernel, so there is no initramfs and no module loading: the kernel mounts the
ext4 root filesystem and runs `/sbin/init`, which is ghazal. User games, BIOS,
saves and states belong on SD2; system logs, SSH host keys and settings persist
on SD1.

## Boot time

Power-on to the menu's first frame takes about 4.2 s on the RG40XX-V (it was
10.7 s with the ROCKNIX kernel, initramfs and udev-ordered services). It is
the sum of:

- SPL and U-Boot: about 2.5 s, including reading the 22 MB kernel from the
  card at ~21 MB/s. A smaller kernel shortens this.
- Kernel: about 0.9 s, dominated by panel and SD/Wi-Fi probing. `quiet` keeps
  the boot log off the 115200-baud serial console; printing it took ~2.5 s.
- ghazal: mounts, then `audio -> daemon -> frontend`. None of these wait for
  udev, which only serves RetroArch. The frontend draws before scanning the
  library. Syncthing waits for the first frame.

The frontend records the kernel uptime of its first frame in
`/run/ghazal/first-frame-ready` and the kernel log. Pre-kernel time is the
generic timer counter (which starts at reset) minus `CLOCK_BOOTTIME`.

SSH and kernel logging start independently of the frontend. A frontend crash
must leave remote access alive. BusyBox supplies the shell and diagnostic
commands; it does not manage boot. Network credentials and authorized SSH keys
are local build inputs under ignored `secrets/` and must never be committed.

Hardware acceptance requires a real boot, display and controller input, sound,
RetroArch launch/exit, persistent saves, clean shutdown and repeated deep
suspend/resume from both menu and emulator. QEMU does not establish H700 sleep
support. Record `/sys/power/mem_sleep`, wake sources and post-resume kernel logs,
then measure idle sleep battery drain on the actual board.

## Build

Create a private `secrets/` directory containing `ssid`, `passphrase` and
`authorized_keys` (one SSH public key per line). SSID and passphrase files each
hold one plain UTF-8 line; WPA-Personal networks are supported. Keep the directory
mode 0700 and files mode 0600.

```sh
nix run .#image -- "$PWD" "$PWD/secrets" "$PWD/build/ghazal-rg40xxv.img"
```

The builder refuses to overwrite an existing output. It verifies the upstream
SHA-256 and embedded source revision, resolves the recursive AArch64 ELF library
closure, and writes a 1.14 GiB MBR image: 128 MiB boot partition and 1 GiB ext4
root. Unused card space is currently left untouched. Only regular output files
are opened; flashing is a separate operation. The provisioned image is private
because it contains Wi-Fi credentials. Host SSH keys are generated on first boot,
so they are never shared between separately provisioned devices.

The recipe and dependency inputs are pinned. Images are not yet byte-for-byte
deterministic: filesystem UUIDs, machine IDs and creation timestamps vary.

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
```

`observe audio` reports PCM state and user volume, reads every fixed routing
control and fails if it differs from the profile. `frame` reads the currently
scanned-out DRM buffer, including emulator and RetroArch menus, without taking
display ownership. It supports linear XRGB8888 and refuses unsupported layouts
or overwriting an observation. Copy the PPM over SSH and convert it on the host
with `ffmpeg -i menu.ppm menu.png`. `press` validates named buttons against the
actual evdev capabilities, presses the chord for 200 ms and releases it even on
an error. These observations help test the real drivers; they do not establish
physical speaker output or sound quality by themselves.

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

Native suspend is disabled in this profile: both deep and s2idle trials have
failed to restore a usable device. Automatic sleep is off, the menu hides Sleep,
and a brief Power press leaves the device awake. Instead, 30 minutes without a
button press power the device off cleanly, even with a game running (SSH
sessions postpone it). Hold Power for a clean shutdown.
The daemon rejects `ctl suspend` while disabled. Dimming still works and input
activity restores brightness; an emulator or SSH session inhibits idle actions.
Physical wake, display/audio/input recovery and battery drain require acceptance
before enabling suspend. Initial scope is RetroArch
games; existing PortMaster applications, standalone emulators and muOS themes
are not launch targets. Current frontend/daemon controls do not yet expose
Bluetooth pairing, HDMI switching or RGB effects.

## Recovery and logs

Three boots that fail to draw the frontend enter a compiled recovery profile
with Wi-Fi and SSH. Configuration/startup failures also enter recovery directly;
the recovery profile does not depend on the editable SD-card configuration.
BusyBox provides an emergency console if recovery itself fails. After repairing
the configuration over SSH, write `0` to `/var/lib/ghazal/boot-attempts` and reboot.

The supervisor owns service output pipes and rotates logs at 4 MiB, keeping one
previous file per service. Kernel-log restarts skip the already captured boot
backlog. Shutdown stops services and the emulator, normally unmounts data
filesystems, and remounts root read-only. Failed unmounts are reported and their
filesystems are remounted read-only when possible.

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

Menu + Start exits a running emulator. The daemon identifies it through a
PID file descriptor and verified process start time, sends TERM, then KILL
if it does not exit within 10 seconds. RetroArch saves an automatic state
while quitting, and the next launch of that game resumes from it; this also
happens on idle poweroff and when holding Power. The frontend regains its display and
input ownership when the process exits. BIOS-dependent systems still require
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

PSP games run in standalone PPSSPP from the pinned bundle: a system in
`frontend.toml` sets either a RetroArch `core` or a standalone `command`
(`{rom}` is the game path). ROCKNIX's PPSSPP has `/storage` paths compiled
in; the image links them to `/var/lib/ghazal/ppsspp` (settings, seeded with
ROCKNIX's H700 defaults) and to the SD card. Game saves live in
`SAVES/file/PPSSPP/PSP/SAVEDATA`, shared with the libretro core; PPSSPP's own
states go to `SAVES/state/PPSSPP-standalone`. Standalone PPSSPP has no
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

The image includes Syncthing from the pinned hardware bundle. Its supervised
service waits for `/mnt/games/ROMS` on the prepared data card. Device identity,
configuration and database live at `/mnt/games/.ghazal/syncthing`, so an OS-card
reflash preserves pairing. Without SD2 it stays pending.

The management UI listens only on localhost. From your computer:

```sh
ssh -N -L 8385:127.0.0.1:8384 root@DEVICE_IP
```

Open `http://localhost:8385`, pair your computer and add `/mnt/games/SAVES` as a
sync folder. ROMs and BIOS can be added separately. Shut down games before
syncing saves; simultaneous gameplay on two devices can produce conflicts.
Do not sync `.ghazal/syncthing` itself. Automatic binary upgrades are disabled;
Syncthing is updated with the OS image. Logs: `/var/log/ghazal/syncthing.log`.
