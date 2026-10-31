# Release checks

Betas 8 and 9 shipped the H700 gamepad profile in `autoconfig/` instead of
`autoconfig/udev/`. Everything built, booted and played, and no game had
controls. Two layers of checks now stand between a change and a release: CI
gates that open the built runtime, and an acceptance run on a real handheld.
Neither replaces the other; CI proves the build, the handheld proves the device.

## CI gates

Every push and pull request runs them; a tag build also needs them before it
publishes. They live in [`.github/workflows/images.yml`](../.github/workflows/images.yml)
and in `tools/release-check`, which derives what to check from the shipped profile
(`config/rg40xxv-rocknix`), so a new core, service or emulator is covered
without editing the checker.

| Job | What it proves |
| --- | --- |
| `release-tests`, layout | The built runtime tree matches the profile. The pad's profile is at `autoconfig/<joypad driver>/<name>.cfg` and identical to `gamepad.cfg`. Every `core` in `frontend.toml` exists as an aarch64 libretro library. RetroArch and each standalone command are aarch64 executables. PPSSPP has its assets and its binary contains their path. The SDL controller mapping for PPSSPP has the pad's GUID (vendor, product, version, name checksum) and agrees with `gamepad.cfg` button by button. Every `/usr` command the boot plan starts exists. The CA bundle and iwd's D-Bus policy exist. |
| `release-tests`, input | The runtime's own RetroArch, with null video and audio, runs a generated Game Boy ROM against a virtual copy of the handheld's pad (made through `/dev/uinput` with the real vendor, product, bus, version, buttons and sticks). It must configure the pad from the shipped profile in port 1; Menu + R1 must save a state and Menu + L1 load it; Menu + Start (twice, as RetroArch asks) must quit; the next launch must resume from the automatic state; and SIGTERM, which the daemon sends, must quit with status 0. |
| `vm-smoke` | `nix run .#vm-smoke` boots Ghazal as PID 1 on a stock kernel in QEMU: the supervisor starts the daemon and the menu, the menu draws (at least 16 distinct colours), a key press changes the screen, `ghazal observe launch` (the acceptance script's launcher) presses its way through the real menu to the last of ten games in the third system, and the menu returns when that game exits. Nothing may panic. |
| `checks` | Clippy and unit tests, including `tools/release-check` and the menu navigation used by the acceptance script. |

`publish` needs `checks`, `images`, `release-tests` and `vm-smoke`. Resuming a
publication from an earlier run (`artifact_run`) requires the run to contain all
of them as successful jobs. The release is only created, as a draft, after they
pass, and stays a draft until its assets are uploaded.

Limits of the gates:

- The input test needs `/dev/uinput`. The workflow runs `modprobe uinput`; a
  runner without it makes the tool exit with status 77, the step warns and the
  layout gate remains. Run `scripts/release-tests.sh input` on a machine with
  uinput before tagging in that case. With no udev daemon (a container), the
  tool writes the database entry the image's `70-gamepad.rules` produces; with
  one, it waits for udev to mark the pad.
- `vm-smoke` uses `config/qemu` and a stock kernel, not the H700 kernel or the
  Nix runtime. It covers the `ghazal` binary: PID 1, the supervisor, the daemon
  and the menu's KMS and input paths. It cannot show H700 drivers, emulators or
  suspend working.
- The layout gate takes BusyBox applets as `busybox <applet>`; the image only
  links the applets listed in the image builder.
- Nothing here shows sound, picture quality or speed. The acceptance run checks
  that the PCM runs and the frame has content; listening and judging play are
  manual.

### Running them yourself

```sh
scripts/release-tests.sh            # layout, input, smoke
scripts/release-tests.sh layout     # one gate
nix build .#checks.aarch64-linux.runtime-layout   # the layout gate as a Nix check
```

The runtime is aarch64, so use an aarch64 Linux machine with Nix. On macOS the
script re-runs itself in a privileged `nixos/nix` container that shares the Nix
store volume (`GHAZAL_NIX_VOLUME`, default `oxmux-nix`); Docker's kernel
provides `/dev/uinput`. Measured in that container: layout takes under a second,
input about ten seconds and the QEMU smoke test about thirty, with the runtime
already built.

`GHAZAL_KEEP_WORKDIR=1` keeps the input test's RetroArch logs;
`GHAZAL_SMOKE_KEEP=1` keeps the smoke test's console log and screenshots.

## Acceptance on the handheld

`scripts/acceptance.sh` runs from your computer over SSH against a booted
handheld and prints a pass/fail table; it exits 1 when anything fails.

```sh
cp scripts/acceptance-games.example ~/ghazal-games.txt   # keep it out of the repository; list your ROMs
scripts/acceptance.sh --ssh 'ssh root@192.168.1.50' --evidence /tmp/acceptance ~/ghazal-games.txt
```

`--ssh` (or `GHAZAL_SSH`, or `GHAZAL_HOST`) is any command that runs its
argument on the handheld. The games file has one ROM path per line, as the menu
lists it. Without a games file only the system checks run. The script stops a
game that is already running.

System checks: SSH reachable and the installed version; the booted slot is
first in `BOOT_ORDER` with all three attempts left (the health service marked it
good, so there was no fallback); power-on to first frame within `--max-boot`
(default 3.5 s, the measured budget is about 2.95 s; measure after a clean
shutdown); the menu is drawn (a frame capture with at least 16 distinct
colours); the daemon answers; the kernel log has no oops or BUG.

For each game the script launches it through the menu itself. `ghazal observe
plan ROM` runs the menu's own library scan and prints the system, the launch
kind, the expected clock policy and the button path (five B presses to reach the
home screen, Explore Content, L1 pages to the top of each list, R1 and Down to
the target); `ghazal observe launch ROM` presses that path on the real gamepad.
Then it checks:

| Check | RetroArch | PPSSPP |
| --- | --- | --- |
| Launch | the menu starts the emulator | same |
| Gamepad | the log says `configured in port` and never `not configured` | the process holds a `/dev/input/event*` device open |
| Audio | `/proc/asound/card0/pcm0p/sub0/status` says `state: RUNNING` | same |
| Picture | a frame capture has at least 16 distinct colours | same |
| CPU policy | governor and floor match the system's `cpu` load (`frontend.toml`, `device.toml [cpu]`) | same |
| Save state | Menu + R1 writes a state, Menu + L1 loads it | skipped |
| Quit | Menu + Start ends the emulator; RetroArch wrote its automatic resume state | Menu + Start ends the emulator |
| Menu back | the CPU returns to the menu policy and the menu is drawn | same |

RetroArch runs through a wrapper bound over its store path for the duration of
the run (`mount --bind`, removed on exit). The wrapper appends a scratch
configuration so save files, save states and core options go to
`/tmp/ghazal-acceptance` and never touch the data card. RetroArch silently falls
back to its defaults when those directories do not exist, so the script creates
them first. Save-state files are checked there, not in `SAVES`. PPSSPP's save
locations are compiled in, so the script only launches and quits it; PPSSPP has
no automatic state.

If a game fails, `--evidence` keeps its frames (`.ppm`; convert with `ffmpeg -i
x.ppm x.png`) and the emulator's log lines from that launch.

## Release checklist

1. CI is green on the commit to tag, including `release-tests` and `vm-smoke`.
   If the input test was skipped for lack of `/dev/uinput`, run
   `scripts/release-tests.sh input` first.
2. Put the build on the handheld's dev slot (`docs/updates.md`, AGENTS.md
   development loop) and boot it. Keep slot A as the fallback.
3. Run `scripts/acceptance.sh` with a games file that covers every system whose
   core, recipe or emulator changed and one PSP game. Fix every failure or
   record why it is not a regression.
4. Check by hand what the script cannot: sound through the speaker and
   headphones, picture, controls feel, a long play, charging, Wi-Fi, a system
   update from the previous release.
5. Tag and publish (`gh release create --verify-tag`, AGENTS.md). Never
   overwrite a published release.
6. After publishing, install the release on a spare card or through System
   update, and run the acceptance script against it.

When a release breaks something these checks missed, add the check that would
have caught it before fixing anything else.
