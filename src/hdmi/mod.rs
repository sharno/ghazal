//! HDMI output. Decisions only: which screens show the picture, how the sink's
//! modes map onto ours, and the wire format between frontend and daemon.
//! `frontend::platform::kms` applies a layout; the daemon owns the panel's
//! power and the sound route (`audio`); `connector` reads the board's status.

pub mod audio;
pub mod connector;

use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

/// Configuration → HDMI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    /// A connected display takes over; the handheld's own screen goes dark.
    #[default]
    Auto,
    /// The menu shows on both screens; games run on HDMI.
    Mirror,
    /// HDMI is never used.
    Off,
}

impl Policy {
    const ALL: [Self; 3] = [Self::Auto, Self::Mirror, Self::Off];

    pub fn step(self, forward: bool) -> Self {
        let index = Self::ALL.iter().position(|policy| *policy == self).unwrap_or(0);
        Self::ALL[(index + if forward { 1 } else { Self::ALL.len() - 1 }) % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "HDMI: Auto",
            Self::Mirror => "HDMI: Mirror",
            Self::Off => "HDMI: Off",
        }
    }
}

/// What the board reports about its HDMI connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    /// The board has no HDMI connector.
    Absent,
    Disconnected,
    Connected,
}

/// Which screens show the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Panel,
    Hdmi,
    Mirror,
}

impl Layout {
    pub fn resolve(policy: Policy, connection: Connection) -> Self {
        match (policy, connection) {
            (Policy::Off, _) | (_, Connection::Absent | Connection::Disconnected) => Self::Panel,
            (Policy::Auto, Connection::Connected) => Self::Hdmi,
            (Policy::Mirror, Connection::Connected) => Self::Mirror,
        }
    }

    /// Games run on HDMI whenever it is part of the layout: an emulator owns
    /// one connector and cannot mirror.
    pub fn game_on_hdmi(self) -> bool {
        !matches!(self, Self::Panel)
    }

    /// Only a handheld-less layout lets the backlight go dark.
    pub fn panel_lit(self) -> bool {
        !matches!(self, Self::Hdmi)
    }
}

/// What Configuration and Information show about HDMI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub connection: Connection,
    /// The layout in effect, which is the panel's when HDMI could not be used.
    pub layout: Layout,
    pub mode: Option<(u16, u16)>,
}

impl Status {
    pub fn describe(self) -> String {
        match (self.connection, self.layout, self.mode) {
            (Connection::Absent, ..) => "Not on this device".into(),
            (Connection::Disconnected, ..) => "No display".into(),
            (Connection::Connected, Layout::Panel, _) => "Not in use".into(),
            (Connection::Connected, layout, mode) => {
                let mode = mode.map_or_else(String::new, |(width, height)| format!(" {width}x{height}"));
                format!("{}{mode}", if layout == Layout::Mirror { "Mirrored" } else { "Active" })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Speaker,
    Hdmi,
}

/// What the frontend asks the daemon to apply: `output panel=on|off sound=speaker|hdmi`.
/// The reply has the same shape and carries what was actually possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outputs {
    pub panel_lit: bool,
    pub sound: Sound,
}

impl Outputs {
    pub fn parse<'a>(words: impl IntoIterator<Item = &'a str>) -> Result<Self> {
        let (mut panel_lit, mut sound) = (None, None);
        for word in words {
            match word.split_once('=') {
                Some(("panel", "on")) => panel_lit = Some(true),
                Some(("panel", "off")) => panel_lit = Some(false),
                Some(("sound", "speaker")) => sound = Some(Sound::Speaker),
                Some(("sound", "hdmi")) => sound = Some(Sound::Hdmi),
                _ => bail!("unknown output setting {word:?}"),
            }
        }
        Ok(Self {
            panel_lit: panel_lit.ok_or_else(|| anyhow!("output needs panel=on|off"))?,
            sound: sound.ok_or_else(|| anyhow!("output needs sound=speaker|hdmi"))?,
        })
    }

    pub fn to_args(self) -> String {
        format!(
            "panel={} sound={}",
            if self.panel_lit { "on" } else { "off" },
            match self.sound { Sound::Speaker => "speaker", Sound::Hdmi => "hdmi" }
        )
    }

    /// Reads the `panel=` and `sound=` fields out of a daemon reply line.
    pub fn from_reply(reply: &str) -> Result<Self> {
        Self::parse(reply.split_whitespace().filter(|word| word.starts_with("panel=") || word.starts_with("sound=")))
    }
}

/// `WIDTHxHEIGHT` in a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl FromStr for Size {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (width, height) = text.split_once('x').with_context(|| format!("size {text:?} must be WIDTHxHEIGHT"))?;
        let (width, height) = (width.parse::<u16>()?, height.parse::<u16>()?);
        if width == 0 || height == 0 { bail!("size {text:?} is empty"); }
        Ok(Self { width, height })
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

/// One of the sink's display modes, reduced to what choosing needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeInfo {
    pub width: u16,
    pub height: u16,
    pub millihertz: u32,
    pub interlaced: bool,
    pub preferred: bool,
}

