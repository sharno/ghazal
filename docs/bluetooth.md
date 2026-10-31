# Bluetooth

Pair Bluetooth gamepads from **Configuration → Bluetooth** and play with them
beside the handheld's own controls. The RG40XX-V's RTL8821CS puts its Bluetooth
on a UART (the Wi-Fi is on SDIO). Nothing Bluetooth runs until the user turns it
on, so a device that never uses it pays nothing at boot.

Status: implemented end to end, but **off in every image**: the kernel's
Bluetooth stack is behind `bluetoothKernel` in `flake.nix` (default `false`),
because a kernel with it has not yet booted on a handheld. Everything else (the
menu screen, the CLI, BlueZ in the runtime, the udev rule, hot-plugged pads in
the menu) ships and stays dormant: with no controller bound the menu hides
Bluetooth. Set `bluetoothKernel = true` to build the kernel below into the
images and test it. Pairing and playing with real controllers, and A2DP audio,
are still to be tested or built (see [Audio](#audio-a2dp-is-not-implemented)).

## From controller to game

```
menu: Configuration → Bluetooth        src/frontend/bluetooth.rs  (list, buttons)
  state machine, orders and events     src/bluetooth/model.rs
  worker thread                        src/bluetooth/worker.rs    (menu never blocks)
  BlueZ over D-Bus                     src/bluetooth/bluez.rs, dbus.rs
bluetoothd (supervised, on demand)     nix/runtime/bluez.nix
kernel: hci_uart_h5 → btrtl            nix/h700-kernel.nix, nix/hci-h5-start-attribute.patch
RTL8821CS firmware                     /usr/lib/firmware/rtl_bt/rtl8821cs_{fw,config}.bin
paired pad → uhid → /dev/input/eventN  menu: evdev_input.rs · games: RetroArch's udev joypad driver
```

### Kernel

With `bluetoothKernel = true` (`nix/h700-kernel.nix`'s `bluetooth` argument),
Bluetooth is built in with no modules: the core with LE, the three-wire UART
transport with the Realtek protocol, HIDP and UHID for HID over Bluetooth, and
the HID drivers for Nintendo (Switch Pro, Joy-Con) and Microsoft pads.
`hid-playstation`, already built in, covers DualShock 4 and DualSense. The
KERNEL image grew from 15,550,472 to 16,082,952 bytes (+0.53 MB, about 25 ms of
firmware time at U-Boot's ~21 MB/s read speed).

The controller's DT node comes from mainline's `sun50i-h700-anbernic-rg35xx-plus.dts`,
which the RG40XX V and H boards include: `&uart1` with RTS/CTS and a
`realtek,rtl8821cs-bt` node (enable PG19, device-wake PG17, host-wake PG16).
Boards whose device tree lacks it simply have no controller, and the menu hides
Bluetooth.

Powering the controller sleeps 0.6 s in the driver's probe, which root mounting
waits for; enabling Bluetooth at boot cost that much on every start. The kernel
patch moves it out: probe only claims the GPIOs, and writing `1` to
`/sys/bus/serial/drivers/hci_uart_h5/serial*/start` powers the controller and
starts the three-wire link; `hci0` appears once the controller answers and its
firmware has loaded. The write takes about a second. Reading the attribute says
whether it was done. The firmware loads from the root filesystem at that
point, so it is not embedded in the kernel.

### Services

`bluetoothd` (`nix/runtime/bluez.nix`: GLib and D-Bus only; 78 MiB closure
including glibc and GLib, +5 MiB over the previous runtime) is a supervised
service with `wait_for = ["/run/ghazal/bluetooth-requested"]`. That marker and
the kernel's `start` attribute are what `ghazal bluetooth on` (the menu's
Bluetooth switch) creates, in `src/bluetooth/hardware.rs`. `bluetooth-restore`, a
background oneshot, repeats this at boot when Bluetooth was on when the device
last ran (`/var/lib/ghazal/bluetooth.toml`, on the state partition shared by both
OS slots). A user who never turns Bluetooth on never starts `bluetoothd`.

BlueZ keeps pairing keys in `/var/lib/bluetooth` (state partition). Its
configuration is `config/rg40xxv-rocknix/bluetooth/{main,input}.conf`:
`AutoEnable = false` (the menu owns power), `UserspaceHID = true` (reports reach
the kernel through `/dev/uhid`) and `JustWorksRepairing = confirm`.

The menu talks to BlueZ itself over a small blocking D-Bus client
(`src/bluetooth/dbus.rs`, tested against a real `dbus-daemon`) rather than a
D-Bus crate, which would add an async runtime to a binary whose page-in time is
part of every boot. It registers a pairing agent while the screen is open: a
fixed PIN of `0000` for old devices, automatic confirmation for the rest, and an
unsolicited request to pair accepted only while scanning or pairing.

### The menu

Configuration → Bluetooth lists the radio switch, **Scan for devices** and the
devices BlueZ knows: connected first, then paired, then new ones by signal
strength, controllers before other devices. Nameless beacons are hidden.

| Button | Action |
|---|---|
| A on Bluetooth | Turn the radio on or off (turning on starts the controller and daemon, ~5 s) |
| A on Scan / X | Start or stop scanning |
| A on a new device | Pair, trust and connect |
| A on a paired device | Connect, or disconnect when connected |
| Y on a paired device | Forget it |
| B | Back, or cancel a pairing or connection in progress |

Put the controller in pairing mode (hold its pairing button until the light
flashes), scan, and select it. Scanning stops while pairing and when leaving
the screen. Paired controllers are trusted: they reconnect when their own
button is pressed, even if the menu was never opened this boot. Audio devices
are listed but marked "No audio yet".

The screen is an explicit state machine (`src/bluetooth/model.rs`): one
operation at a time (power, pair, connect, disconnect, forget), refusals that
say why, and events from the backend that must match the running operation.
`src/bluetooth/backend.rs` has a fake backend for tests and the desktop
simulator (`cargo sim`, or `GHAZAL_BLUETOOTH=real` for the host's BlueZ).

### Controllers in the menu and in games

- The menu accepts any gamepad besides the built-in one, hot-plugged: an inotify
  watch on `/dev/input` adopts a new pad when it connects and drops it silently
  when it goes away (losing the built-in pad still restarts the menu). Buttons
  map by `device.toml`'s codes, which are the standard evdev gamepad codes: east
  is A, south is B, the mode (guide/PS) button is Menu.
- The daemon already rescans input devices every 5 s and reads every key device,
  so a pad's activity resets the idle timer and **Menu + Start** (mode + start)
  exits a game from a Bluetooth pad too. It has no volume keys.
- RetroArch's udev joypad driver handles hotplug itself, and the image's stock
  autoconfig profiles match common pads by name. Stock udev rules are masked,
  so `71-input-hotplug.rules` runs udev's `input_id` classifier on every input
  device (the stock rule): without it nothing marks a Bluetooth or USB pad as a
  joystick. The built-in pad is
  player 1 (it enumerates first); a Bluetooth pad is player 2, or the next free
  port. RetroArch's hotkeys stay on the built-in pad. Change ports in
  RetroArch's own menu (Configuration → RetroArch settings → Input).

## Audio (A2DP is not implemented)

`bluetoothd` is built without A2DP and AVRCP, so audio devices pair and connect
as devices but carry no sound. Plan, in order:

1. Enable `a2dp` and `avrcp` in `nix/runtime/bluez.nix` (no new dependencies).
2. Package `bluealsa` (`bluealsad` and the ALSA plugin; needs `sbc`, optionally
   `fdk_aac`) against that BlueZ with `--enable-library`, and supervise
   `bluealsad` after `bluetoothd`.
3. When an A2DP sink is connected, launch emulators with its PCM instead of
   `hw:0,0`: RetroArch's `audio_device` through an appended config,
   PPSSPP's SDL through `AUDIODEV`. The ALSA plugin loads from
   `/usr/lib/alsa-lib` through an absolute `lib` in `pcm_type.bluealsa`.
4. Send the daemon's volume to the bluealsa mixer while a sink is connected,
   and let the headphone jack win over Bluetooth.
5. Expect 150-300 ms of latency; raise `audio_latency` for it and warn in the
   menu that rhythm games suffer.

## Checking a device

```sh
ghazal bluetooth status            # controller state, radio, preference
ghazal bluetooth on                # start controller and daemon, power the adapter
ghazal bluetooth scan 15           # lists what it finds, controllers in pairing mode first
ghazal bluetooth pair E4:17:D8:00:00:01
ghazal bluetooth devices
ghazal bluetooth connect|disconnect|forget ADDRESS
ghazal bluetooth off
```

`pair` needs a device BlueZ has seen, so scan first. With the controller
started: `btmon` traces HCI traffic (firmware download, pairing),
`bluetoothctl` is BlueZ's own client, `btmgmt info` shows the adapter, and
`dmesg | grep -i bluetooth` shows the firmware load (`Bluetooth: hci0: RTL:`).
`/var/log/ghazal/bluetoothd.log` holds the daemon's output and
`frontend.log` the screen's.
