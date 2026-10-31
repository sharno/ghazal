# PortMaster ports

Ghazal runs [PortMaster](https://portmaster.games) ports: community builds of
native games (Cave Story, Stardew Valley, OpenTTD, Quake, ...) packaged as a shell
script plus a folder. PortMaster itself is not part of the image; the handheld
downloads it on request. Ghazal supplies what ports expect of a handheld Linux
system, which a Nix-built runtime does not have by default.

Status: the whole flow is implemented and was checked on an RG40XX-V v2 with
PortMaster 2026.10 and small test ports (see [What is verified](#what-is-verified)).
No real port from PortMaster's catalogue has run yet, and the PortMaster GUI's
downloads and the controller feel have not been tried by a person.

## Install PortMaster

With Wi-Fi connected, open **Applications → Install PortMaster**. The handheld
downloads `PortMaster.zip` from PortMaster's GitHub release, unpacks it as
`ROMS/PORTS/PortMaster` on the data card and adds Ghazal's control files. The entry
then reads **PortMaster**; it opens PortMaster's own interface, where you pick ports
and runtimes to download. When PortMaster exits, the Ports system is rescanned.

Over SSH: `ghazal ports install`.

Without Wi-Fi: unzip `PortMaster.zip` on a computer into `ROMS/PORTS` (so that
`ROMS/PORTS/PortMaster/PortMaster.sh` exists), then run **Applications →
PortMaster** (or `ghazal ports prepare`) once. Ports and runtimes can be copied the
same way: a port is the `.sh` script plus its folder, both directly in `ROMS/PORTS`;
runtimes (`*.squashfs`) go in `ROMS/PORTS/PortMaster/libs`; zips dropped in
`ROMS/PORTS/autoinstall` are installed the next time PortMaster starts.

An existing PortMaster folder from muOS (`MUOS/PortMaster`) is not reused: copy it to
`ROMS/PORTS/PortMaster` if you want its downloaded runtimes.

## Run ports

Installed ports appear under **Explore Content → Ports** (the `.sh` files directly in
`ROMS/PORTS`; the scan does not descend, since a port's data sits beside its script).
Menu + Start quits a port like any game. Favorites and History work. Ports are not
resumed with a state: they save the way each game does.

## What a port gets

`ghazal ports run SCRIPT` starts the script with bash in a private mount namespace:

| A port expects | Ghazal provides |
| --- | --- |
| `/roms/ports` (and `PortMaster` inside it) | `ROMS/PORTS` bind-mounted there, for this port only |
| `/lib/ld-linux-aarch64.so.1` for its glibc binaries | link to the runtime's glibc loader (`nix/runtime/default.nix`) |
| `#!/bin/bash`, `#!/usr/bin/env python3` | `/usr/bin/bash`, `/usr/bin/env` |
| python3 with ctypes and ssl, unzip, curl, mount (loop), unsquashfs, pkill, ... | `/usr/lib/ghazal/ports/bin`, first on the port's `PATH` |
| SDL2, SDL2_image, SDL2_mixer, SDL2_ttf, EGL, GLESv2, gbm, drm, ALSA, udev, zlib, libpng, libstdc++, libgcc_s; freetype, OpenAL, mpg123, Ogg, Vorbis and Theora for Love2D's runtime | `/usr/lib/ghazal/ports/lib`, in the port's `LD_LIBRARY_PATH` (a port's own `libs.aarch64` is searched first); the four SDL libraries are also linked in `/usr/lib`, where PortMaster's launcher points its GUI |
| `/dev/fd`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr` (bash's `<(...)`, which PortMaster's launcher uses) | linked at boot to `/proc/self/fd` (`config/*/system.toml`) |
| `control.txt`, `mod_<CFW>.txt`, `get_controls`, `$GPTOKEYB` | Ghazal's own, copied into the PortMaster folder before every run (`config/rg40xxv-rocknix/ports`); `CFW_NAME` is `Ghazal` |
| a gamepad SDL knows | an SDL mapping generated from the pad's evdev capabilities and `device.toml`, in `/run/ghazal/ports/gamecontrollerdb.txt` |
| `/dev/uinput` for gptokeyb's keyboard | present; a udev rule classifies virtual input devices so SDL reads them |
| a writable home | `HOME=/mnt/games/.ghazal/ports/home` (the root is read-only) |
| `/dev/tty0`, `/dev/tty1` for status text | `/dev/null` over them, so writes cannot make fbcon take over the display |

Face buttons follow position, as in ROCKNIX's mapping that ports are made against:
the pad's right button (labelled A) is SDL's `b` and its bottom button (B) is SDL's
`a`. `ANALOG_STICKS` follows the board (`src/ports/env.rs`).

Ports run at the full-speed CPU policy. Their output goes to `/var/log/ghazal/ports.log`
(and to each port's own `log.txt`). A port that fails within five seconds shows
its exit status in the menu; later exit codes are not reported, since games return
all sorts of codes when quit.

## Diagnostics

```sh
/opt/ghazal/ghazal ports check            # everything above, one line each
/opt/ghazal/ghazal ports ldd PORT/binary  # libraries a port binary cannot find
/opt/ghazal/ghazal ports run /mnt/games/ROMS/PORTS/Port.sh
tail -n 100 /var/log/ghazal/ports.log
```

A port that fails on a missing library: list it with `ports ldd`, then either add the
library to `libraries` in `nix/runtime/ports.nix` (and `SONAMES` in
`src/ports/check.rs`) or ask for a port build that bundles it.

## What is verified

On an RG40XX-V v2 (slot B, Wi-Fi up), with PortMaster 2026.10.29 downloaded by the
menu's own **Install PortMaster**:

- `ghazal ports check` passes every line: loader, bash, env, tools, libraries,
  Python imports, pysdl2, uinput, squashfs, loop, controller, display, PortMaster
  and the namespace. `ghazal ports prepare` writes the SDL mapping for the pad.
- Install over SSH and from the menu: download, unpack in RAM, copy onto the card,
  control files added; the Applications entry changes from **Install PortMaster** to
  **PortMaster**. It takes about five seconds on Wi-Fi.
- **Applications → PortMaster** starts PortMaster's GUI (`pugwash`, Python and
  pysdl2 on SDL2 KMSDRM); `observe frame` shows its disclaimer and main menu, the
  D-pad moves the cursor and the pad's bottom button is its accept. Menu + Start
  ends it in under 3 s with no `python3`, `pugwash` or `gptokeyb` left, no bind
  mount, and a working menu.
- A port script written like PortMaster's template, run from **Explore Content →
  Ports**, sees `/roms/ports`, sources Ghazal's `control.txt` and PortMaster's stock
  files, gets `CFW_NAME=Ghazal`, and runs PortMaster's own `portmsg` through SDL2,
  EGL and GLES on the panel. `gptokeyb` starts and creates a virtual keyboard that
  udev marks `ID_INPUT_KEYBOARD=1`; the pad's bottom button closes the dialog and
  the port's exit leaves nothing running and the input device gone.
- A port playing a tone through SDL2: PCM `RUNNING`, owner the port.
- A squashfs runtime mounts with plain `mount` for gzip, zstd and **xz**
  compression, reads back intact and leaves no loop device after `umount`.
- A script that exits with status 3 shows `[ERROR] Broken.sh stopped after 0 s with
  status 3; see /var/log/ghazal/ports.log` in the menu.
- Boot time is unchanged (2.9 s to the first frame) with PortMaster installed, and
  RetroArch (with the pad configured and sound), standalone PPSSPP and Menu + Start
  still behave as before.

Found and fixed by those runs: `/dev/fd` was missing; PortMaster's zip stores
`gptokeyb` without an execute bit and starts its Love2D splash before its own
`chmod -R +x`; Love2D's runtime needs freetype, OpenAL, mpg123, Vorbis and Theora
from the system.

Off-device, `scripts/test-ports.sh` repeats the port, namespace, squashfs and
PortMaster-tool checks in an aarch64 Linux container with the runtime's own glibc,
bash, Python and libraries and a PortMaster 2025.11 folder; unit tests cover the
pad's SDL GUID (it equals the one SDL reports), button and axis numbering, install,
control-file copying, executable bits, the ports folder lookup and failure
reporting.

Not tried: a real port from the catalogue (SDL2/GLES, gptokeyb mouse, mono, Godot),
PortMaster's own downloads and its Options, Love2D's splash and runtime with the
new libraries (see AGENTS.md), game audio beyond a tone, and anything that needs a
person: controller feel and the `ANALOG_STICKS` value for this board.

## What should work, and what will not

Should work, pending the device checks in AGENTS.md: aarch64 ports with bundled
libraries or the system libraries above; SDL2 and GLES (Panfrost) rendering and
ALSA audio; gptokeyb keyboard and mouse emulation; PortMaster's GUI, downloads,
`autoinstall`; runtimes mounted from squashfs (mono, Love2D, Godot/frt ...: the
kernel mounts gzip, zstd and xz).

Will not work:

- 32-bit ports (armhf): the kernel is built without `COMPAT`, and there is no 32-bit
  userland.
- Ports that need X11, Wayland, Weston or sway, Java, box86/box64 or a Vulkan driver.
- Ports that need desktop OpenGL without a gl4es of their own (the runtime's Mesa has
  GLES only).
- Rotated panels (RG28XX): ports draw landscape.
- Bluetooth controllers and HDMI output (not in Ghazal yet).
- Moonlight, ScummVM and other PortMaster "ports" that are really emulators with
  their own system requirements are untested.
