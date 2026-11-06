# Ghazal H700 beta

Beta 13 adds Configuration → Syncthing: turn it on or off, pair a phone by
scanning this handheld's ID as a QR code, accept or ignore new devices and
shared folders, see each folder's state and rescan it, and open Syncthing's own
web interface on the network with a generated password
([Syncthing](syncthing.md)). The status bar shows Wi-Fi signal, Bluetooth,
Syncthing (orange while syncing, red on errors) and HDMI. RetroArch's menu now
confirms with A and goes back with B, like Ghazal's; an existing RetroArch
config is changed once, and setting it back in RetroArch is kept. None of this
has run on a handheld yet: it was checked with unit tests, the desktop
simulator and, for Syncthing, a real Syncthing 2.1.3 in a container.

Beta 12 is beta 11 republished. Beta 11's release lacked five DDR4 images
(RG40XX H, RG40XX H v2, RG40XX V, RG40XX V v2 and RG CubeXX) and was withdrawn;
the code is identical, and publication now refuses an incomplete set.

Beta 11 puts every planned feature into one release so they can be tested and
fixed together. Most of what is new has never run on a handheld in its final
form; treat it as experimental and keep slot A (the previous release) as the
fallback.

**Betas 11 and 12 were not booted on a handheld before they were tagged.** The
development RG40XX-V v2 stopped answering after its first boot of a test image
that had Bluetooth built into the kernel, and nobody could power-cycle it before
tagging. Why it stopped is unknown. That kernel part is switched off in this
release, so the kernel is the one the handheld had booted before it. CI built
and checked every image (the runtime's layout, its RetroArch driven by a virtual
copy of the handheld's pad, and the menu booted in QEMU), and each feature below
except Bluetooth and HDMI ran on the handheld in an earlier build, but the final
combination has not. If a new slot fails to start, the previous one takes over
after three attempts; a kernel panic still freezes the RG40XX-V v2 until the
power button is held (see below).

New and experimental:

- **Choose the emulator per game or per system.** X on a game opens its actions
  (Play, Emulator, Host netplay, Join netplay); Emulator lists the alternatives
  (for example Snes9x 2010, VBA-Next, Nestopia, PicoDrive, PPSSPP's libretro
  core) and X again makes one the system's default. Picks are stored in
  `emulators.toml` on the data card. Battery saves are filed per core, so a game
  switched to another core starts without its old `.srm`. The alternatives'
  CPU clock floors are copies of their default's and unmeasured.
- **Moonlight game streaming** (Applications → Moonlight game streaming) from
  Sunshine or GeForce Experience: find or add a host, pair with a PIN, start an
  app, leave with Menu + Start. Software H.264 decoding on the four CPU cores,
  640x360 at 60 fps by default. Its menu side ran against a stand-in host; no
  stream has run against a real one.
- **PortMaster** (Applications → Install PortMaster, over Wi-Fi) and a Ports
  system for its games. PortMaster's own GUI, its runner, gptokeyb and the
  Love2D runtime started with test scripts; no port from its catalogue has run.
  32-bit (armhf) ports are not supported.
- **Wi-Fi setup** in Configuration → Wi-Fi: scan, join with an on-screen keyboard,
  hidden networks, forget and disconnect. Listing and the keyboard ran; joining a
  network has not (the only network in reach was the one the test link used).
- **RetroAchievements and netplay.** Configuration → Achievements account opens
  RetroArch's sign-in; Achievements switches off, on or hardcore. From a game's
  actions, host a RetroArch netplay session or join one found on the network or
  saved in `netplay-hosts.txt`. Signing in and a two-device session have not
  been tried.
- **HDMI output** (Configuration → HDMI: Auto, Mirror, Off) shows the menu and
  games on the mini-HDMI port. It has never run with a display. Sound stays on
  the handheld's speaker: the kernel's HDMI sound driver is off in this release.
- **Bluetooth controllers** are in the code (`ghazal bluetooth`, Configuration →
  Bluetooth, hot-plugged pads in the menu) but not in the kernel. The menu hides
  Bluetooth until a kernel with it boots.
- Other changes: the loopback interface is raised at boot, and a release is now
  published only after CI has opened the built runtime and booted the menu in
  QEMU (see the end of these notes).

Known issues in beta 11: the crash freeze described under beta 10; no real
stream, catalogue port, Wi-Fi join, RetroAchievements login, netplay session,
Bluetooth pad or HDMI display has been tried; and a capture taken with a menu
overlay open (`ghazal observe frame`) shows the picture shifted about 26 px
left, which nobody has compared with the panel.

Beta 10 restores game controls. Betas 8 and 9 started every RetroArch game
without the handheld's gamepad profile, so buttons and RetroArch hotkeys did
nothing; update from either. Kernel panics now show the kernel log on screen.
On the RG40XX-V v2 the handheld then stays frozen until the power button is
held, because emergency restarts don't reset it yet. A USB-C-to-USB-C charger
no longer makes the handheld switch its own 5 V output on and off into it.
Those chargers still can't charge the RG40XX-V: the board lacks the resistor
they need. Charge with a USB-A-to-C cable.

Every libretro core is now built from ROCKNIX's H700 recipe: the same
revision, patches and compiler flags as ROCKNIX's own build, so emulation speed
is within 5% of it. Flycast again reads core options saved under ROCKNIX.
RetroArch, standalone PPSSPP, Mesa and the system services are built from pinned
sources too; no ROCKNIX binaries are used. On the RG40XX-V every system with
games launches, plays sound, saves and quits, and existing RetroArch save states
and standalone PPSSPP states still load.

Lighter systems no longer pin the CPU at full speed. Game Boy, NES, Master
System, Game Gear, Mega Drive, PC Engine, MSX and Game & Watch games start from
720 MHz on the RG40XX-V; GBA, SNES, arcade, WonderSwan and 32X games from
1008 MHz. Up to 1008 MHz the CPU runs at a lower voltage, and a demanding scene
still raises the clock. Other systems run at full speed.

Power-on to the menu takes about 2.9 s on the RG40XX-V (4.4 s in beta 6): a
smaller kernel that mounts root sooner, network, SSH and udev starting after
the menu appears, and no `/var` journal replay after a clean shutdown. One-card
setups wait for a second card only on their first boot. The handheld charges at the stock
1.5 A instead of 1 A. The watchdog now runs from early
kernel start, so a boot that hangs resets and falls back to the other OS slot.
Plugging in headphones mutes the speaker. The CPU slows down in the menu. RG40XX H/V and RG CubeXX owners can set the stick
lights from Configuration; they turn red at low battery.

The menu battery indicator shows a green lightning bolt while connected to power.

This release adds signed RAUC system updates from GitHub Releases through
**Configuration → System update**. Updates install to the inactive OS slot;
failed starts fall back to the previous slot. Games, saves, settings and SSH
identity live outside both OS slots. Beta 4 and earlier require a one-time
migration: back up before flashing the new layout. See
[system updates and recovery](https://github.com/sharno/ghazal/blob/main/docs/updates.md).

Configuration → RetroArch settings opens the native emulator settings menu.
Changes persist on the data card and apply to games. Manual save states advance
automatically and retain ten numbered states per game; the automatic resume
state remains separate. Change saving defaults in RetroArch Settings → Saving.

This release fixes input leaking from an emulator into the menu after quitting:
gameplay can no longer trigger another launch, change systems or scroll the menu.
Held controls must return to neutral before the menu accepts new input.

RetroArch automatic save and resume remain enabled for cores that support save
states. A failed automatic save now produces a warning when returning to the
menu. In-game saves and automatic save states are separate.

Configuration → Startup offers Main Menu or Last Game. Automatic game launch is
attempted once per boot; exiting or failing to launch returns to the menu.

This beta provides 38 flashable images: 19 device/panel variants, each with
DDR3 and DDR4 bootloaders. Every image supports one-card and two-card setups.
Choose the model, panel and actual RAM revision before flashing.

- [Installation](https://github.com/sharno/ghazal/blob/main/docs/installation.md)
- [Files and storage](https://github.com/sharno/ghazal/blob/main/docs/storage-layout.md)
- [Device catalog](https://github.com/sharno/ghazal/blob/main/config/h700/devices.toml)

Images include the ghazal menu, RetroArch cores, standalone PPSSPP on unrotated
devices, SSH/SFTP, Wi-Fi setup and Syncthing. Public images contain no personal
credentials, ROMs or proprietary BIOS files. Each handheld creates its own
machine ID and SSH host key. SHA256SUMS covers every compressed image and
signed update bundle. Use `.img.gz` to flash a card and `.raucb` for RAUC updates.

CI builds both bootloaders, all device trees and the common kernel from pinned
sources, runs Rust checks, audits public provisioning, checks both image
filesystems and verifies the complete artifact inventory before publication.
It also opens the built runtime (every core, command, PPSSPP asset and the
gamepad profile), runs its RetroArch against a virtual copy of the handheld's
pad and boots the menu in QEMU; publication waits for all three
([release checks](https://github.com/sharno/ghazal/blob/main/docs/release-checks.md)).
A disposable loop-device test grows p3 while p1/p2 are mounted, kills a
formatter after its first write, recovers that transaction, checks save retention
and proves unknown data is refused. Provisioning errors are reported on the
boot volume and the device can continue offline. New A/B images use RAUC
for signed updates and slot rollback.
Kernel, U-Boot and the emulator runtime are built from pinned nixpkgs and
ROCKNIX sources; `image.json` records both revisions.

**Hardware acceptance:** RG40XX-V v2 with DDR4 has been exercised on a physical
device, including both OS slots, signed installations from ext4/exFAT data
volumes, retained saves/settings/identities, missing-kernel fallback and three
failed starts with watchdog resets followed by fallback. Deliberate power
removal during installation/environment writes still needs validation.
Other models, panel revisions and DDR3 targets are experimental; a
successful CI build does not prove their display, controls, sound or Wi-Fi.
Test on a spare card and report the exact image and hardware revision.

Sleep is off by default: Configuration → Sleep turns it on, but wake has not
been validated on hardware and a failed resume needs Reset ([sleep](sleep.md)). Clamshell lid suspend is not implemented.
Charging stability is unresolved on RG40XX-V v2: USB power detection can cycle
and the battery can continue discharging while connected. The lightning-bolt
indicator does not fix this power-detection issue. Charging is not hardware-accepted yet.
Some systems require user-supplied BIOS files and stay hidden until available.
RG28XX uses rotated menu output and the libretro PSP core; art previews are
hidden on its compact screen. RG28XX and RG35XX 2024 have no built-in Wi-Fi.
Updates are requested manually; no background download or automatic reboot runs.
Routine RAUC updates preserve TF1 data too. Reflashing the whole card erases it.
