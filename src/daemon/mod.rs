//! `oxmux daemon`: replaces muOS's muhotkey + mux/hotkey.sh, mubattery, lowpower.sh,
//! idle.sh and the brightness/volume scripts. Runs for the whole session, including while
//! an emulator is in the foreground, and never grabs input.

pub mod ctl;

use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use evdev::{Device, EventType};
use serde::{Deserialize, Serialize};

use crate::device::DeviceConfig;
use crate::hw::{self, battery, Hardware};
use crate::system::{Action, PowerMethod, SystemConfig};

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    brightness: u8,
    volume: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self { brightness: 70, volume: 60 }
    }
}

pub struct Daemon<'a> {
    dev: &'a DeviceConfig,
    sys: &'a SystemConfig,
    hw: Hardware,
    settings: Settings,
    inputs: Vec<(std::path::PathBuf, Device)>,
    inputs_scanned: Instant,
    listener: UnixListener,
    held: HashSet<String>,
    /// Buttons whose release shouldn't trigger anything (used in a combo).
    consumed: HashSet<String>,
    power_down_at: Option<Instant>,
    last_input: Instant,
    dimmed: bool,
    battery: Option<battery::Status>,
    battery_checked: Instant,
    low_led: bool,
    /// Ignore the power key right after resume (the wake press/release).
    ignore_power_until: Instant,
    muted_from: Option<u8>,
}

pub fn run(dev: &DeviceConfig, sys: &SystemConfig) -> Result<()> {
    let cfg = &sys.daemon;
    let settings: Settings = std::fs::read_to_string(&cfg.state).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();

    if let Some(dir) = cfg.socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&cfg.socket);
    let listener = UnixListener::bind(&cfg.socket).with_context(|| format!("binding {}", cfg.socket.display()))?;
    listener.set_nonblocking(true)?;

    let mut d = Daemon {
        dev,
        sys,
        hw: Hardware::open(dev),
        settings,
        inputs: Vec::new(),
        inputs_scanned: Instant::now(),
        listener,
        held: HashSet::new(),
        consumed: HashSet::new(),
        power_down_at: None,
        last_input: Instant::now(),
        dimmed: false,
        battery: None,
        battery_checked: Instant::now() - Duration::from_secs(3600),
        low_led: false,
        ignore_power_until: Instant::now(),
        muted_from: None,
    };
    d.rescan_inputs();
    d.apply_settings();
    hw::set_led(&d.hw.leds.power, true);
    eprintln!("oxmux-daemon: {} ({} input devices)", dev.name, d.inputs.len());
    d.run()
}

const INPUT_RESCAN: Duration = Duration::from_secs(5);

