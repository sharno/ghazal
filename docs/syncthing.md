# Syncthing

Syncthing keeps folders such as `SAVES` the same on the handheld, a PC and a
phone. Configuration → Syncthing turns it on and off, shows this handheld's ID,
pairs devices, accepts shared folders and opens Syncthing's own web interface
on the network, without an SSH tunnel. Syncthing's identity, configuration and
database stay at `/mnt/games/.ghazal/syncthing` on the data card, so pairing
survives an OS-card reflash; Syncthing waits for a prepared data card
(`/mnt/games/ROMS`) before it starts.

## The screen

| Control | Action |
|---|---|
| Up / Down, L1 / R1 | Move, page |
| A | Turn on or off, accept, rescan, or confirm |
| Y | Ignore a pairing or folder request (asks first) |
| B | Back |

Rows, top to bottom:

- **Syncthing**: on or off. Off stops Syncthing and keeps it off after a reboot;
  it is on by default, so a card that synced before keeps syncing. While it
  starts, or while no data card is in, the screen shows only this row.
- **This device**: its name. While selected, the panel on the right shows the
  device ID as a QR code with the ID spelled out below (the panel needs a
  screen at least 400 pixels wide); A shows the ID as a message.
- **Requests** (marked `!`): a new device that wants to pair, and a folder a
  paired device shares. A accepts, Y ignores from now on (undo that in the web
  interface).
- **Folders**: `Up to date`, `Syncing 42%`, `Scanning`, `Paused`, `N errors`
  (files it could not sync), `Waiting for a device` (behind, and nobody online
  has the rest), `Not shared` or `Stopped` (for example, its directory is
  missing). A rescans the folder.
- **Devices**: `Connected`, `Paused` or `Last seen 3 h ago`.
- **Web interface on the network**: `Off`, or the address to open, such as
  `http://192.168.1.20:8384`.
- **Web interface login**: user `ghazal` and its password, once there is one.
  A makes a new password (asks first).

The screen refreshes every few seconds while it is open, and stops when you
leave it.

## Pairing a phone

1. Install Syncthing on the phone (on Android, Syncthing-Fork).
2. On the handheld, open Configuration → Syncthing and select **This device**.
3. In the phone app, add a device and scan the QR code. Give it a name.
4. Within a minute the handheld lists **New device: (the phone)**. Press A.
5. Share a folder from the phone with the handheld. The handheld lists
   **(folder) from (the phone)**; press A.

## Pairing a PC

1. Put the PC on the same network and open Syncthing's web interface there.
2. Add Remote Device. The handheld shows up among the nearby devices (compare
   its first seven characters with the ID on **This device**); pick it.
3. Accept **New device: (the PC)** on the handheld with A, then share a folder
   from the PC and accept it the same way.

## Where accepted folders go

A folder labelled `Saves` goes to `/mnt/games/Saves`, or to a directory that
already exists there under the same name in any case: `Saves` lands in `SAVES`,
`bios` in `BIOS`. If that directory already has files, the screen asks first:
both devices end up with every file, and an edit made on both sides keeps both
copies (one as a `.sync-conflict` file). The screen refuses, with a reason, a
directory another Syncthing folder already uses, and a label that names no
directory; accept those in the web interface with a path of your choice.
Hidden names are never used, so a shared `.ghazal` would land in `ghazal`. Do
not sync `.ghazal/syncthing` itself.

## The web interface on the network

Turning it on makes a login (user `ghazal`, a random password shown on the
screen), gives it to Syncthing, and restarts Syncthing with its web interface on
every network interface at port 8384. Syncthing stores only a hash of the
password; Ghazal keeps the password in `/mnt/games/.ghazal/syncthing/ghazal.toml`
so the screen can show it. The login then protects the web interface everywhere,
including through an SSH tunnel; the screen keeps showing it while the
interface is off.

Turning it off restarts Syncthing with the web interface on the handheld only.
The address comes from the command line (`ghazal syncthing serve`), so an
address set in Syncthing's own settings never exposes it, and the interface
never goes on the network without a login: when `config.xml` has none, it stays
on the handheld, `/var/log/ghazal/syncthing.log` says so, and the row reads
`Not on the network yet` (also its wording for the few seconds of a restart).
Turning it off and on again gives Syncthing the login again.

The interface uses plain HTTP: turn it on only on a network you trust, and off
when you are done. An SSH tunnel still works too:

```sh
ssh -N -L 8385:127.0.0.1:8384 root@DEVICE_IP
```

then open `http://localhost:8385`.

## Battery

Syncthing keeps Wi-Fi awake and hashes changed files, which costs battery. On a
long session away from your other devices, turn it off here; it stays off,
across reboots too, until you turn it on again. Quit a game before
syncing its saves, and do not play the same game on two devices at once: both
copies of the save change, and one becomes a conflict file.

## Status bar

Syncthing's icon is hidden while it is off; dim while it starts or no paired
device is connected; bright while everything is up to date; in the accent color
while it syncs; red when a folder has errors or is stopped. The menu asks
Syncthing at most every 10 seconds, on a background thread, and only while the
menu is on screen.

## From a shell

`ghazal syncthing` does what the screen does, over SSH:

```text
ghazal syncthing status              # service, ID, folders, devices, requests
ghazal syncthing on | off
ghazal syncthing rescan FOLDER-ID
ghazal syncthing accept ID           # a pending device or folder ID
ghazal syncthing ignore ID
ghazal syncthing web on | off
ghazal syncthing password            # a new web interface password, printed
```

The supervisor runs `ghazal syncthing serve` (the service) and
`ghazal syncthing restore` (at boot). Syncthing is updated with the OS image;
its automatic upgrades are off. Logs: `/var/log/ghazal/syncthing.log`.
