//! Keeps the screens, the panel's backlight and the sound route in step with the
//! HDMI cable and the user's choice. The frontend polls the connector while it
//! owns the display; during a game only the daemon watches the cable, and it
//! lights the panel again if the cable goes away.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::Display;
use crate::daemon::ctl;
use crate::device::DeviceConfig;
use crate::frontend::launcher::LaunchSpec;
use crate::hdmi::{self, Connection, Layout, Outputs, Policy, Sound, Status};

/// Reading a connector's status is a register read; two seconds feels
/// immediate for a cable and costs the idle menu next to nothing.
const POLL: Duration = Duration::from_secs(2);

/// RetroArch overrides for one launch (see `LaunchSpec::with_sound_device`).
const RETROARCH_ROUTE: &str = "/run/ghazal/retroarch-route.cfg";

/// What changed when the screens were brought in line.
pub struct Update {
    pub status: Status,
    /// Why HDMI could not be used, for a toast.
    pub error: Option<String>,
}

pub struct Routing {
    socket: PathBuf,
    /// What the last synchronisation acted on. A failed attempt is not retried
    /// until the policy or the cable changes.
    applied: Option<(Policy, Connection)>,
    layout: Layout,
    next_poll: Option<Instant>,
    /// Boards without HDMI never poll.
    absent: bool,
    /// Whether an earlier frontend's forcing of the connector has been undone.
    released: bool,
}

/// How one launch reaches the screens and speakers.
pub struct LaunchPlan {
    pub layout: Layout,
    pub sound: Sound,
}

impl Routing {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket, applied: None, layout: Layout::Panel, next_poll: None, absent: false, released: false }
    }

    /// When the loop must wake to look at the cable.
    pub fn next_poll(&self) -> Option<Instant> {
        if self.absent { None } else { self.next_poll }
    }

    /// Whether the policy changed or the poll is due.
    pub fn due(&self, policy: Policy, now: Instant) -> bool {
        !self.absent && (self.next_poll.is_none_or(|at| now >= at) || self.applied.is_some_and(|(applied, _)| applied != policy))
    }

    /// Reads the cable and brings the screens in line with `policy`.
    pub fn sync(&mut self, display: &mut Display, policy: Policy, now: Instant) -> Option<Update> {
        if self.absent { return None; }
        self.next_poll = Some(now + POLL);
        if !std::mem::replace(&mut self.released, true) { display.release_forced(); }
        let connection = display.hdmi_connection();
        if connection == Connection::Absent {
            self.absent = true;
            self.next_poll = None;
            return None;
        }
        let first = self.applied.is_none();
        if self.applied == Some((policy, connection)) { return None; }
        let (layout, error) = match display.set_layout(Layout::resolve(policy, connection)) {
            Ok(layout) => (layout, None),
            Err(error) => (Layout::Panel, Some(format!("{error:#}"))),
        };
        self.applied = Some((policy, connection));
        // The daemon starts lit; it only needs to hear about a change (or hear once, so
        // a backlight left off by a frontend that died in a HDMI layout cannot persist).
        if first || layout.panel_lit() != self.layout.panel_lit() {
            self.tell_daemon(Outputs { panel_lit: layout.panel_lit(), sound: Sound::Speaker });
        }
        self.layout = layout;
        Some(Update { status: Status { connection, layout, mode: display.hdmi_mode() }, error })
    }

    /// Settles the layout and the sound route for a launch.
    pub fn prepare_launch(&mut self, display: &mut Display, device: &DeviceConfig, policy: Policy) -> (LaunchPlan, Option<Update>) {
        // A cable plugged in during the last poll interval still counts.
        let update = self.sync(display, policy, Instant::now());
        let layout = display.layout();
        let sound = if layout.game_on_hdmi() && device.audio.as_ref().is_some_and(|audio| audio.hdmi.is_some()) {
            self.tell_daemon(Outputs { panel_lit: layout.panel_lit(), sound: Sound::Hdmi }).map_or(Sound::Speaker, |actual| actual.sound)
        } else {
            Sound::Speaker
        };
        (LaunchPlan { layout, sound }, update)
    }

    /// Puts the speaker back and notices a cable that changed during the game.
    pub fn finish_launch(&mut self, display: &mut Display, plan: &LaunchPlan) {
        if plan.sound == Sound::Hdmi {
            self.tell_daemon(Outputs { panel_lit: display.layout().panel_lit(), sound: Sound::Speaker });
        }
        // A restore that dropped HDMI, or a cable pulled meanwhile, shows in the next sync.
        if display.layout() != self.layout || plan.layout != Layout::Panel { self.applied = None; }
        self.layout = display.layout();
        self.next_poll = None;
    }

    /// The daemon's answer says what it could actually do; `None` when it is unreachable.
    fn tell_daemon(&self, outputs: Outputs) -> Option<Outputs> {
        let reply = ctl::request(&self.socket, &format!("output {}", outputs.to_args()))
            .and_then(|reply| Outputs::from_reply(&reply));
        match reply {
            Ok(actual) => Some(actual),
            Err(error) => {
                eprintln!("ghazal: telling the daemon {:?}: {error:#}", outputs.to_args());
                None
            }
        }
    }
}

