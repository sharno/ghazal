# Sleep

Configuration → Sleep chooses whether a short Power press puts the handheld to
sleep, and how long it may stay asleep: Off, then 1, 2, 3, 6 or 12 hours. Left
and Right cycle through them. When the limit passes, the SoC RTC's alarm
(`[rtc].wakealarm` in device.toml) wakes the handheld and it powers off
cleanly. A running game is stopped as at any shutdown, which writes its
battery save and RetroArch's auto save state.

Sleep starts Off on the RG40XX-V profile (`[power].sleep = "off"` in
system.toml): native wake has never been shown to work on the H700, and a
failed resume freezes the handheld until Reset. While it is Off, a short Power
press does nothing, holding Power shuts down, the Start menu has no Sleep entry
and `ghazal ctl suspend` is refused. Idle never sleeps (`sleep_after = 0`);
30 minutes without input power the handheld off, as before.

A board whose profile sets `[power].suspend = "disabled"` cannot sleep at all
and does not list Configuration → Sleep. The daemon keeps the player's choice
in `settings.toml`; until there is one, `[power].sleep` applies.

Over SSH:

```sh
ghazal ctl status            # ... sleep=off
ghazal ctl sleep 2h          # or off, 1h, 3h, 6h, 12h, next, prev
ghazal ctl suspend
```

`/var/log/ghazal/daemon.log` records each sleep (`sleeping; powering off if
still asleep after …`, `awake after …`) and `/var/lib/ghazal/suspend-checkpoint`
the last phase reached (synchronizing, selecting mode, arming the wake count,
entering the kernel, resumed).

## Hardware test

Run it with someone at the handheld, from slot B with slot A as the fallback,
the battery above 50 % and an SSH session open. Stop at the first failure: a hang
needs Reset, then read the checkpoint file and `kernel-log.log`; deeper
diagnosis uses the debug kernel in [kernel debugging](kernel-debugging.md).

1. With Sleep Off: a short Power press leaves the handheld awake, holding Power
   shuts down, and the Start menu has no Sleep.
2. Before any sleep: `cat /sys/power/mem_sleep` lists `deep`,
   `/sys/class/rtc/rtc1/device/power/wakeup` reads `enabled`, and
   `/proc/interrupts` has the RTC's interrupt.
3. RTC wake on its own, from SSH: `echo 0 > /sys/class/rtc/rtc1/wakealarm;
   echo +30 > /sys/class/rtc/rtc1/wakealarm; echo deep > /sys/power/mem_sleep;
   echo mem > /sys/power/state`. The shell should come back after about 30 s.
   Check that `date` moved on by the time asleep: the limit is measured on the
   wall clock, so a clock that stands still during sleep never powers off.
4. Power-button wake: `ghazal ctl sleep 1h`, a short Power press in the menu,
   wait 10 s, press Power. Check the panel and brightness, buttons and sticks,
   sound from the speaker and headphones, Wi-Fi and SSH, the battery reading
   and the stick lights. Repeat ten times.
5. The same from a RetroArch game, PPSSPP, standalone Mupen64Plus and a port:
   the game carries on with sound and steady frame pacing.
6. The limit: sleep in a game with `sleep 1h` and leave it. About an hour later
   it should wake and power off on its own; the game's `.srm` and `.state.auto`
   are written at that time and `daemon.log` says `powering off`.
7. Drain: battery capacity before and after a one-hour sleep, and overnight
   with `12h`. Compare with the power-off drain.

Sleep should stay Off in the shipped profile until all of these pass.
