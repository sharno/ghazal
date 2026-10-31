# Wi-Fi setup on the device

Configuration → Wi-Fi shows the connection, scans, and joins, forgets and
disconnects networks without a computer. `provision.toml` (see
[installation](installation.md)) still seeds a first connection; both end up as
iwd's own saved networks in `/var/lib/iwd`, on the persistent state partition.

## Using it

| Control | Action |
|---|---|
| Up / Down, L1 / R1 | Move, page |
| A | Connect to the network, or confirm |
| X | Scan again |
| Y | Forget a saved network (asks first) |
| B | Back; cancels a connection attempt |

The top row is the connection (A shows its signal, band and security). Below it:
Scan for networks, every network in range (★ saved, ● connected, with signal bars and
`Open`, `WPA` or `802.1X`), saved networks that are out of range, and Join a
hidden network.

- **Open or saved networks** connect straight away. A on the connected network
  asks before disconnecting, so a stray press does not drop an SSH session.
- **A new password-protected network** opens the on-screen keyboard. A refused
  password returns to the keyboard with the text kept and the reason above it.
- **Hidden networks** ask for the name, then the password (empty means open, or
  already saved). Once joined they are saved, listed as out of range (they never
  appear in scans) and joined again with the same hidden-network probe.
- **802.1X (enterprise) networks** are listed but cannot be joined here.
- Errors are short messages: the service is not running, no adapter, out of
  range, refused, did not answer in time.

The keyboard (`src/frontend/keyboard.rs`, `ui/keyboard.slint`) is reusable by any
screen that needs typed text:

| Control | Action |
|---|---|
| D-pad | Move over the keys |
| A | Type the highlighted key (or use its action: layer, space, delete, Cancel, OK) |
| B | Delete the last character |
| X | Cycle abc, ABC and symbols; ABC applies to one character, symbols stay |
| Y | Show or hide the password |
| Start | Confirm (same as OK) |
| Menu | Cancel |

It types ASCII only: every printable ASCII character is reachable, which covers
WPA passphrases (8 to 63 characters, as in `provision.toml`). Names with other
characters can be joined when they are in range, since they are not typed.
WPA3 passwords shorter than 8 characters are refused by the keyboard.

## How it works

`src/frontend/wifi/`:

- `types.rs`: validated values (`Ssid`, `Passphrase` whose `Debug` is redacted,
  `Signal`, `Security`) and the `Snapshot` a refresh produces.
- `parse.rs`: turns iwctl's coloured tables into those values. The fixtures in
  its tests were captured from iwd 3.12 on the handheld. Names are found by the
  security word after them, not by column, so SSIDs with runs of spaces or
  multi-byte characters survive.
- `backend.rs`: the `Backend` trait and `Iwctl`, which runs `/usr/bin/iwctl`
  with `--dont-ask` (a missing credential is an error, never a prompt nobody
  can answer) and `--` (an SSID that looks like an option is still an SSID). A
  passphrase goes to iwd through `--passphrase=`, which makes iwctl answer
  iwd's agent request; it is visible in the process list for the seconds a
  connection takes. iwd then saves the network itself.
- `screen.rs`: the screen as a state machine: Loading, Unavailable, Browse,
  Confirm, Entering, Working, Closed. `step(screen, event)` is pure; every
  state that can go back holds the list it returns to. This is where nearly all
  the tests are.
- `controller.rs`: a worker thread runs the commands (a refresh is three
  iwctl calls), polls while scanning or while an address is pending, and
  refreshes every 6 s otherwise. Nothing runs while the keyboard is up.

Nothing starts at boot. The worker is created when Wi-Fi is opened and ends
when it is closed, so the first frame and the supervisor are unchanged. The
release binary grows by 197 KB (11.30 to 11.49 MB, measured against main
with the Moonlight merge); that code is not touched before the screen opens. The
Configuration entry appears only when the image has iwctl and a wireless
interface exists (`/sys/class/net/*/phy80211`; RG28XX and RG35XX 2024 have none).
If iwd is not running (it starts in the background phase) iwctl prints nothing
and waits for it, so the first command is cut off after 8 s and the screen says
the service is not running, with Try again.

iwctl 3.12 behaviour this relies on, checked off the device: `--` ends option
parsing (without it a name such as `-foo` is "invalid option"), `--passphrase=-abc`
is accepted, bad options exit 1, and a missing iwd means waiting, not failing.
Checked on the handheld: an unknown network name gives `Invalid network name
'X'`, and a connect that needs a passphrase under `--dont-ask` gives `No Agent
registered` (the screen treats it like a refused password and asks again). Not
yet checked on hardware: the wording for a refused password (`KNOWN_FAILURES`
in `backend.rs` matches iwd's messages by text and assumes `Operation failed`)
and whether a failed `connect` exits non-zero; both are handled either way.

To see the unavailable state without dropping an SSH session over Wi-Fi, stop
iwd instead of killing it: `kill -STOP <iwd pid>` leaves the kernel link up,
and `kill -CONT` brings it back (keep a detached `sleep 60; kill -CONT` as a
safety net, BusyBox `setsid` needs `busybox setsid sh -c ...`).

The desktop simulator (`cargo sim`) uses a pretend radio so it never touches
the host's Wi-Fi: networks to join, a saved one, a hidden one, and the password
`ghazal123` for the unsaved ones. Set `GHAZAL_WIFI_REAL=1` to drive the host's
iwd instead. Render screens without a window, one PNG per button press, with:

```sh
# Home: Down x5 then A opens Configuration; Down x3 then A opens Wi-Fi;
# Down x2 then A asks for the password of the third network.
cargo sim --snapshot /tmp/wifi "down,down,down,down,down,a,down,down,down,a,down,down,a,a,a,a"
```

## Checking it on a handheld

Joining, disconnecting or forgetting the network an SSH session runs over ends
that session; iwd reconnects to a saved network on its own within seconds.
Drive the menu with `ghazal observe press` and read results with `iwctl` once
the link is back.

1. Configuration → Wi-Fi shows the same network and address as
   `iwctl station wlan0 show`; X rescans and the list changes.
2. A new password-protected network: a wrong password returns to the keyboard
   with the text kept; the right one connects and `/var/lib/iwd/<name>.psk`
   appears. An open network connects without a prompt.
3. B while connecting cancels; the list returns and iwd is left idle.
4. Y then Yes forgets a saved network and deletes its `.psk`; it is listed again
   only while in range.
5. Join a hidden network, then reboot: it is saved, listed as out of range and
   joins again from the list.
6. With iwd stopped (`killall iwd`; the supervisor restarts it) the screen says
   the service is not running and Try again recovers.
7. Boot time (`ghazal observe boot`) is unchanged.

## Not done

- Enterprise (802.1X) networks, WPS and ad-hoc/AP modes.
- Turning the radio off, or choosing between several adapters (the first
  station iwd lists is used).
- Non-ASCII text on the keyboard.
- The iwctl approach costs a few process starts per refresh. Moving to iwd's
  D-Bus API would remove the output parsing and the passphrase on a command
  line; `Backend` is the seam for that.
