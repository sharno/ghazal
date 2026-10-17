# Installing the H700 beta

For network updates after installation, see [System updates](updates.md).
Beta 4 and earlier need the one-time A/B migration described there.

Download images from [GitHub Releases](https://github.com/sharno/ghazal/releases).
Every image supports **either one SD card or two SD cards**. Choose the device,
panel revision and RAM variant; storage layout does not require another image.

## Choose the image

New images are named `ghazal-<version>-<device>-<ddr3|ddr4>.img.gz`.
Use the latest beta release for the paths and filenames in this guide.
The catalog in [devices.toml](../config/h700/devices.toml) is authoritative.

| Model / panel | Device name in the filename |
|---|---|
| RG28XX | `rg28xx` |
| RG34XX | `rg34xx` |
| RG34XX SP / v2 panel | `rg34xx-sp` / `rg34xx-sp-v2` |
| RG35XX 2024 / rev6 panel | `rg35xx-2024` / `rg35xx-2024-rev6` |
| RG35XX Plus / rev6 panel | `rg35xx-plus` / `rg35xx-plus-rev6` |
| RG35XX H / rev6 panel | `rg35xx-h` / `rg35xx-h-rev6` |
| RG35XX SP / v2 panel | `rg35xx-sp` / `rg35xx-sp-v2` |
| RG35XX Pro | `rg35xx-pro` |
| RG40XX H / v2 panel | `rg40xx-h` / `rg40xx-h-v2` |
| RG40XX V / v2 panel | `rg40xx-v` / `rg40xx-v-v2` |
| RG CubeXX | `rgcubexx` |
| RG SP | `rg-sp` |

The original RG35XX from 2022 is not H700 and is not supported. Other similarly
named Anbernic devices with different processors must not use these images.
The DDR name selects the bootloader's RAM initialization, not the amount of RAM.
Match your board's RAM revision. If unknown, try DDR4 on a spare card, then
DDR3 if it does not boot. If the display shows stripes, try the alternate panel
revision for the same model. The selected device tree is already installed;
no manual DTB copying is needed for a correctly chosen image.

## Before writing a card

1. Back up existing ROMs, BIOS files, saves, states, collections and history.
2. Download the image and `SHA256SUMS`. Verify its SHA-256 against that file.
   On Linux/macOS use `shasum -a 256 IMAGE.img.gz`; on PowerShell use
   `Get-FileHash IMAGE.img.gz -Algorithm SHA256`.
3. Use an SD card of at least 4 GB; 16 GB or larger leaves useful game space.
4. Decompress the `.gz` if your image writer needs a raw `.img`, then write
   the **whole image to the whole card**. This erases that card. Select the card
   by capacity and name before writing. Do not write an image to a partition.

## Optional Wi-Fi and SSH

After writing, mount `GHAZALBOOT`. Copy `provision.toml.example` to
`provision.toml`, and edit it before first boot:

```toml
ssh_authorized_keys = ["ssh-ed25519 YOUR_PUBLIC_KEY your-computer"]
data_card = "auto" # "tf1" for one card; "tf2" to require the games card

[wifi]
ssid = "Your network name"
password = "Your network password"
```

Use a complete public key from your computer's `.pub` file, never its private
key. Omit `[wifi]` or the key list if not needed. The image contains no personal
credentials and has no default SSH password. On boot these settings are stored
on the persistent system-state filesystem and the provisioning file is removed from the boot volume.
You can create a new `provision.toml` on a later boot to replace these settings.
Invalid input is preserved as `provision.rejected-*.toml`; `provision.error.txt`
on the boot partition explains the error, and the device still boots offline.
UTF-8 with a BOM and CRLF line endings are accepted. Deletion is not secure
erasure of FAT storage; keep the edited file and card private. RG28XX and
RG35XX 2024 have no built-in Wi-Fi; use card transfers or a compatible USB
Wi-Fi adapter instead.

## One SD card

1. Safely eject the written card and insert it into **TF1**. Leave TF2 empty.
2. Boot once and wait for Main Menu. First boot grows and creates the reserved
   `GHAZALDATA` exFAT partition in the remaining space. Let preparation finish.
3. Choose **Shutdown**, wait for the device to turn off, and move the card to
   the computer. Mount `GHAZALDATA` and copy games into `ROMS/<system>/` and
   BIOS files into `BIOS/`. Do not put games on the boot partition.
4. Safely eject, return the card to TF1 and boot. Explore Content lists systems
   with games. BIOS-dependent systems remain hidden until their BIOS is present.

First boot must happen **before** copying files into the new data partition.
Factory preparation only handles the reserved TF1 partition. It refuses to
claim an unknown filesystem. A durable formatting transaction permits retry
after power loss during the initial format, before data has ever been mounted.
Recognized, healthy data is preserved on retry. Once preparation finishes,
the factory marker is removed and subsequent boots never format the volume.

## Two SD cards

1. Write the OS image to the TF1 card as above. Do not write it to the games card.
2. Back up the TF2 card before preparing it. Use one data partition formatted
   as exFAT (recommended for computer access), FAT32 or ext4. Existing compatible
   cards can be used without reformatting. The first partition is selected.
3. Copy `ROMS/`, `BIOS/`, `SAVES/` and `.ghazal/` as needed to the **root** of TF2.
   Empty cards get the required directories on first boot.
4. With the handheld off, insert OS into TF1 and data into TF2, then boot.
5. TF2 takes priority for games, saves, history, thumbnails and Syncthing state.
   Its filesystem is never formatted or resized by ghazal. TF1 also gets its own
   data partition so it remains usable when TF2 is removed.

Set `data_card = "tf2"` in provision.toml if you want a missing TF2 card to be an
error rather than using TF1. Set `"tf1"` to always use TF1 and skip automatic
card detection. Default `"auto"` allows a bounded two-second detection window.
Information → Data card shows which volume is active. Dirty exFAT volumes get
a filesystem check before mounting; failures require manual repair.

Insert or remove cards while powered off. An invalid or damaged TF2 partition
causes a visible boot/recovery failure rather than silently placing saves on
TF1. Remove TF2 to use the TF1 data volume while repairing the second card.
Changing cards does not automatically migrate files; copy your data explicitly.

## Copy files over the network

Information shows the LAN address. Connect using the provisioned SSH key:

```sh
ssh root@DEVICE_IP
scp -r ROMS BIOS root@DEVICE_IP:/mnt/games/
```

The beta enables OpenSSH's internal SFTP subsystem. File managers with SFTP
support can open `sftp://root@DEVICE_IP/mnt/games/` using the same key.
Use Applications → Rescan library after transferring games while the menu runs.

## Updating and troubleshooting

Keep a backup before testing a new beta. Reflashing TF1 erases **all** of TF1,
including its games in a one-card setup. A two-card setup preserves TF2 when
only TF1 is reflashed; re-provision Wi-Fi/SSH on the new OS card.

A/B installations try the previous OS slot after three unconfirmed boots; see
[update recovery](updates.md#layout-and-recovery). A kernel hang before the
userspace watchdog starts can require Reset. If neither slot boots, back up
the data partition from a computer and reflash TF1.

Logs persist in `/var/log/ghazal/`; include the image filename, board/panel
revision and `/etc/ghazal/image.json` in a bug report. Native suspend remains
disabled. Use Shutdown before removing a card.

See [storage layout](storage-layout.md) and [beta limitations](beta-release.md).
