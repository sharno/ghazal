# Files and SD-card layout

## Physical partitions

A/B images built from current source use this MBR layout on TF1. Beta 4 and
earlier use the previous single-slot layout; see [updates and migration](updates.md).

| Partition | Label / format | Purpose |
|---|---|---|
| TF1 p1 | `GHAZALBOOT`, FAT32, 128 MiB | Optional provisioning |
| TF1 p2 | `GHAZALROOT`, ext4, 1 GiB | Read-only OS slot A, including kernel and device tree |
| TF1 p3 | `GHAZALDATA`, exFAT | Games and user data; first boot uses the remaining card space |
| TF1 p4 | Extended container | Holds logical p5 and p6; not a filesystem |
| TF1 p5 | `GHAZALROOT`, ext4, 1 GiB | Read-only OS slot B, including kernel and device tree |
| TF1 p6 | `GHAZALSTATE`, ext4, 256 MiB | Persistent settings, identities, Wi-Fi and logs, mounted at `/var` |
| TF2 p1, when inserted | exFAT, FAT32 or ext4 | Preferred games and user data |

The first 16 MiB contains the MBR and the RAM-specific U-Boot/SPL. It is not a
filesystem. Do not move the boot or root partitions. The factory data partition
starts at sector 5017600 (512-byte sectors). First boot validates the complete
factory layout before altering that third partition; it never touches TF2.

## Data volume

The chosen data card is mounted at `/mnt/games`. The directory structure is
identical for one-card and two-card installations:

```text
GHAZALDATA/                 # or the root of your TF2 partition
├── ROMS/
│   ├── GB/
│   ├── GBC/
│   ├── GBA/
│   ├── NES/
│   ├── SNES/
│   ├── PSP/
│   └── ...               # aliases supported by frontend.toml
├── BIOS/
│   ├── dc/               # Dreamcast/Naomi BIOS files, where required
│   └── ...
├── SAVES/
│   ├── file/             # SRAM, memory cards and standalone game saves
│   ├── state/            # emulator save states, grouped by core
│   └── screenshots/
└── .ghazal/
    ├── activity.toml     # Collection and History
    ├── thumbnails/       # downloaded art and missing-art markers
    ├── retroarch/        # persistent RetroArch configuration
    └── syncthing/        # this card's Syncthing identity and database
```

Folder names and extension matching come from `/etc/ghazal/frontend.toml`.
Keep multi-file games together; use playlists when a system supports them.
ROMs and proprietary BIOS files are not included. Box art can live beside ROMs
in `Imgs/<game>.png`, in a preserved muOS catalogue, or in `.ghazal/thumbnails/`.
The Collection is the favorites list; History holds the latest 100 unique games.

PPSSPP shares game saves through `SAVES/file/PPSSPP/PSP/SAVEDATA` and stores its
standalone states under `SAVES/state/PPSSPP-standalone`. The rotated RG28XX uses
the libretro PSP core instead of standalone PPSSPP, so core states are separate.

Back up **ROMS, BIOS, SAVES and .ghazal**, including hidden files. Syncthing's GUI
is local-only on port 8384; use an SSH tunnel to configure it. Avoid cloning a
running Syncthing identity to two handhelds; create a separate identity on each.

## Boot and OS files

```text
GHAZALBOOT/
└── provision.toml.example

GHAZALROOT/                # either OS slot; matching boot files update together
├── boot/
│   ├── KERNEL
│   └── dtb.img
├── opt/ghazal/ghazal        # static runtime: init, supervisor, daemon and menu
├── etc/ghazal/
│   ├── device.toml
│   ├── system.toml
│   ├── frontend.toml
│   ├── retroarch.cfg
│   ├── gamepad.cfg
│   ├── image.json        # device, RAM, release and hardware-source identity
│   └── hardware.lock
└── var/                   # separate GHAZALSTATE filesystem mounted here
    ├── lib/ghazal/
    │   ├── settings.toml     # brightness and volume
    │   ├── frontend-settings.toml
    │   ├── data-card         # auto / tf1 / tf2 preference
    │   ├── machine-id
    │   ├── ssh/              # authorized_keys and SSH host identity
    │   └── update-channel
    ├── lib/iwd/              # provisioned Wi-Fi credentials
    ├── lib/rauc/             # update status
    └── log/ghazal/           # bounded service and emulator logs
```

`/run` and `/tmp` are RAM filesystems. The active data-card record, live level snapshot, control socket
and emulator session record live there and disappear on reboot. The factory
storage marker has Fresh and Formatting ownership states. It is durably
changed before the formatter writes, allowing an interrupted initial format
to resume. It is removed before the volume is ever mounted for user data.
Wi-Fi credentials, SSH keys and frontend settings are in TF1's persistent state
partition; saves and history are on the selected data card. OTA preserves both.
A whole-card TF1 reflash resets its state and data; back them up first.

`frontend-settings.toml` stores `startup = "menu"` (default) or `"last-game"`,
selected through Configuration → Startup. The last game comes from the active
card's `.ghazal/activity.toml`. `/run/ghazal/startup-claimed` prevents a supervisor
restart from relaunching the game during the same boot; `/run` clears on reboot.
