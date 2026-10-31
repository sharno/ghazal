# PortMaster ports

Ghazal runs [PortMaster](https://portmaster.games) ports: community builds of
native games (Cave Story, Stardew Valley, OpenTTD, Quake, ...) packaged as a shell
script plus a folder. PortMaster itself is not part of the image; the handheld
downloads it on request. Ghazal supplies what ports expect of a handheld Linux
system, which a Nix-built runtime does not have by default.

Status: the whole flow is implemented and its parts are proven off-device
(see [What is verified](#what-is-verified)). It has not run on a handheld yet.

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
| SDL2, SDL2_image, SDL2_mixer, SDL2_ttf, EGL, GLESv2, gbm, drm, ALSA, udev, zlib, libpng, libstdc++, libgcc_s | `/usr/lib/ghazal/ports/lib`, in the port's `LD_LIBRARY_PATH` (a port's own `libs.aarch64` is searched first) |
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

Off-device, by `scripts/test-ports.sh` in an aarch64 Linux container with the
runtime's own glibc, bash, Python and libraries, the static `ghazal` binary, and a
real PortMaster 2025.11 folder (its tools and Python code):

- a port script written like PortMaster's template, with a glibc binary that names
  `/lib/ld-linux-aarch64.so.1` and has no RPATH, runs under `ghazal ports run`: it
  finds `/roms/ports/PortMaster`, sources Ghazal's `control.txt` and PortMaster's
  stock `device_info.txt` and `funcs.txt`, gets `CFW_NAME=Ghazal`, `$GPTOKEYB`, the
  ports `PATH`, and loads SDL2 from the compat library directory;
- the mount namespace works: the ports folder is bound at `/roms/ports` for the
  port only, and the virtual consoles read as `/dev/null` inside it;
- an immediate failure keeps its exit status and shows `[ERROR]` for the menu; TERM
  (Menu + Start) ends the launcher at once, and helpers the script left running are killed;
- a port mounts a squashfs runtime with plain `mount`;
- gptokeyb, gptokeyb2, xdelta3, innoextract, sdl2imgshow, sdl_resolution and 7zz
  resolve their libraries, and xdelta3, innoextract and sdl_resolution (SDL's dummy
  video driver) run;
- under the runtime's Python, `harbourmaster`, `pugscene` and `pySDL2gui` import, and
  pysdl2 drives SDL2, SDL2_image (a PNG decodes), SDL2_mixer (opens the dummy audio
  device) and SDL2_ttf through `PYSDL2_DLL_PATH`;
- `ghazal ports check` passes the checks that do not depend on the handheld;
- unit tests: the pad's SDL GUID equals the one SDL reports for it, and its button
  and axis numbers follow from the key bitmap; install, control-file copying, the
  ports folder lookup and failure reporting.

Not run anywhere yet: PortMaster's GUI on a display, gptokeyb with real input,
KMSDRM and GLES in a port, audio, the menu's install flow over the network, and
anything on the handheld itself.

## What should work, and what will not

Should work, pending the device checks in AGENTS.md: aarch64 ports with bundled
libraries or the system libraries above; SDL2 and GLES (Panfrost) rendering and
ALSA audio; gptokeyb keyboard and mouse emulation; PortMaster's GUI, downloads,
`autoinstall`; runtimes mounted from squashfs (mono, Love2D, Godot/frt ... if the
kernel's squashfs decompressors cover their compression: zstd and zlib are built in,
xz is not).

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
