//! `ghazal daemon`: replaces muOS's muhotkey + mux/hotkey.sh, mubattery, lowpower.sh,
//! idle.sh and the brightness/volume scripts. Runs for the whole session, including while
//! an emulator is in the foreground, and never grabs input.

pub mod ctl;

use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use evdev::{Device, EventType, InputEventKind, SwitchType};
use serde::{Deserialize, Serialize};

use crate::device::DeviceConfig;
use crate::hdmi::connector::{Connector, DRM_CLASS};
use crate::hdmi::{self, Connection, Outputs, Sound};
use crate::hw::{self, battery, lights, Hardware};
use crate::system::{Action, PowerMethod, SystemConfig};

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    brightness: u8,
    volume: u8,
    lights: lights::Preset,
}

impl Default for Settings {
    fn default() -> Self {
        Self { brightness: 70, volume: 60, lights: lights::Preset::Off }
    }
}

/// The stick-light controller is either unpowered, warming up after power-on
/// (it drops frames for ~100 ms), or showing an effect.
#[derive(Clone, Copy, PartialEq)]
enum LightsState {
    Unpowered,
    WarmingUp { until: Instant, effect: lights::Effect },
    Showing(lights::Effect),
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
    /// Last button press (or open SSH session): idle poweroff counts from here.
    last_input: Instant,
    /// Like `last_input`, but a running emulator also counts: dimming and
    /// sleep wait for this, so a game isn't dimmed during a cutscene.
    last_presence: Instant,
    dimmed: bool,
    battery: Option<battery::Status>,
    battery_checked: Instant,
    /// Consecutive critical readings; one bad sample must not power off.
    critical_samples: u8,
    /// The idle check scans /proc: at most once a second, not per input event.
    idle_checked: Instant,
    low_led: bool,
    /// Ignore the power key right after resume (the wake press/release).
    ignore_power_until: Instant,
    muted_from: Option<u8>,
    settings_dirty: Option<Instant>,
    lights: LightsState,
    /// The headphone jack's last reading; the speaker plays only when nothing else carries the sound.
    headphones: bool,
    /// Off while HDMI shows the picture and the frontend asked for a dark panel.
    panel_lit: bool,
    sound: Sound,
    /// While the panel is dark or HDMI carries sound, when the cable is next checked.
    hdmi_check: Option<Instant>,
}

/// A frontend that is busy in a game cannot notice the cable being pulled; the daemon can.
const HDMI_CHECK: Duration = Duration::from_secs(2);

pub fn run(dev: &DeviceConfig, sys: &SystemConfig) -> Result<()> {
    sys.validate()?;
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
        last_presence: Instant::now(),
        dimmed: false,
        battery: None,
        battery_checked: Instant::now() - Duration::from_secs(3600),
        critical_samples: 0,
        idle_checked: Instant::now() - Duration::from_secs(1),
        low_led: false,
        ignore_power_until: Instant::now(),
        muted_from: None,
        settings_dirty: None,
        lights: LightsState::Unpowered,
        headphones: false,
        panel_lit: true,
        sound: Sound::Speaker,
        hdmi_check: None,
    };
    // The speaker plays unless a jack reports headphones during the first scan.
    d.follow_jack(false);
    d.rescan_inputs();
    d.apply_settings();
    hw::set_led(&d.hw.leds.power, true);
    eprintln!("ghazal-daemon: {} ({} input devices)", dev.name, d.inputs.len());
    d.run()
}

const INPUT_RESCAN: Duration = Duration::from_secs(5);