impl Daemon<'_> {
    fn run(&mut self) -> Result<()> {
        loop {
            let now = Instant::now();
            self.check_battery(now);
            self.check_idle(now);
            self.check_power_hold(now);
            if now.duration_since(self.inputs_scanned) >= INPUT_RESCAN {
                self.rescan_inputs();
            }

            let mut fds: Vec<libc::pollfd> = self
                .inputs
                .iter()
                .map(|(_, d)| d.as_raw_fd())
                .chain([self.listener.as_raw_fd()])
                .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
                .collect();
            let timeout = self.next_wake(now).saturating_duration_since(now);
            // SAFETY: valid pollfd array.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout.as_millis().min(60_000) as i32) };
            if n <= 0 {
                continue;
            }
            let (input_fds, sock) = fds.split_at(fds.len() - 1);
            let ready: Vec<usize> = input_fds.iter().enumerate().filter(|(_, f)| f.revents != 0).map(|(i, _)| i).collect();
            for i in ready {
                let events: Vec<_> = match self.inputs[i].1.fetch_events() {
                    Ok(ev) => ev.filter(|e| e.event_type() == EventType::KEY).map(|e| (e.code(), e.value())).collect(),
                    Err(_) => continue,
                };
                for (code, value) in events {
                    self.on_key(code, value);
                }
            }
            if sock[0].revents != 0 {
                self.serve_ctl();
            }
        }
    }

    /// Opens every evdev device with keys (gamepad, power button, volume keys) that we
    /// aren't reading yet, and drops ones that went away. Nothing is grabbed.
    fn rescan_inputs(&mut self) {
        self.inputs_scanned = Instant::now();
        self.inputs.retain(|(path, _)| path.exists());
        for (path, d) in evdev::enumerate() {
            if self.inputs.iter().any(|(p, _)| *p == path) || !d.supported_events().contains(EventType::KEY) {
                continue;
            }
            let fd = d.as_raw_fd();
            // SAFETY: fcntl on an fd we own.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK) } < 0 {
                continue;
            }
            eprintln!("oxmux-daemon: input {} {:?}", path.display(), d.name().unwrap_or("?"));
            self.inputs.push((path, d));
        }
    }

    fn next_wake(&self, now: Instant) -> Instant {
        let idle = &self.sys.daemon.idle;
        let mut t = self.battery_checked + Duration::from_secs(self.sys.daemon.battery.poll_secs.max(5));
        t = t.min(self.inputs_scanned + INPUT_RESCAN);
        if idle.dim_after > 0 && !self.dimmed {
            t = t.min(self.last_input + Duration::from_secs(idle.dim_after));
        }
        if idle.sleep_after > 0 {
            t = t.min(self.last_input + Duration::from_secs(idle.sleep_after));
        }
        if let Some(down) = self.power_down_at {
            t = t.min(down + Duration::from_millis(self.sys.daemon.power_hold_ms));
        }
        t.max(now)
    }

    fn on_key(&mut self, code: u16, value: i32) {
        let now = Instant::now();
        self.last_input = now;
        if self.dimmed {
            self.dimmed = false;
            self.apply_brightness(self.settings.brightness);
        }
        let Some(name) = self.dev.button_name(code).map(str::to_string) else { return };
        match value {
            1 => {
                self.held.insert(name.clone());
                if name == "power" && now >= self.ignore_power_until {
                    self.power_down_at = Some(now);
                }
                // The most specific combo wins: Menu+Vol+ beats a plain Vol+ hotkey.
                let hit = self
                    .sys
                    .daemon
                    .hotkeys
                    .iter()
                    .filter(|h| h.keys.last() == Some(&name) && h.keys.iter().all(|k| self.held.contains(k)))
                    .max_by_key(|h| h.keys.len());
                if let Some(hotkey) = hit.cloned() {
                    self.consumed.extend(hotkey.keys.iter().cloned());
                    if hotkey.keys.iter().any(|k| k == "power") {
                        self.power_down_at = None;
                    }
                    self.act(&hotkey.action);
                }
            }
            // Autorepeat (2): repeat brightness/volume hotkeys while held.
            2 => {
                let hit = self
                    .sys
                    .daemon
                    .hotkeys
                    .iter()
                    .filter(|h| h.keys.last() == Some(&name) && h.keys.iter().all(|k| self.held.contains(k)))
                    .max_by_key(|h| h.keys.len())
                    .filter(|h| {
                        matches!(h.action, Action::BrightnessUp | Action::BrightnessDown | Action::VolumeUp | Action::VolumeDown)
                    });
                if let Some(h) = hit {
                    let action = h.action.clone();
                    self.act(&action);
                }
            }
            0 => {
                self.held.remove(&name);
                let consumed = self.consumed.remove(&name);
                if name == "power" {
                    if let Some(down) = self.power_down_at.take() {
                        let held = now.duration_since(down) < Duration::from_millis(self.sys.daemon.power_hold_ms);
                        if held && !consumed {
                            self.act(&Action::Suspend);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn check_power_hold(&mut self, now: Instant) {
        if let Some(down) = self.power_down_at {
            if now.duration_since(down) >= Duration::from_millis(self.sys.daemon.power_hold_ms) {
                self.power_down_at = None;
                self.act(&Action::Poweroff);
            }
        }
    }

    fn check_idle(&mut self, now: Instant) {
        let idle = &self.sys.daemon.idle;
        let quiet = now.duration_since(self.last_input);
        if idle.sleep_after > 0 && quiet >= Duration::from_secs(idle.sleep_after) {
            let charging = self.battery.is_some_and(|b| b.charging);
            if !(charging && idle.stay_awake_charging) {
                eprintln!("oxmux-daemon: idle for {quiet:?}, sleeping");
                self.act(&Action::Suspend);
            }
            self.last_input = Instant::now();
            return;
        }
        if idle.dim_after > 0 && !self.dimmed && quiet >= Duration::from_secs(idle.dim_after) {
            self.dimmed = true;
            self.apply_brightness(idle.dim_to.min(self.settings.brightness));
        }
    }

    fn check_battery(&mut self, now: Instant) {
        let policy = &self.sys.daemon.battery;
        if now.duration_since(self.battery_checked) < Duration::from_secs(policy.poll_secs.max(5)) {
            return;
        }
        self.battery_checked = now;
        self.battery = battery::read(self.hw.battery.as_ref());
        let Some(b) = self.battery else { return };
        let low = !b.charging && b.percent <= policy.low;
        if low != self.low_led {
            self.low_led = low;
            hw::set_led(&self.hw.leds.low_battery, low);
        }
        if !b.charging && b.percent <= policy.critical {
            eprintln!("oxmux-daemon: battery at {}%, powering off", b.percent);
            hw::rumble(&self.hw.rumble, Duration::from_millis(300));
            self.act(&Action::Poweroff);
        }
    }

    pub fn act(&mut self, action: &Action) {
        let step = |v: u8, s: u8, up: bool| if up { v.saturating_add(s).min(100) } else { v.saturating_sub(s) };
        let d = &self.sys.daemon;
        match action {
            Action::BrightnessUp | Action::BrightnessDown => {
                // Never go fully dark from a hotkey; 0 is reserved for sleep.
                let b = step(self.settings.brightness, d.brightness_step, *action == Action::BrightnessUp).max(5);
                self.settings.brightness = b;
                self.apply_brightness(b);
                self.save();
            }
            Action::VolumeUp | Action::VolumeDown => {
                self.settings.volume = step(self.settings.volume, d.volume_step, *action == Action::VolumeUp);
                self.muted_from = None;
                self.apply_volume(self.settings.volume);
                self.save();
            }
            Action::Mute => match self.muted_from.take() {
                Some(v) => self.apply_volume(v),
                None => {
                    self.muted_from = Some(self.settings.volume);
                    self.apply_volume(0);
                }
            },
            Action::Suspend => self.suspend(),
            Action::Poweroff => self.power(&self.sys.power.poweroff.clone(), libc::SIGUSR2),
            Action::Reboot => self.power(&self.sys.power.reboot.clone(), libc::SIGTERM),
            Action::Exec(argv) => spawn_detached(argv),
        }
    }

    /// Mirrors muOS's suspend.sh: LED off, mute, backlight off, panel blank, hooks, then
    /// sleep; an RTC alarm wakes us to power off after a long sleep.
    fn suspend(&mut self) {
        let power = &self.sys.power;
        hw::set_led(&self.hw.leds.power, false);
        self.apply_volume(0);
        self.apply_brightness(0);
        blank(self.dev, true);
        run_hooks(&power.before_suspend);

        let alarm = self.dev.rtc.wakealarm.as_ref().filter(|_| power.sleep_poweroff_after > 0);
        if let Some(path) = alarm {
            // The kernel refuses a new alarm while one is set.
            let _ = crate::sys::write_file(path, "0");
            let _ = crate::sys::write_file(path, &format!("+{}", power.sleep_poweroff_after));
        }
        let slept_from = std::time::SystemTime::now();
        if let Err(e) = hw::suspend(power.mem_sleep.as_deref()) {
            eprintln!("oxmux-daemon: {e:#}");
        }
        // Back from sleep. Wall-clock time keeps counting while suspended.
        let slept = slept_from.elapsed().unwrap_or_default();
        if let Some(path) = alarm {
            let _ = crate::sys::write_file(path, "0");
            if slept + Duration::from_secs(2) >= Duration::from_secs(power.sleep_poweroff_after) {
                eprintln!("oxmux-daemon: asleep for {slept:?}, powering off");
                self.act(&Action::Poweroff);
                return;
            }
        }
        run_hooks(&self.sys.power.after_resume);
        blank(self.dev, false);
        self.apply_volume(if self.muted_from.is_some() { 0 } else { self.settings.volume });
        self.ignore_power_until = Instant::now() + Duration::from_millis(800);
        self.power_down_at = None;
        self.held.clear();
        self.consumed.clear();
        self.last_input = Instant::now();
        self.dimmed = false;
        self.apply_brightness(self.settings.brightness);
        hw::set_led(&self.hw.leds.power, true);
        self.battery_checked = Instant::now() - Duration::from_secs(3600);
    }

    fn power(&mut self, method: &PowerMethod, signal: i32) {
        self.save();
        hw::set_led(&self.hw.leds.power, false);
        crate::sys::sync();
        match method {
            PowerMethod::SignalInit => crate::sys::kill(1, signal),
            // Detached: a shutdown script stops the supervisor, which signals our whole
            // process group; the script must not be in it.
            PowerMethod::Exec(argv) => spawn_detached(argv),
        }
    }

    fn apply_settings(&mut self) {
        self.apply_brightness(self.settings.brightness);
        self.apply_volume(self.settings.volume);
    }

    fn apply_brightness(&mut self, percent: u8) {
        if let Some(b) = &mut self.hw.backlight {
            if let Err(e) = b.set_percent(percent) {
                eprintln!("oxmux-daemon: brightness: {e:#}");
            }
        }
    }

    fn apply_volume(&self, percent: u8) {
        if let Some(v) = &self.hw.volume {
            if let Err(e) = v.set_percent(percent) {
                eprintln!("oxmux-daemon: volume: {e:#}");
            }
        }
    }

    fn save(&self) {
        let path = &self.sys.daemon.state;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        let text = toml::to_string(&self.settings).unwrap_or_default();
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
            eprintln!("oxmux-daemon: couldn't save {}", path.display());
        }
    }

    fn status(&self) -> ctl::Status {
        let battery = battery::read(self.hw.battery.as_ref());
        ctl::Status {
            brightness: self.settings.brightness,
            volume: if self.muted_from.is_some() { 0 } else { self.settings.volume },
            battery: battery.map(|b| b.percent),
            charging: battery.is_some_and(|b| b.charging),
        }
    }

    fn serve_ctl(&mut self) {
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            if let Err(e) = ctl::handle(self, stream) {
                eprintln!("oxmux-daemon: ctl: {e:#}");
            }
        }
    }

    fn set_brightness(&mut self, percent: u8) {
        self.settings.brightness = percent.clamp(5, 100);
        self.apply_brightness(self.settings.brightness);
        self.save();
    }

    fn set_volume(&mut self, percent: u8) {
        self.settings.volume = percent.min(100);
        self.muted_from = None;
        self.apply_volume(self.settings.volume);
        self.save();
    }
}

fn blank(dev: &DeviceConfig, off: bool) {
    if let Some(path) = &dev.display.blank {
        let _ = crate::sys::write_file(path, if off { "4" } else { "0" });
    }
}

fn run_hooks(hooks: &[Vec<String>]) {
    for argv in hooks {
        if let Some((prog, args)) = argv.split_first() {
            match std::process::Command::new(prog).args(args).status() {
                Ok(s) if !s.success() => eprintln!("oxmux-daemon: hook {argv:?} exited with {s}"),
                Err(e) => eprintln!("oxmux-daemon: hook {argv:?}: {e}"),
                _ => {}
            }
        }
    }
}

/// Starts a command in its own session, so it outlives the daemon and isn't hit by
/// signals sent to the daemon's process group. Not waited for; the supervisor (or PID 1)
/// reaps it.
fn spawn_detached(argv: &[String]) {
    use std::os::unix::process::CommandExt;
    let Some((prog, args)) = argv.split_first() else { return };
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).stdin(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        // Dropping the Child doesn't kill it; reap it in the background.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("oxmux-daemon: exec {prog}: {e}"),
    }
}