/// The ALSA device a launch opens for HDMI sound. Launches that stay on the
/// speaker are not touched at all.
pub fn hdmi_sound_device(device: &DeviceConfig, sound: Sound) -> Option<String> {
    match (sound, device.audio.as_ref()?.hdmi.as_ref()) {
        (Sound::Hdmi, Some(hdmi)) => Some(hdmi::audio::alsa_device(hdmi)),
        _ => None,
    }
}

/// Applies a launch's sound route to its program.
pub fn route_sound(spec: LaunchSpec, device: &DeviceConfig, sound: Sound) -> LaunchSpec {
    match hdmi_sound_device(device, sound) {
        Some(alsa) => spec.with_hdmi_sound(&alsa, Path::new(RETROARCH_ROUTE)),
        None => spec,
    }
}

/// RetroArch saves the settings it ran with on exit, which include the HDMI
/// device an appended configuration gave it. Puts the speaker's back so the
/// next launch (and the RetroArch menu) starts from it.
pub fn forget_hdmi_device(user_config: &Path, device: &DeviceConfig) {
    let (Some(audio), Some(hdmi_device)) = (device.audio.as_ref(), hdmi_sound_device(device, Sound::Hdmi)) else { return };
    let Ok(text) = std::fs::read_to_string(user_config) else { return };
    if let Some(fixed) = replace_audio_device(&text, &hdmi_device, &format!("hw:{},0", audio.card)) {
        if let Err(error) = crate::sys::write_atomic(user_config, fixed.as_bytes()) {
            eprintln!("ghazal: restoring RetroArch's sound device: {error:#}");
        }
    }
}

/// `text` with every `audio_device = "from"` line pointing at `to`; `None` when there is none.
fn replace_audio_device(text: &str, from: &str, to: &str) -> Option<String> {
    let line = format!("audio_device = \"{from}\"");
    text.lines().any(|candidate| candidate.trim() == line).then(|| {
        text.lines().map(|candidate| if candidate.trim() == line { format!("audio_device = \"{to}\"") } else { candidate.to_owned() }).collect::<Vec<_>>().join("\n") + "\n"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;

    #[test]
    fn only_hdmi_sound_changes_a_launch() {
        let device = Profile { dir: None }.device().unwrap();
        assert_eq!(hdmi_sound_device(&device, Sound::Hdmi), Some("plughw:HDMI,0".into()));
        assert_eq!(hdmi_sound_device(&device, Sound::Speaker), None);
        let mut without_card = device;
        without_card.audio.as_mut().unwrap().hdmi = None;
        assert_eq!(hdmi_sound_device(&without_card, Sound::Hdmi), None);
    }

    #[test]
    fn the_hdmi_device_is_taken_out_of_a_saved_retroarch_config() {
        let saved = "video_driver = \"gl\"\naudio_device = \"plughw:HDMI,0\"\naudio_driver = \"alsathread\"\n";
        assert_eq!(replace_audio_device(saved, "plughw:HDMI,0", "hw:0,0").unwrap(), "video_driver = \"gl\"\naudio_device = \"hw:0,0\"\naudio_driver = \"alsathread\"\n");
        assert_eq!(replace_audio_device("audio_device = \"hw:0,0\"\n", "plughw:HDMI,0", "hw:0,0"), None);
        assert_eq!(replace_audio_device("", "plughw:HDMI,0", "hw:0,0"), None);
    }

    #[test]
    fn polling_waits_its_interval_unless_the_policy_changed() {
        let now = Instant::now();
        let mut routing = Routing::new("/nonexistent".into());
        assert!(routing.due(Policy::Auto, now), "never polled yet");
        routing.next_poll = Some(now + POLL);
        routing.applied = Some((Policy::Auto, Connection::Disconnected));
        assert!(!routing.due(Policy::Auto, now));
        assert!(routing.due(Policy::Off, now), "a new choice applies at once");
        assert!(routing.due(Policy::Auto, now + POLL));
        routing.absent = true;
        assert!(!routing.due(Policy::Off, now + POLL) && routing.next_poll().is_none());
    }
}
