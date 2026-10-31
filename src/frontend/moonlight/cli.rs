//! Moonlight's command line: the arguments of each action and the reading of
//! what it prints. `moonlight` always exits 0 after a failed pair, unpair or
//! list, so its messages, not its status, tell what happened.

use std::ffi::OsString;
use std::path::PathBuf;

use super::job::{Captured, Ended};
use super::store::{Host, StreamSettings, DEFAULT_PORT};
use crate::frontend::config;

/// The four digits shown for the user to type on the host, 0001 to 9999.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin(u16);

impl Pin {
    /// Maps any number onto a PIN; callers pass random bytes.
    pub fn from_entropy(entropy: u32) -> Self { Self(1 + (entropy % 9999) as u16) }

    pub fn random() -> Self {
        let mut bytes = [0u8; 4];
        // Without entropy the PIN is merely guessable, which the host's own
        // confirmation step still guards against.
        let _ = std::fs::File::open("/dev/urandom").and_then(|mut urandom| std::io::Read::read_exact(&mut urandom, &mut bytes));
        Self::from_entropy(u32::from_le_bytes(bytes))
    }
}

impl std::fmt::Display for Pin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(formatter, "{:04}", self.0) }
}

/// Where Moonlight keeps this device's client key and ID.
pub fn keys_dir(config: &config::Moonlight) -> PathBuf { config.data_dir.join("keys") }

pub fn store_path(config: &config::Moonlight) -> PathBuf { config.data_dir.join("moonlight.toml") }

/// `ACTION HOST -port N -keydir DIR`: what every action takes.
fn base(action: &str, host: &Host, config: &config::Moonlight) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![action.into(), host.address.as_str().into()];
    if host.port != DEFAULT_PORT { args.extend(["-port".into(), host.port.to_string().into()]); }
    args.extend(["-keydir".into(), keys_dir(config).into()]);
    args
}

pub fn pair(config: &config::Moonlight, host: &Host, pin: Pin) -> Vec<OsString> {
    let mut args = base("pair", host, config);
    args.extend(["-pin".into(), pin.to_string().into()]);
    args
}

pub fn list(config: &config::Moonlight, host: &Host) -> Vec<OsString> { base("list", host, config) }
pub fn unpair(config: &config::Moonlight, host: &Host) -> Vec<OsString> { base("unpair", host, config) }
pub fn quit(config: &config::Moonlight, host: &Host) -> Vec<OsString> { base("quit", host, config) }

/// The stream: SDL for video, audio and the pad, H.264 (the only decoder in
/// the image), the size Moonlight asks the host to encode.
pub fn stream(config: &config::Moonlight, host: &Host, app: &str, settings: &StreamSettings) -> Vec<OsString> {
    let mut args = base("stream", host, config);
    let (width, height) = settings.resolution.size();
    let mut options: Vec<(&str, String)> = vec![
        ("-platform", "sdl".into()),
        ("-codec", "h264".into()),
        ("-width", width.to_string()),
        ("-height", height.to_string()),
        ("-fps", settings.fps.value().to_string()),
        ("-app", app.into()),
    ];
    if let Some(kbps) = settings.bitrate.kbps() { options.push(("-bitrate", kbps.to_string())); }
    if let Some(mapping) = &config.mapping { options.push(("-mapping", mapping.display().to_string())); }
    args.extend(options.into_iter().flat_map(|(flag, value)| [flag.into(), value.into()]));
    if settings.quit_app_after { args.push("-quitappafter".into()); }
    args
}

