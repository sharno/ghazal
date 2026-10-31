# Moonlight game streaming

Applications → **Moonlight game streaming** streams a PC game from a
[Sunshine](https://github.com/LizardByte/Sunshine) or GeForce Experience host
to the handheld. The picture is H.264 decoded in software on the four
Cortex-A53 cores; the pad, audio and display are the handheld's own.

Status: the whole flow is in, but it has not run against a real host yet. Parts
that no one has seen working are marked *unverified* below.

## Using it

1. Open Applications → Moonlight game streaming. With no host known yet it
   starts looking for one on the network; otherwise choose **Find hosts on the
   network** (mDNS, about three seconds). A host that is not found, or a host
   outside the LAN, is added with **Add host by IP address**: Left/Right pick
   an octet, Up/Down change it (L1/R1 by ten), A adds it. The editor starts
   from the handheld's own address.
2. Open the host and choose **Pair**. A four-digit PIN appears. Enter it on the
   host (Sunshine: Web UI → PIN, with any device name; GeForce Experience shows
   its own dialog) within three minutes. B cancels.
3. **Games and apps** lists the host's applications (Sunshine's Desktop, Steam
   Big Picture and whatever is configured). A starts the stream. If the host
   already runs that app the stream resumes it.
4. **Close the running app on the host**, at the end of the list, ends it.
5. Leave the stream with Menu + Start (the daemon's usual exit, which ends
   Moonlight cleanly) or Start + Select + L1 + R1 (Moonlight's own combination).
   Closing the stream from the host also returns to the menu; a stream cut by
   the network says why.

**Stream settings** (at the end of the host list; Left/Right change a value and
it is saved at once):

| Setting | Values | Default |
|---|---|---|
| Resolution | 640x360, 960x540, 1280x720, 640x480 | 640x360 |
| Frame rate | 30, 60 | 60 |
| Bitrate | Auto, 2, 5, 10, 15, 20 Mbps | 5 Mbps |
| Close the app when the stream ends | On, Off | Off |

The panel is 640x480. A stream of another size is scaled to fit with black
bars, so 16:9 sizes keep their shape. 640x360 is the default because it is the
cheapest to decode and loses nothing the panel could show; larger sizes are
downscaled with a linear filter, which looks smoother but costs decoder time.
`Auto` bitrate is Moonlight's own table (2 Mbps at 640x360 and 60 fps).

In a stream the pad is an Xbox-style controller with the buttons named as on
the device (A, B, X, Y by label), Select as Back, Start as Start and Menu as
the Guide button. The volume and brightness hotkeys work as everywhere.
Rumble is forwarded if the host asks for it. There is no keyboard or mouse
input; the Desktop app is therefore of little use without a Bluetooth or USB
mouse (SDL picks up any evdev device).

## How it is built

| Part | Where |
|---|---|
| Screens, state machine, hosts and settings store | `src/frontend/moonlight/` |
| `moonlight` (Moonlight Embedded 2.7.1, SDL platform) | `nix/runtime/moonlight.nix` and three patches |
| FFmpeg reduced to the H.264 decoder (libavcodec + libavutil, 2.3 MB) | `nix/runtime/ffmpeg.nix` |
| Pad mapping for SDL | `config/rg40xxv-rocknix/gamecontrollerdb.txt` |
| Paths and environment | `[moonlight]` in `frontend.toml` |
| Decode benchmark (not in the image) | `tools/decode-bench`, `nix build .#decode-bench` |
| Stand-in for a paired host's app list (not in the image) | `tools/fake-host/fake_host.py` |

The menu starts nothing at boot: the screens exist only while the entry is open,
so boot time is unchanged. Everything that talks to a host runs as a job on its
own thread, so the menu keeps responding, and dropping a screen (Back) kills its
job. `moonlight list`, `pair`, `unpair` and `quit` run with a time limit; the
stream itself runs through the same display and gamepad handover as an
emulator, at the `performance` CPU governor, so Menu + Start, idle-poweroff
inhibition and the log pump all apply. A failed stream shows Moonlight's
message (`Can't connect to server`, `You must pair with the PC first`, a cut
connection) as the toast, and the full output is in `/var/log/ghazal/moonlight.log`.

State lives on the data card in `/mnt/games/.ghazal/moonlight/`:

- `keys/` is Moonlight's client certificate, key and ID. Keeping them there is
  what makes pairings survive an OS reflash; delete the folder to forget this
  device (and unpair it on each host).
- `moonlight.toml` holds the hosts (name, address, port, whether paired) and the
  settings. A hostname that mDNS cannot give can be typed into this file over SSH
  (`[[host]]` with `name`, `address`); the file is rewritten by the menu, so
  edit it with Moonlight closed. An unreadable file is renamed
  `moonlight.toml.damaged` and replaced by defaults.

Patches to Moonlight Embedded (`nix/runtime/moonlight-*.patch`):

- `sdl-fit-panel`: its SDL window asked for a mode switch to the stream's size,
  which a fixed panel cannot do. It now goes fullscreen at the display's own
  mode and the renderer scales and letterboxes the stream.
- `connect-timeout`: libcurl's default connect timeout is minutes, so an
  offline host left the menu waiting. Five seconds, for connecting only
  (pairing legitimately waits for the PIN).
- `no-avahi`: ghazal does its own one-shot mDNS browse (`discover.rs`) and
  always passes the host's address, so Avahi and its D-Bus daemon are not needed.

Moonlight Embedded behaviours the menu works around, checked against the real
binary and a stand-in host (`tools/fake-host`, below):

- `list` prints the apps in the reverse of the host's order; the menu reverses
  them back. `pair`, `unpair` and `list` can fail with exit status 0 (after
  `Failed to pair to server: ...` or `Can't get app list`), so the menu reads
  the messages, not the status, and "Succesfully paired" is spelled that way.
- Its own PIN comes from an unseeded `random()` and is the same every run, so
  the menu generates the PIN and passes `-pin`.
- The first run generates the client's RSA key ("Generating certificate...done");
  an A53 takes a few seconds, which is why helpers get 25 s.
- With the connect-timeout patch an unreachable host fails in five seconds with
  `Can't connect to server <address>`; a refused connection fails at once.

## Decoding: what was measured and what is expected

Why software: the kernel has `MEDIA_SUPPORT` off (about 1 MB of image saved) and
the H700's Cedrus video engine has no device-tree node in this kernel or in
ROCKNIX's H700 tree. Hardware decoding would need Cedrus enabled and a
`/dev/video` decoder that Moonlight can use (FFmpeg's V4L2 request hwaccel plus
a DRM-PRIME output path); neither exists in Moonlight Embedded's SDL platform,
so it is not a small change and it costs boot time and image size. ROCKNIX
streams with the same software path on this SoC.

`decode-bench` decodes an Annex B stream the way Moonlight does (low delay, slice
threads, one access unit per packet) and prints the rate and frame times. Streams
shaped like Sunshine's (High profile, CABAC, no B-frames, four slices):

```sh
ffmpeg -f lavfi -i "mandelbrot=size=640x360:rate=60" -t 12 -pix_fmt yuv420p \
  -c:v libx264 -profile:v high -preset veryfast -tune zerolatency -bf 0 \
  -x264-params "slices=4:keyint=120:nal-hrd=none:bitrate=5000:vbv-maxrate=5000:vbv-bufsize=83" \
  -f h264 360p60.h264
```

The handheld's cores are Cortex-A53 r0p4 with NEON and the AES/PMULL/SHA
extensions, so FFmpeg's NEON decoding and Moonlight's encrypted streams use
hardware instructions. These numbers were measured on an Apple-silicon core in
the aarch64 Docker container used for builds, which is several times faster
than an A53, so only the relative costs mean something (one decoder thread;
four slice threads gained 15 to 50%):

| Stream | Bitrate | Frames per second | Time per frame |
|---|---|---|---|
| 640x360, 60 fps | 5 Mbps | 673 | 1.5 ms |
| 640x480, 60 fps | 8 Mbps | 454 | 2.2 ms |
| 1280x720, 30 fps | 10 Mbps | 175 | 5.7 ms |
| 1280x720, 60 fps | 15 Mbps | 238 | 4.2 ms |

720p60 costs about 2.8 times what the default 640x360 does. Scaling the
container's numbers to the A53 by the usual several-fold gap leaves 640x360 at
60 fps with headroom and 1280x720 at 60 fps on the edge, but that is an
estimate, not a measurement: **run `decode-bench` on the handheld** (below) and
set the defaults from it. The frame-time distribution matters more than the
average, since a frame over 16.7 ms is a dropped frame at 60 fps.

## Checking it on the handheld

Build the benchmark and copy it with a stream (the runtime's libraries are
already on the device once the image has Moonlight; the binary is linked to them
by store path):

```sh
nix build .#packages.aarch64-linux.decode-bench     # in an aarch64 Linux container
cat result/bin/decode-bench | ssh root@DEVICE 'cat > /tmp/decode-bench && chmod +x /tmp/decode-bench'
cat 360p60.h264 | ssh root@DEVICE 'cat > /tmp/360p60.h264'
ssh root@DEVICE '/tmp/decode-bench /tmp/360p60.h264 4 3'
```

Without a host, the menu side can be exercised up to the point of a host:
Applications → Moonlight → Find hosts (the search reports "No hosts found"), Add
host by IP address (any address), Games and apps (fails with
`Can't connect to server`, within about five seconds), Pair (shows the PIN and
waits; B cancels), and Stream settings.

`tools/fake-host/fake_host.py` stands in for a host that is already paired: it
answers `serverinfo` and `applist` on Sunshine's ports 47989 and 47984. Run it on
a computer on the same network (commands in its header), add that computer's
address as a host, and Games and apps lists Desktop, Steam Big Picture and
Half-Life 2: Episode One, in that order. Started without `--paired` it makes the
menu say "Not paired with ... yet: choose Pair". It cannot pair or stream.

With a Sunshine host: pair, list, stream at 640x360@60, then 1280x720@60, and
read `/var/log/ghazal/moonlight.log`. Moonlight's `-debug` output is not
enabled; add `-verbose` to `cli::stream` for a session when diagnosing.

## Not verified, not done

- No stream has run: the SDL KMSDRM window and GLES YUV texture on the Mali
  (the stream is rendered with SDL's GLES2 renderer, the same stack PPSSPP
  uses), audio through ALSA's `default` PCM, the pad mapping, and the letterboxed
  scaling are all untested on the device.
- mDNS answers from Sunshine on Windows and Linux: the browse sends a
  unicast-response query from an ephemeral port and listens for the reply. A
  host that answers only by multicast is not found; add it by address.
- The audio buffer is Moonlight's 4096 samples (about 85 ms at 48 kHz); lowering
  it is the first thing to try if sound lags the picture, once measured.
- Wi-Fi power saving is on (`iw dev wlan0 get power_save`) and can add latency
  to a stream's first packets after a pause; compare a stream with
  `iw dev wlan0 set power_save off` before changing the default. The tested
  RG40XX-V v2 links at 433 Mbps on 5 GHz (VHT80, one stream), so bandwidth is
  not the limit for the bitrates offered here.
- HEVC and AV1 are not built; software decoding of them is slower and the host is
  asked for H.264 only.
- Hostnames cannot be typed in the menu, and Moonlight Embedded offers no
  mouse/keyboard path here, no HDR and no surround sound.
- RG28XX (left-rotated panel) has no Moonlight entry: the image builder drops
  `[moonlight]` for it, as it does PPSSPP's standalone command, because SDL's
  output cannot rotate.