impl ModeInfo {
    /// The refresh rate the way RetroArch computes it, so both pick the same
    /// mode and a launch keeps the link up instead of retraining it.
    pub fn millihertz(clock_khz: u32, htotal: u16, vtotal: u16, interlaced: bool, doublescan: bool, vscan: u16) -> u32 {
        let total = u64::from(htotal) * u64::from(vtotal) * u64::from(vscan.max(1)) * if doublescan { 2 } else { 1 };
        if total == 0 { return 0; }
        (u64::from(clock_khz) * 1_000_000 * if interlaced { 2 } else { 1 } / total) as u32
    }

    fn distance_from_60hz(self) -> u32 {
        self.millihertz.abs_diff(60_000)
    }
}

/// The mode the menu and games use on HDMI. The configured size at the rate
/// nearest 60 Hz; otherwise the largest progressive mode not taller than it;
/// otherwise whatever the sink prefers.
pub fn pick_mode(modes: &[ModeInfo], wanted: Size) -> Option<usize> {
    let best = |candidates: Vec<(usize, &ModeInfo)>| {
        candidates.into_iter().min_by_key(|(index, mode)| (std::cmp::Reverse(u32::from(mode.width) * u32::from(mode.height)), mode.distance_from_60hz(), !mode.preferred, *index)).map(|(index, _)| index)
    };
    let progressive = || modes.iter().enumerate().filter(|(_, mode)| !mode.interlaced);
    best(progressive().filter(|(_, mode)| (mode.width, mode.height) == (wanted.width, wanted.height)).collect())
        .or_else(|| best(progressive().filter(|(_, mode)| mode.height <= wanted.height && mode.width <= wanted.width).collect()))
        .or_else(|| modes.iter().position(|mode| mode.preferred))
        .or_else(|| (!modes.is_empty()).then_some(0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The largest rectangle with the source's aspect ratio, centred in the target.
pub fn fit((source_width, source_height): (u32, u32), (target_width, target_height): (u32, u32)) -> Option<Rect> {
    if [source_width, source_height, target_width, target_height].contains(&0) { return None; }
    let (width, height) = if u64::from(target_width) * u64::from(source_height) <= u64::from(target_height) * u64::from(source_width) {
        (target_width, (u64::from(source_height) * u64::from(target_width) / u64::from(source_width)).max(1) as u32)
    } else {
        ((u64::from(source_width) * u64::from(target_height) / u64::from(source_height)).max(1) as u32, target_height)
    };
    Some(Rect { x: (target_width - width) / 2, y: (target_height - height) / 2, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u16, height: u16, millihertz: u32) -> ModeInfo {
        ModeInfo { width, height, millihertz, interlaced: false, preferred: false }
    }

    #[test]
    fn policy_cycles_both_ways() {
        assert_eq!(Policy::Auto.step(true), Policy::Mirror);
        assert_eq!(Policy::Off.step(true), Policy::Auto);
        assert_eq!(Policy::Auto.step(false), Policy::Off);
        assert_eq!(Policy::Mirror.step(false), Policy::Auto);
    }

    #[test]
    fn layout_follows_policy_and_cable() {
        use Connection::{Absent, Connected, Disconnected};
        assert_eq!(Layout::resolve(Policy::Auto, Connected), Layout::Hdmi);
        assert_eq!(Layout::resolve(Policy::Mirror, Connected), Layout::Mirror);
        assert_eq!(Layout::resolve(Policy::Off, Connected), Layout::Panel);
        for connection in [Absent, Disconnected] {
            for policy in Policy::ALL {
                assert_eq!(Layout::resolve(policy, connection), Layout::Panel);
            }
        }
        assert!(Layout::Mirror.panel_lit() && Layout::Panel.panel_lit() && !Layout::Hdmi.panel_lit());
        assert!(Layout::Mirror.game_on_hdmi() && Layout::Hdmi.game_on_hdmi() && !Layout::Panel.game_on_hdmi());
    }

    #[test]
    fn status_reads_in_a_word_or_two() {
        let status = |connection, layout, mode| Status { connection, layout, mode }.describe();
        assert_eq!(status(Connection::Absent, Layout::Panel, None), "Not on this device");
        assert_eq!(status(Connection::Disconnected, Layout::Panel, None), "No display");
        assert_eq!(status(Connection::Connected, Layout::Panel, None), "Not in use");
        assert_eq!(status(Connection::Connected, Layout::Hdmi, Some((1280, 720))), "Active 1280x720");
        assert_eq!(status(Connection::Connected, Layout::Mirror, Some((1280, 720))), "Mirrored 1280x720");
        assert_eq!(status(Connection::Connected, Layout::Hdmi, None), "Active");
    }

    #[test]
    fn outputs_round_trip_and_reject_gaps() {
        let outputs = Outputs { panel_lit: false, sound: Sound::Hdmi };
        assert_eq!(outputs.to_args(), "panel=off sound=hdmi");
        assert_eq!(Outputs::parse(outputs.to_args().split(' ')).unwrap(), outputs);
        assert_eq!(Outputs::from_reply("ok brightness=70 panel=on sound=speaker").unwrap(), Outputs { panel_lit: true, sound: Sound::Speaker });
        assert!(Outputs::parse(["panel=on"]).is_err());
        assert!(Outputs::parse(["panel=maybe", "sound=hdmi"]).is_err());
        assert!(Outputs::parse(["panel=on", "sound=hdmi", "extra"]).is_err());
    }

    #[test]
    fn sizes_parse_from_config_text() {
        assert_eq!("1280x720".parse::<Size>().unwrap(), Size { width: 1280, height: 720 });
        for bad in ["1280", "0x720", "axb", "1280x", "70000x1"] {
            assert!(bad.parse::<Size>().is_err(), "{bad}");
        }
        #[derive(Deserialize)]
        struct Holder { size: Size }
        assert_eq!(toml::from_str::<Holder>("size = \"640x480\"").unwrap().size, Size { width: 640, height: 480 });
    }

    #[test]
    fn refresh_matches_retroarchs_formula() {
        // CEA 1280x720@60: 74.25 MHz over 1650x750.
        assert_eq!(ModeInfo::millihertz(74_250, 1650, 750, false, false, 0), 60_000);
        // 59.94 Hz variant: 74.176 MHz.
        assert_eq!(ModeInfo::millihertz(74_176, 1650, 750, false, false, 0), 59_940);
        // 1080i is quoted per frame by doubling the field rate.
        assert_eq!(ModeInfo::millihertz(74_250, 2200, 1125, true, false, 0), 60_000);
        assert_eq!(ModeInfo::millihertz(25_175, 0, 0, false, false, 0), 0);
    }

    #[test]
    fn the_wanted_size_wins_at_the_rate_nearest_60hz() {
        let modes = [mode(1920, 1080, 60_000), mode(1280, 720, 50_000), mode(1280, 720, 59_940), mode(1280, 720, 60_000), mode(640, 480, 60_000)];
        assert_eq!(pick_mode(&modes, Size { width: 1280, height: 720 }), Some(3));
        let ntsc_only = [mode(1280, 720, 50_000), mode(1280, 720, 59_940)];
        assert_eq!(pick_mode(&ntsc_only, Size { width: 1280, height: 720 }), Some(1));
    }

    #[test]
    fn without_the_wanted_size_the_largest_smaller_mode_wins() {
        let modes = [mode(1920, 1080, 60_000), mode(1024, 768, 60_000), mode(640, 480, 60_000)];
        assert_eq!(pick_mode(&modes, Size { width: 1280, height: 720 }), Some(2), "1024x768 is taller than 720");
        let modes = [mode(1920, 1080, 60_000), mode(800, 600, 60_000), mode(640, 480, 60_000)];
        assert_eq!(pick_mode(&modes, Size { width: 1280, height: 720 }), Some(1));
    }

    #[test]
    fn interlaced_modes_are_a_last_resort() {
        let interlaced = ModeInfo { interlaced: true, ..mode(1280, 720, 60_000) };
        let sink_only_interlaced = [interlaced, ModeInfo { preferred: true, interlaced: true, ..mode(1920, 1080, 60_000) }];
        assert_eq!(pick_mode(&sink_only_interlaced, Size { width: 1280, height: 720 }), Some(1), "falls back to the preferred mode");
        let big_only = [mode(1920, 1080, 60_000), ModeInfo { preferred: true, ..mode(3840, 2160, 30_000) }];
        assert_eq!(pick_mode(&big_only, Size { width: 1280, height: 720 }), Some(1));
        assert_eq!(pick_mode(&[], Size { width: 1280, height: 720 }), None);
    }

    #[test]
    fn a_4_3_picture_is_centred_in_a_16_9_mode() {
        assert_eq!(fit((640, 480), (1280, 720)), Some(Rect { x: 160, y: 0, width: 960, height: 720 }));
        assert_eq!(fit((640, 480), (640, 480)), Some(Rect { x: 0, y: 0, width: 640, height: 480 }));
        assert_eq!(fit((640, 480), (1920, 1080)), Some(Rect { x: 240, y: 0, width: 1440, height: 1080 }));
        // A portrait panel keeps its shape on a wide display.
        assert_eq!(fit((480, 640), (1280, 720)), Some(Rect { x: 370, y: 0, width: 540, height: 720 }));
        assert_eq!(fit((640, 480), (0, 720)), None);
    }
}
