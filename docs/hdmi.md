# HDMI output

The mini-HDMI port can show the menu and games on a display. Status: complete
in code and checked against the kernel's build, the connector's sysfs
interface and the unit tests; not yet exercised with a display. The kernel's HDMI sound part is off by default (see Sound). The checklist
at the end is what a first session with a display must go through.

Configuration → HDMI (shown on boards that have the connector) offers:

| Setting | With a display connected |
| --- | --- |
| Auto (default) | The menu and games show on HDMI; the handheld's screen goes dark. |
| Mirror | The menu shows on both screens; games run on HDMI and the handheld shows a notice. |
| Off | HDMI is never used. |

Without a display every setting behaves as before. Boards whose device tree has
no HDMI connector, and profiles without `[display.hdmi]`, never poll or touch
anything HDMI related.

## How the menu reaches a display

`src/frontend/platform/kms.rs` keeps the handheld's screen and, when the
profile enables HDMI, a second surface on a second CRTC. The menu is drawn once
at the panel's size. An HDMI display gets that picture scaled with a bilinear
filter to the largest size that keeps its aspect ratio, with bars on the sides
(`platform/scale.rs`); the display engine has no usable plane scaler in
mainline. Only the part of the output a damaged source region can reach is
rewritten.

The mode is `[display.hdmi] mode` (1280x720) at the rate nearest 60 Hz;
displays that lack it get the largest progressive mode not taller than it, then
their preferred mode (`hdmi::pick_mode`). Refresh is computed the way RetroArch
does, so that a game later asks for the identical mode and the link stays up
instead of retraining.

The frontend reads the connector's `status` in sysfs every two seconds, but
only after the first frame, so the boot path never waits for it and boards
without HDMI never poll. `Routing` (`platform/routing.rs`) turns policy plus
cable into a `Layout` (panel, HDMI, mirror) and applies it:

- the KMS surfaces change, then the whole menu is redrawn;
- the daemon is told (`output panel=on|off sound=speaker`), which turns the
  panel's backlight off or on. `apply_brightness` holds the backlight at zero
  while the panel is dark, so dimming, hotkeys and wake-ups cannot light it;
- a failure (no EDID, no usable mode, no CRTC) falls back to the panel, shows a
  toast and is not retried until the cable or the setting changes.

If HDMI's only possible CRTC is the panel's, both screens cannot show at once:
Mirror degrades to Auto.

## How games reach a display

RetroArch and SDL (standalone PPSSPP) both draw on the first connected
connector. Before a launch the frontend forces the other connector off through
sysfs (`echo off > /sys/class/drm/cardN-DSI-1/status`, `echo detect` after the
game), so either emulator picks the right screen without configuration. Both
start from the mode already on the CRTC, which is why the menu and the game
share one. A frontend that dies during a game leaves the forced state behind;
the next frontend start releases it.

While a game runs on HDMI the handheld's screen shows "Playing on HDMI" and how
to quit. In Auto its backlight is off; if the cable is pulled the daemon, which
checks the cable every two seconds while the panel is dark or HDMI carries
sound, lights the panel and turns the speaker on again. The game itself keeps
running on a connector that no longer exists: quit with Menu + Start (RetroArch
saves its resume state; standalone PPSSPP has none, like any other quit).
Hotplug is handled between games, not inside one.

## Sound

The H616's HDMI sound goes through the audio hub, which mainline Linux does not
drive. The kernel builds ROCKNIX's port of Allwinner's driver
(`0203-sound-soc-Add-sunxi_v2-for-h616-ahub.patch.disabled`, which ROCKNIX
keeps off) with `nix/sunxi-ahub-7.2.patch`: the removed `of_gpio.h` include,
`snd_soc_kcontrol_component` (now `snd_kcontrol_chip`) and `card->dapm`
(a pointer now) were all it needed to build against 7.2.

ROCKNIX enables the hub's device-tree nodes in the base tree. Here they ship
disabled (`nix/h700-hdmi-audio-overlay.patch`): an enabled hub card can take
card number 0 from the codec, which `device.toml`, `audio-init` and the
RetroArch configuration all name by number, and its probe would run on the boot
path. Instead the first launch that goes to HDMI asks the daemon for
`sound=hdmi`, which applies `ghazal-hdmi-audio.dtbo` through configfs
(`nix/of-overlay-configfs.patch` adds mainline's missing
`/sys/kernel/config/device-tree/overlays/` interface). The card appears as
`HDMI`, number 1. If anything in that chain fails the daemon answers
`sound=speaker`, the launch goes to the speaker and the failure is in
`daemon.log`.

The daemon turns the speaker amplifier off while HDMI carries the sound, as it
does for headphones. Volume keys still move the codec's volume, which HDMI
does not use: set the loudness on the display.

Each launch states its device. RetroArch gets `--appendconfig` with an
`audio_device` line (`/run/ghazal/retroarch-route.cfg`, rewritten every launch
because RetroArch saves its effective settings on exit); SDL programs get
`AUDIODEV=plughw:HDMI,0` only for HDMI.

