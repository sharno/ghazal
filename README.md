# oxmux

A Rust frontend for the Anbernic RG40XXV (Allwinner H700), built as a muOS alternative.
Right now it runs *on top of* muOS: it uses the muOS kernel, rootfs, and RetroArch
build, and replaces the menu. It lists your systems and games and launches them in
RetroArch with the right libretro core.

## Design

- **UI in Slint** (`ui/app.slint`): the screens are declarative markup. Rust owns all
  state (`src/app.rs`) and pushes it into the window's properties. Slint handles
  layout, text, and animation.
- **Software rendering with no GPU and no windowing system.** oxmux provides its own
  Slint platform (`src/platform/mod.rs`) around the software renderer. Slint repaints
  only the regions that changed, and only that rectangle is copied to `/dev/fb0`.
  When nothing changes, the process sleeps in `poll()`.
- **Virtualized lists.** The UI only ever gets a window of about 48 rows around the
  cursor, so a system with thousands of games costs the same as a screenful.
- **Static musl binary** (`aarch64-unknown-linux-musl`, linked with `rust-lld`), so it
  runs on any H700 rootfs whatever its glibc version. No C cross toolchain needed.
  Fonts (DejaVu) are embedded; fontconfig is only dlopen'ed and isn't required.
- **Backends** (`src/platform/`):
  - `fbdev.rs`: `/dev/fb0` via mmap, damage-rect blits, 16 or 32 bpp
  - `evdev_input.rs`: gamepad via evdev, with an exclusive grab, d-pad, hat, analog stick, and a probe tool
  - `desktop.rs`: winit + softbuffer simulator window, plus headless PNG snapshots
- **Launching**: the frontend releases its input grab, runs RetroArch as a child
  process, then re-reads the framebuffer mode and grabs input again.

## Develop on the desktop

```bash
nix develop -c cargo sim
```

Keys: arrows = d-pad, Z/Enter = A, X/Backspace = B, Q/W = L1/R1, Esc/Tab = Menu.
Fake ROMs live in `dev/ROMS`.

To render frames without a window (handy for checking UI changes):

```bash
cargo run --features desktop -- --config dev/oxmux.toml --snapshot /tmp/snap "a,down,menu"
```

## Build and install on the device

```bash
scripts/package.sh                          # -> dist/Oxmux (binary, oxmux.toml, mux_launch.sh)
DEVICE=root@<device-ip> scripts/deploy.sh   # -> /mnt/mmc/MUOS/application/Oxmux
```

Then start **Oxmux** from the muOS Applications menu. Logs go to `oxmux.log` and
`retroarch.log` next to the binary.

### First run on real hardware

Paths in `config/muos.toml` come from the muOS source, but the button codes still need checking on real hardware. Over SSH:

```bash
cd /mnt/mmc/MUOS/application/Oxmux
./oxmux --probe-input         # press every button, copy the codes into [input]
ls /mnt/mmc/ROMS              # add any folder names that are missing from system.dirs
```

Edit `oxmux.toml` next to the binary. It overrides the built-in config.

## Roadmap

- [ ] Confirm input codes, RetroArch paths, and fb format on a real RG40XXV
- [ ] Power button: sleep/suspend (`/sys/power/state`) and power-off
- [ ] Volume/brightness hotkeys, settings screen, Wi-Fi
- [ ] Favourites, history, resume last game
- [ ] Box art / screenshots
- [ ] DRM/KMS backend (for mainline kernels with Panfrost)
- [ ] Replace the muOS shell scripts (boot, hotkeys, sleep, audio, brightness, storage, network) with a Rust supervisor configured declaratively
- [ ] Own bootable image: H700 kernel + minimal rootfs with oxmux as the frontend

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). Slint is used under its GPLv3 option.

Fonts: DejaVu Sans (Bitstream Vera / DejaVu license, free to redistribute).