impl Daemon<'_> {
    fn run(&mut self) -> Result<()> {
        loop {
            let now = Instant::now();
            self.check_battery(now);
            if now >= self.idle_checked + Duration::from_secs(1) {
                self.idle_checked = now;
                self.check_idle(now);
            }
            self.check_power_hold(now);
            self.check_hdmi(now);
            if self.settings_dirty.is_some_and(|at| now >= at + Duration::from_millis(500)) {
                self.save();
            }
            if now.duration_since(self.inputs_scanned) >= INPUT_RESCAN {
                self.rescan_inputs();
            }
            if let LightsState::WarmingUp { until, effect } = self.lights {
                if now >= until { self.show_lights(effect); }
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
            let mut removed = Vec::new();
            for i in ready {
                if input_fds[i].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    removed.push(i);
                    continue;
                }
                let events: Vec<_> = match self.inputs[i].1.fetch_events() {
                    Ok(ev) => ev.map(|e| (e.kind(), e.value())).collect(),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(_) => { removed.push(i); continue; },
                };
                for (kind, value) in events {
                    match kind {
                        InputEventKind::Key(key) => self.on_key(key.code(), value),
                        InputEventKind::Switch(SwitchType::SW_HEADPHONE_INSERT) => self.follow_jack(value != 0),
                        _ => {}
                    }
                }
            }
            if !removed.is_empty() { self.clear_keys(); }
            for index in removed.into_iter().rev() { self.inputs.remove(index); }
            if sock[0].revents != 0 {
                self.serve_ctl();
            }
        }
    }

    /// Opens every evdev device with keys (gamepad, power button, volume keys) or
    /// switches (headphone jack) that we aren't reading yet, and drops ones that
    /// went away. Nothing is grabbed.
    fn rescan_inputs(&mut self) {
        self.inputs_scanned = Instant::now();
        if self.inputs.iter().any(|(path, _)| !path.exists()) { self.clear_keys(); }
        self.inputs.retain(|(path, _)| path.exists());
        for (path, d) in evdev::enumerate() {
            let events = d.supported_events();
            if self.inputs.iter().any(|(p, _)| *p == path) || !(events.contains(EventType::KEY) || events.contains(EventType::SWITCH)) {
                continue;
            }
            if d.supported_switches().is_some_and(|switches| switches.contains(SwitchType::SW_HEADPHONE_INSERT)) {
                match d.get_switch_state() {
                    Ok(state) => self.follow_jack(state.contains(SwitchType::SW_HEADPHONE_INSERT)),
                    Err(error) => eprintln!("ghazal-daemon: reading headphone jack: {error}"),
                }
            }
            let fd = d.as_raw_fd();
            // SAFETY: fcntl on an fd we own.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK) } < 0 {
                continue;
            }
            eprintln!("ghazal-daemon: input {} {:?}", path.display(), d.name().unwrap_or("?"));
            self.inputs.push((path, d));
        }
    }

    /// The codec feeds the speaker amplifier and the jack from one output.
    fn follow_jack(&mut self, headphones: bool) {
        self.headphones = headphones;
        self.apply_speaker();
    }

    /// The speaker plays unless headphones or HDMI carry the sound.
    fn apply_speaker(&self) {
        if let Some(speaker) = &self.hw.speaker {
            if let Err(error) = speaker.set(!self.headphones && self.sound == Sound::Speaker) { eprintln!("ghazal-daemon: speaker switch: {error:#}"); }
        }
    }

    /// Applies what the frontend wants of the panel and the sound route, and answers with what
    /// was possible: HDMI sound needs its card, which is created on first use.
    fn set_outputs(&mut self, wanted: Outputs) -> Outputs {
        self.sound = match (wanted.sound, self.dev.audio.as_ref().and_then(|audio| audio.hdmi.as_ref())) {
            (Sound::Hdmi, Some(config)) => match hdmi::audio::enable(config) {
                Ok(()) => Sound::Hdmi,
                Err(error) => {
                    eprintln!("ghazal-daemon: HDMI sound unavailable, staying on the speaker: {error:#}");
                    Sound::Speaker
                }
            },
            _ => Sound::Speaker,
        };
        self.panel_lit = wanted.panel_lit;
        self.apply_speaker();
        self.apply_brightness(self.shown_brightness());
        self.hdmi_check = (!self.panel_lit || self.sound == Sound::Hdmi).then(|| Instant::now() + HDMI_CHECK);
        Outputs { panel_lit: self.panel_lit, sound: self.sound }
    }

    /// Safety net for a game that is running on HDMI: when the cable goes away
    /// the handheld's screen and speaker come back, so the device is never mute and dark.
    fn check_hdmi(&mut self, now: Instant) {
        let Some(at) = self.hdmi_check else { return };
        if now < at { return; }
        let connected = Connector::find_hdmi(std::path::Path::new(DRM_CLASS), None).is_some_and(|connector| connector.connection() == Connection::Connected);
        if connected {
            self.hdmi_check = Some(now + HDMI_CHECK);
            return;
        }
        eprintln!("ghazal-daemon: HDMI display gone; lighting the panel and using the speaker");
        self.hdmi_check = None;
        self.panel_lit = true;
        self.sound = Sound::Speaker;
        self.apply_speaker();
        self.apply_brightness(self.shown_brightness());
    }

    /// The level the backlight shows when lit: dimming still applies.
    fn shown_brightness(&self) -> u8 {
        if self.dimmed { self.sys.daemon.idle.dim_to.min(self.settings.brightness) } else { self.settings.brightness }
    }

    fn clear_keys(&mut self) {
        self.held.clear();
        self.consumed.clear();
        self.power_down_at = None;
    }

    fn next_wake(&self, now: Instant) -> Instant {
        let idle = &self.sys.daemon.idle;
        let mut t = self.battery_checked + Duration::from_secs(self.sys.daemon.battery.poll_secs.max(5));
        t = t.min(self.inputs_scanned + INPUT_RESCAN);
        if let Some(at) = self.settings_dirty { t = t.min(at + Duration::from_millis(500)); }
        if let LightsState::WarmingUp { until, .. } = self.lights { t = t.min(until); }
        if let Some(at) = self.hdmi_check { t = t.min(at); }
        // check_idle runs at most once a second; an earlier wake would spin until then.
        let idle_due = |deadline: Instant| deadline.max(self.idle_checked + Duration::from_secs(1));
        if idle.dim_after > 0 && !self.dimmed {
            t = t.min(idle_due(self.last_presence + Duration::from_secs(idle.dim_after)));
        }
        if idle.sleep_after > 0 {
            t = t.min(idle_due(self.last_presence + Duration::from_secs(idle.sleep_after)));
        }
        if idle.poweroff_after > 0 {
            t = t.min(idle_due(self.last_input + Duration::from_secs(idle.poweroff_after)));
        }
        if let Some(down) = self.power_down_at {
            t = t.min(down + Duration::from_millis(self.sys.daemon.power_hold_ms));
        }
        t.max(now)
    }

    fn on_key(&mut self, code: u16, value: i32) {
        let now = Instant::now();
        self.last_input = now;
        self.last_presence = now;
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
        if crate::update::busy() { self.activity(); return; }
        let emulator = match crate::emulator::active() {
            Ok(active) => active,
            Err(error) => { eprintln!("ghazal-daemon: emulator activity: {error:#}"); return; },
        };
        let ssh = std::fs::read_dir("/proc").is_ok_and(|entries| entries.flatten().any(|entry| {
            entry.file_name().to_string_lossy().bytes().all(|byte| byte.is_ascii_digit())
                && std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|name| name.trim() == "sshd-session")
        }));
        if ssh {
            self.activity();
            return;
        }
        let idle = &self.sys.daemon.idle;
        // Without input, even a running game is abandoned; shutdown stops the
        // emulator cleanly, which writes its save RAM.
        let untouched = now.duration_since(self.last_input);
        if idle.poweroff_after > 0 && untouched >= Duration::from_secs(idle.poweroff_after) {
            eprintln!("ghazal-daemon: no input for {untouched:?}, powering off");
            self.act(&Action::Poweroff);
            return;
        }
        if emulator {
            self.last_presence = now;
            return;
        }
        let quiet = now.duration_since(self.last_presence);
        if idle.sleep_after > 0 && quiet >= Duration::from_secs(idle.sleep_after) {
            let charging = self.battery.is_some_and(|b| b.charging);
            if !(charging && idle.stay_awake_charging) {
                eprintln!("ghazal-daemon: idle for {quiet:?}, sleeping");
                self.act(&Action::Suspend);
            }
            self.last_input = Instant::now();
            self.last_presence = self.last_input;
            return;
        }
        if idle.dim_after > 0 && !self.dimmed && quiet >= Duration::from_secs(idle.dim_after) {
            self.dimmed = true;
            self.apply_brightness(idle.dim_to.min(self.settings.brightness));
        }
    }

    fn activity(&mut self) {
        self.last_input = Instant::now();
        self.last_presence = self.last_input;
        if self.dimmed {
            self.dimmed = false;
            self.apply_brightness(self.settings.brightness);
        }
    }

    fn check_battery(&mut self, now: Instant) {
        let policy = &self.sys.daemon.battery;
        if now.duration_since(self.battery_checked) < Duration::from_secs(policy.poll_secs.max(5)) {
            return;
        }
        self.battery_checked = now;
        let previous = std::mem::replace(&mut self.battery, battery::read(self.hw.battery.as_ref()));
        let Some(b) = self.battery else { return };
        // Evidence for intermittent charging: the log shows when power came and went.
        if previous.is_some_and(|p| p.charging != b.charging) {
            eprintln!("ghazal-daemon: {} at {}%", if b.charging { "charging" } else { "on battery" }, b.percent);
        }
        let low = !b.charging && b.percent <= policy.low;
        if low != self.low_led {
            self.low_led = low;
            hw::set_led(&self.hw.leds.low_battery, low);
            self.apply_lights();
        }
        self.critical_samples = if !b.charging && b.percent <= policy.critical { self.critical_samples.saturating_add(1) } else { 0 };
        if self.critical_samples >= 2 {
            eprintln!("ghazal-daemon: battery at {}%, powering off", b.percent);
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
                self.settings_dirty = Some(Instant::now());
            }
            Action::VolumeUp | Action::VolumeDown => {
                self.settings.volume = step(self.settings.volume, d.volume_step, *action == Action::VolumeUp);
                self.muted_from = None;
                self.apply_volume(self.settings.volume);
                self.settings_dirty = Some(Instant::now());
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
            // Stopping can wait 10 s for a hung emulator; hotkeys and the power
            // button must keep working meanwhile.
            Action::StopEmulator => {
                std::thread::spawn(|| if let Err(error) = crate::emulator::stop() { eprintln!("ghazal-daemon: stop emulator: {error:#}"); });
            }
            Action::Exec(argv) => spawn_detached(argv),
        }
    }

    /// Mirrors muOS's suspend.sh: LED off, mute, backlight off, panel blank, hooks, then
    /// sleep; an RTC alarm wakes us to power off after a long sleep.
    fn suspend(&mut self) {
        if crate::update::busy() { return; }
        if self.sys.power.suspend == crate::system::SuspendPolicy::Disabled {
            eprintln!("ghazal-daemon: suspend disabled until native wake is validated");
            return;
        }
        self.save();
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
        if let Err(e) = hw::suspend(power.mem_sleep.as_deref(), power.suspend_checkpoint.as_deref()) {
            eprintln!("ghazal-daemon: {e:#}");
        }
        // Back from sleep. Wall-clock time keeps counting while suspended.
        let slept = slept_from.elapsed().unwrap_or_default();
        if let Some(path) = alarm {
            let _ = crate::sys::write_file(path, "0");
            if slept + Duration::from_secs(2) >= Duration::from_secs(power.sleep_poweroff_after) {
                eprintln!("ghazal-daemon: asleep for {slept:?}, powering off");
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
        self.last_presence = self.last_input;
        self.dimmed = false;
        self.apply_brightness(self.settings.brightness);
        hw::set_led(&self.hw.leds.power, true);
        self.battery_checked = Instant::now() - Duration::from_secs(3600);
    }

    fn power(&mut self, method: &PowerMethod, signal: i32) {
        if crate::update::busy() { eprintln!("ghazal: update in progress; power action deferred"); return; }
        self.save();
        hw::set_led(&self.hw.leds.power, false);
        self.set_lights_power(false);
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
        self.apply_lights();
    }

    /// Low battery turns enabled rings red, as on muOS; Off cuts their power.
    fn apply_lights(&mut self) {
        if self.hw.lights.is_none() { return; }
        let wanted = self.settings.lights.effect().map(|effect| if self.low_led { lights::LOW_BATTERY } else { effect });
        match (wanted, self.lights) {
            (None, LightsState::Unpowered) => {}
            (None, _) => self.set_lights_power(false),
            (Some(effect), LightsState::Unpowered) => {
                self.set_lights_power(true);
                self.lights = LightsState::WarmingUp { until: Instant::now() + Duration::from_millis(150), effect };
            }
            (Some(effect), LightsState::WarmingUp { until, .. }) => self.lights = LightsState::WarmingUp { until, effect },
            (Some(effect), LightsState::Showing(shown)) if shown == effect => {}
            (Some(effect), LightsState::Showing(_)) => self.show_lights(effect),
        }
    }

    fn set_lights_power(&mut self, on: bool) {
        let Some(lights) = &self.hw.lights else { return };
        if let Err(error) = lights.power(on) { eprintln!("ghazal-daemon: stick lights: {error:#}"); }
        if !on { self.lights = LightsState::Unpowered; }
    }

    fn show_lights(&mut self, effect: lights::Effect) {
        let Some(lights) = &mut self.hw.lights else { return };
        if let Err(error) = lights.show(effect) {
            eprintln!("ghazal-daemon: stick lights: {error:#}");
            self.set_lights_power(false);
            return;
        }
        self.lights = LightsState::Showing(effect);
    }

    fn set_lights(&mut self, preset: lights::Preset) {
        self.settings.lights = preset;
        self.apply_lights();
        self.settings_dirty = Some(Instant::now());
    }

    fn apply_brightness(&mut self, percent: u8) {
        // A dark panel stays dark through dimming, hotkeys and wake-ups alike.
        let percent = if self.panel_lit { percent } else { 0 };
        if let Some(b) = &mut self.hw.backlight {
            if let Err(e) = b.set_percent(percent) {
                eprintln!("ghazal-daemon: brightness: {e:#}");
            }
        }
        self.publish_levels();
    }

    fn apply_volume(&self, percent: u8) {
        if let Some(v) = &self.hw.volume {
            if let Err(e) = v.set_percent(percent) {
                eprintln!("ghazal-daemon: volume: {e:#}");
            }
        }
        self.publish_levels();
    }

    fn publish_levels(&self) {
        let volume = if self.muted_from.is_some() { 0 } else { self.settings.volume };
        let result = crate::levels::Levels::new(self.settings.brightness, volume)
            .and_then(|levels| levels.publish(&self.sys.daemon.socket));
        if let Err(error) = result { eprintln!("ghazal-daemon: publishing levels: {error:#}"); }
    }

    fn save(&mut self) {
        if self.settings_dirty.is_none() { return; }
        let path = &self.sys.daemon.state;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let result = toml::to_string(&self.settings).map_err(anyhow::Error::from)
            .and_then(|text| crate::sys::write_atomic(path, text.as_bytes()));
        match result {
            Ok(()) => self.settings_dirty = None,
            Err(error) => {
                eprintln!("ghazal-daemon: couldn't save {}: {error}", path.display());
                self.settings_dirty = Some(Instant::now() + Duration::from_secs(5));
            }
        }
    }

    fn status(&self) -> ctl::Status {
        let battery = battery::read(self.hw.battery.as_ref());
        ctl::Status {
            brightness: self.settings.brightness,
            volume: if self.muted_from.is_some() { 0 } else { self.settings.volume },
            battery: battery.map(|b| b.percent),
            charging: battery.is_some_and(|b| b.charging),
            suspend: self.sys.power.suspend == crate::system::SuspendPolicy::Enabled,
            lights: self.hw.lights.as_ref().map(|_| self.settings.lights),
        }
    }

    fn serve_ctl(&mut self) {
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            if let Err(e) = ctl::handle(self, stream) {
                eprintln!("ghazal-daemon: ctl: {e:#}");
            }
        }
    }

    fn set_brightness(&mut self, percent: u8) {
        self.settings.brightness = percent.clamp(5, 100);
        self.apply_brightness(self.settings.brightness);
        self.settings_dirty = Some(Instant::now());
    }

    fn set_volume(&mut self, percent: u8) {
        self.settings.volume = percent.min(100);
        self.muted_from = None;
        self.apply_volume(self.settings.volume);
        self.settings_dirty = Some(Instant::now());
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
                Ok(s) if !s.success() => eprintln!("ghazal-daemon: hook {argv:?} exited with {s}"),
                Err(e) => eprintln!("ghazal-daemon: hook {argv:?}: {e}"),
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
        Err(e) => eprintln!("ghazal-daemon: exec {prog}: {e}"),
    }
}