The kernel Image grows by 2 KiB for the audio hub driver, the machine driver
and the configfs interface (15,550,472 to 15,552,520 bytes); none of them probes
a device at boot.

This kernel part is off by default (`hdmiAudioKernel = false` in `flake.nix`,
the `hdmiAudio` argument of `nix/h700-kernel.nix`) because it has not booted on
a handheld. Without it the kernel has none of the three patches or the DT
change, the image has no `ghazal-hdmi-audio.dtbo`, and the first HDMI launch
logs "HDMI sound unavailable, staying on the speaker: reading /boot/overlays/ghazal-hdmi-audio.dtbo" in `daemon.log` and plays
on the speaker. Video, Mirror and the cable handling do not depend on it. Set
the flag to `true`, build and flash slot B with `BOOT_B_LEFT=1` and check the
boot before the first `sound=hdmi`.

## Files

| File | Purpose |
| --- | --- |
| `src/hdmi/mod.rs` | Policy, layout, mode choice, fitting, wire format (pure, tested) |
| `src/hdmi/connector.rs` | The connector through sysfs: status, modes, forcing |
| `src/hdmi/audio.rs` | The HDMI sound card: detection, overlay application |
| `src/frontend/platform/kms.rs` | Surfaces, layouts, hand-off to emulators |
| `src/frontend/platform/routing.rs` | Cable polling, launch plan, daemon requests |
| `src/frontend/platform/scale.rs` | Bilinear scaling onto the HDMI buffer |
| `src/daemon/mod.rs` | Panel backlight, speaker switch, cable watch (`ctl output`) |
| `config/rg40xxv-rocknix/device.toml` | `[display.hdmi]`, `[audio.hdmi]` |
| `nix/h700-kernel.nix` and three patches | Audio hub driver, configfs overlays, disabled nodes (`hdmiAudio` only) |

`ghazal observe hdmi` prints the connector, its status and modes, and whether
the sound card and overlay exist; `ghazal observe frame FILE.ppm hdmi` captures
what HDMI is scanning out, where plain `frame` takes the first active CRTC. `ghazal ctl output panel=off sound=hdmi`
exercises the daemon half over SSH (the reply says what was possible).

## Checklist for a session with a display

Each step names what to look at; `ghazal observe hdmi`, `dmesg` and
`/var/log/ghazal/daemon.log` and `retroarch.log` hold the evidence.

1. Boot with nothing connected. Menu appears as before; Configuration shows
   HDMI: Auto with detail "No display". `observe hdmi` reads `Disconnected`.
2. Plug in a 16:9 TV in the menu. Within about three seconds the menu appears
   pillarboxed on it and the panel goes dark; the detail reads "Active
   1280x720". Unplug: the panel returns within about three seconds.
3. Repeat with a 4:3 or 16:10 monitor and a display that lacks 720p; check the
   mode in `daemon.log`/`frontend.log` ("ghazal: HDMI ...").
4. Set Mirror: both screens show the menu, the handheld at full size.
5. Boot with the cable already in; the first frame is on the panel, HDMI takes
   over after a moment.
6. Launch a RetroArch game in Auto. The picture is on HDMI at 1280x720 without
   the display resyncing, sound comes from the display (from the speaker with the
   default kernel), the speaker is silent (with a kernel built with `hdmiAudio`),
   Menu + Start quits and the menu returns on HDMI.
7. (Needs a kernel built with `hdmiAudioKernel = true`; without it sound stays
   on the speaker and `observe hdmi` says the overlay is missing.)
   `cat /proc/asound/cards` shows `HDMI` after the first such launch;
   `ghazal observe hdmi` reports the card present. Check `dmesg` for the
   overlay and the audio hub probe.
8. Launch standalone PPSSPP (PSP) the same way: picture and sound on HDMI.
9. Mirror, then launch: the handheld shows "Playing on HDMI" and stays lit.
10. Pull the cable during a game: the panel lights and shows the notice, the
    speaker returns, Menu + Start quits to the handheld.
11. Policy Off with a display connected: the menu and games stay on the
    handheld.
12. Plug in during a launch and unplug right after returning; no state is left
    behind (`cat /sys/class/drm/card*-*/status` agrees with reality).
13. Headphones plugged in while on HDMI sound; unplugged afterwards: the
    speaker follows the jack again.

## Limitations

- The menu is scaled, not drawn at the display's size, and is soft on a large
  display. Drawing it natively (Slint's scale factor on a resized window) is
  the next step; the panel notice and the Mirror layout need a panel-size
  render, which is why it is not done yet.
- Hotplug during a game is not handled beyond restoring the panel and speaker.
- HDMI has no volume control here, and no CEC.
- `Mirror` cannot mirror games: an emulator owns one connector.
- The audio hub driver is the vendor port at ROCKNIX's pin. It is untested on
  7.2 and may need fixes for sample rates and channel layouts once there is a
  display to listen on.