/// The applications from `moonlight list`, which prints `1. ...`, `2. ...`
/// in the reverse of the host's order (it builds the list by prepending), so
/// this reads them back to front; the numbers mean nothing.
fn apps(output: &str) -> Vec<String> {
    let mut apps: Vec<String> = output.lines().filter_map(|line| {
        let (number, name) = line.split_once(". ")?;
        (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then(|| name.trim().to_owned()).filter(|name| !name.is_empty())
    }).collect();
    apps.reverse();
    apps
}

/// The first of Moonlight's failure messages in its output, else the last
/// line, without the prefix that only says which step failed.
fn failure(output: &str) -> Option<String> {
    const FAILURES: [&str; 8] = ["Can't", "Gamestream error", "Failed", "Invalid data", "Unsupported version", "Not enough memory", "Couldn't", "Server doesn't"];
    const STEPS: [&str; 3] = ["Failed to pair to server: ", "Failed to unpair to server: ", "Gamestream error: "];
    let lines = || output.lines().map(str::trim);
    let line = lines().find(|line| FAILURES.iter().any(|prefix| line.starts_with(prefix))).or_else(|| lines().rfind(|line| !line.is_empty()))?;
    Some(STEPS.iter().find_map(|step| line.strip_prefix(step)).unwrap_or(line).to_owned())
}

/// Why a helper that did not finish properly failed.
fn did_not_finish(run: &Captured) -> Option<String> {
    match &run.ended {
        Ended::CouldNotStart(message) => Some(message.clone()),
        Ended::TimedOut => Some("The host did not answer".into()),
        Ended::Exited(code) if *code != Some(0) => Some(failure(&run.output).unwrap_or_else(|| format!("Moonlight exited with {}", code.map_or("a signal".into(), |code| code.to_string())))),
        Ended::Exited(_) => None,
    }
}

#[derive(Debug, PartialEq)]
pub enum Listed {
    Apps(Vec<String>),
    NotPaired,
    Failed(String),
}

pub fn listed(run: &Captured) -> Listed {
    if run.output.contains("You must pair with the PC first") { return Listed::NotPaired; }
    if let Some(reason) = did_not_finish(run) { return Listed::Failed(reason); }
    // `list` exits 0 even when the host's answer was unusable.
    if run.output.contains("Can't get app list") { return Listed::Failed("The host's app list could not be read".into()); }
    Listed::Apps(apps(&run.output))
}

/// Why `quit` failed, if it did: it prints nothing on success.
pub fn quit_failure(run: &Captured) -> Option<String> {
    if run.output.contains("You must pair with the PC first") { return Some("Not paired with the host".into()); }
    did_not_finish(run)
}

/// Whether a pair or unpair went through, or why not.
#[derive(Debug, PartialEq)]
pub enum Done {
    Yes,
    Failed(String),
}

fn done(run: &Captured, success: &[&str]) -> Done {
    if run.output.lines().any(|line| success.iter().any(|message| line.contains(message))) { return Done::Yes; }
    Done::Failed(did_not_finish(run).or_else(|| failure(&run.output)).unwrap_or_else(|| "The host refused".into()))
}

/// Moonlight spells it "Succesfully"; a fixed upstream would not.
pub fn paired(run: &Captured) -> Done { done(run, &["Succesfully paired", "Successfully paired"]) }
pub fn unpaired(run: &Captured) -> Done { done(run, &["Succesfully unpaired", "Successfully unpaired"]) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::moonlight::store::{Address, Bitrate, Fps, Pairing, Resolution};

    fn config() -> config::Moonlight {
        config::Moonlight { bin: "/usr/bin/moonlight".into(), data_dir: "/data/moonlight".into(), mapping: Some("/usr/share/moonlight/gamecontrollerdb.txt".into()), env: Default::default(), log: None }
    }

    fn host(port: u16) -> Host {
        Host { name: "PC".into(), address: Address::try_from("192.168.1.20".to_owned()).unwrap(), port, pairing: Pairing::Paired }
    }

    fn strings(args: Vec<OsString>) -> Vec<String> { args.into_iter().map(|arg| arg.to_string_lossy().into_owned()).collect() }

    fn exited(code: i32, output: &str) -> Captured { Captured { ended: Ended::Exited(Some(code)), output: output.into() } }

    #[test]
    fn helpers_take_the_host_the_port_when_unusual_and_the_key_directory() {
        assert_eq!(strings(list(&config(), &host(47989))), ["list", "192.168.1.20", "-keydir", "/data/moonlight/keys"]);
        assert_eq!(strings(unpair(&config(), &host(48000))), ["unpair", "192.168.1.20", "-port", "48000", "-keydir", "/data/moonlight/keys"]);
        assert_eq!(strings(pair(&config(), &host(47989), Pin::from_entropy(481))), ["pair", "192.168.1.20", "-keydir", "/data/moonlight/keys", "-pin", "0482"]);
    }

    #[test]
    fn a_stream_asks_for_sdl_h264_the_chosen_size_and_the_app() {
        let settings = StreamSettings { resolution: Resolution::R1280x720, fps: Fps::Thirty, bitrate: Bitrate::Mbps10, quit_app_after: true };
        assert_eq!(strings(stream(&config(), &host(47989), "Steam Big Picture", &settings)), [
            "stream", "192.168.1.20", "-keydir", "/data/moonlight/keys", "-platform", "sdl", "-codec", "h264",
            "-width", "1280", "-height", "720", "-fps", "30", "-app", "Steam Big Picture", "-bitrate", "10000",
            "-mapping", "/usr/share/moonlight/gamecontrollerdb.txt", "-quitappafter",
        ]);
        let plain = strings(stream(&config(), &host(47989), "Desktop", &StreamSettings { bitrate: Bitrate::Auto, quit_app_after: false, ..settings }));
        assert!(!plain.contains(&"-bitrate".to_owned()) && !plain.contains(&"-quitappafter".to_owned()));
    }

    #[test]
    fn pins_are_four_digits_and_never_zero() {
        assert_eq!(Pin::from_entropy(0).to_string(), "0001");
        assert_eq!(Pin::from_entropy(9998).to_string(), "9999");
        assert_eq!(Pin::from_entropy(9999).to_string(), "0001");
        assert_eq!(Pin::from_entropy(u32::MAX).to_string().len(), 4);
        assert_eq!(Pin::random().to_string().len(), 4);
    }

    #[test]
    fn the_app_list_is_read_from_numbered_lines_in_the_hosts_order() {
        // As the real `moonlight list` printed it for a host whose apps are
        // Desktop, Steam Big Picture, Half-Life 2: Episode One.
        let output = "Connecting to 192.168.1.20...\n1. Half-Life 2: Episode One \n2. Steam Big Picture\n3. Desktop\n";
        assert_eq!(listed(&exited(0, output)), Listed::Apps(vec!["Desktop".into(), "Steam Big Picture".into(), "Half-Life 2: Episode One".into()]));
        assert_eq!(listed(&exited(0, "Connecting to x...\n")), Listed::Apps(vec![]));
    }

    #[test]
    fn listing_tells_unpaired_from_unreachable_from_broken() {
        assert_eq!(listed(&exited(255, "Connecting to x...\nYou must pair with the PC first\n")), Listed::NotPaired);
        assert_eq!(listed(&exited(255, "Connecting to x...\nCan't connect to server x\n")), Listed::Failed("Can't connect to server x".into()));
        assert_eq!(listed(&exited(255, "Connecting to x...\nGamestream error: Connection timed out\n")), Listed::Failed("Connection timed out".into()));
        assert_eq!(listed(&exited(0, "Can't get app list\n")), Listed::Failed("The host's app list could not be read".into()));
        assert_eq!(listed(&Captured { ended: Ended::TimedOut, output: String::new() }), Listed::Failed("The host did not answer".into()));
        assert_eq!(listed(&Captured { ended: Ended::Exited(None), output: String::new() }), Listed::Failed("Moonlight exited with a signal".into()));
    }

    #[test]
    fn pairing_succeeds_on_moonlights_message_and_reports_its_failure() {
        assert_eq!(paired(&exited(0, "Please enter the following PIN on the target PC: 0482\nSuccesfully paired\n")), Done::Yes);
        assert_eq!(paired(&exited(0, "Successfully paired\n")), Done::Yes);
        assert_eq!(paired(&exited(0, "Please enter the following PIN on the target PC: 0482\nFailed to pair to server: Pairing failed\n")), Done::Failed("Pairing failed".into()));
        assert_eq!(paired(&exited(0, "Failed to pair to server: Already in pairing session\n")), Done::Failed("Already in pairing session".into()));
        assert_eq!(unpaired(&exited(0, "Succesfully unpaired\n")), Done::Yes);
        assert_eq!(unpaired(&exited(0, "Failed to unpair to server: nope\n")), Done::Failed("nope".into()));
        assert_eq!(paired(&exited(0, "")), Done::Failed("The host refused".into()));
    }
}
