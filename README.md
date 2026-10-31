# Ghazal

<img src="assets/ghazal.svg" width="96" alt="Ghazal gazelle logo">

A standalone Linux gaming system for Anbernic H700 handhelds, focused on
**fast boot, responsive menus and game performance**.

[Download a beta](https://github.com/sharno/ghazal/releases) ·
[Installation](docs/installation.md) ·
[Files and SD-card layout](docs/storage-layout.md) ·
[Device catalog](config/h700/devices.toml)

## What it does

- Browse systems and games, keep favorites and recently played games, and display box art.
- Play through RetroArch and standalone PPSSPP on supported devices, and pick
  another emulator for a single game or a whole system (for example Snes9x 2010
  or PPSSPP's libretro core).
- Install [PortMaster](docs/ports.md) from the menu (Wi-Fi) and run its ports.
- Start at the menu or resume the last game on boot.
- Automatically save and resume supported RetroArch games. Manual saves retain
  ten rolling numbered states, separate from the automatic resume state.
- Change brightness, volume, stick lights (RG40XX H/V, RG CubeXX) and persistent
  RetroArch settings from Configuration.
- Earn RetroAchievements and play RetroArch games together over Wi-Fi: host a
  netplay session or pick a host on the network from a game's actions.
- Pair Bluetooth gamepads from Configuration → Bluetooth and play with them
  beside the handheld's own controls.
- Mute the speaker while headphones are plugged in, and run lighter systems at a
  lower CPU clock than demanding ones.
- Stream PC games from Sunshine or GeForce Experience with Moonlight
  (Applications → Moonlight); see [game streaming](docs/moonlight.md).
- Use one SD card for everything, or separate OS and games/data cards.
- Install signed system updates from GitHub through Configuration → System update
  on A/B installations; see [updates and migration](docs/updates.md).
- Join Wi-Fi networks from Configuration → Wi-Fi with an on-screen keyboard
  ([Wi-Fi setup](docs/wifi.md)), connect over SSH, sync files with Syncthing,
  and fall back to the other OS slot when one fails to start. Kernel panics show
  the kernel log on screen; on the RG40XX-V v2 they then need a power cycle.

## What we care about

- **Boot time:** reach a usable menu quickly, scan games in the background and
  keep nonessential work off the startup path.
- **Performance:** responsive navigation, low system overhead and measured
  emulator performance on real hardware.
- **Reliability:** preserve saves, return cleanly from games and keep recovery accessible.
- **Simplicity:** a small, maintainable system with clear ownership of configuration.

## Controls

| Action | Control |
|---|---|
| Open / back | A / B |
| Favorite a game | Y |
| Game actions (choose its emulator, host or join netplay) | X |
| Volume | Volume Up / Down |
| Brightness | Menu + Volume Up / Down |
| RetroArch menu during a game | Menu + X |
| Save / load selected manual state | Menu + R1 / L1 |
| Quit a game | Menu + Start |

Choose **Configuration → Startup** for menu or last-game startup.
**Configuration → RetroArch settings** opens the native emulator settings menu;
changes persist on the data card. **Configuration → Achievements** switches
RetroAchievements on once an account is set up under **Achievements account**.

## Beta status

Releases include 38 images covering 19 device/panel variants with DDR3 and DDR4
bootloaders. Physical validation currently covers the RG40XX-V v2 / DDR4;
other targets are experimental. Native suspend/wake is disabled.
Games and proprietary BIOS files are supplied by the user.

See [release notes and limitations](docs/beta-release.md),
[building and debugging](docs/standalone.md), and
[remaining work and development priorities](AGENTS.md).

## License

[GPL-3.0-or-later](LICENSE). Slint uses its GPLv3 option.
DejaVu Sans is distributed under the Bitstream Vera / DejaVu license.
