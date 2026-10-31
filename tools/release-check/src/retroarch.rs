//! Runs the runtime's own RetroArch with null video and audio against a
//! virtual copy of the handheld's pad, and checks what the real controls do:
//! the pad is configured from the shipped profile, the hotkeys the README
//! promises work (Menu + R1/L1 save and load a state, Menu + Start quits), and
//! the automatic resume state written on quit loads on the next launch.
//!
//! Betas 8 and 9 shipped without the pad's profile: RetroArch started, played
//! and ignored every button. Only a running RetroArch shows that.

use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::gamepad::{Tagged, VirtualPad};
use crate::profile::Profile;
use crate::tree::under;

/// What RetroArch writes to its log, as far as the checks read it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LogFacts {
    /// Pads it configured: the name shown and the port.
    pub configured: Vec<(String, u32)>,
    /// Pads it found no profile for.
    pub unconfigured: Vec<String>,
    /// State files it wrote and read.
    pub saved: Vec<String>,
    pub loaded: Vec<String>,
    pub state_failures: Vec<String>,
    /// The automatic state at launch: its path, and whether loading worked.
    pub auto_loads: Vec<(String, bool)>,
}

/// Drops the leading `[INFO] [Autoconf]` tags (with or without a colon) from a log line.
fn strip_tags(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(tail) = rest.strip_prefix('[').and_then(|tail| tail.split_once(']')) {
        rest = tail.1.trim_start_matches(':').trim_start();
    }
    rest.trim()
}

/// The quoted path in `Saving state "path", 26912 bytes.`
fn quoted(text: &str) -> &str {
    text.split('"').nth(1).unwrap_or(text)
}

pub fn facts(log: &str) -> LogFacts {
    let mut facts = LogFacts::default();
    for line in log.lines() {
        if let Some((name, port)) = line.split_once(" configured in port ") {
            let port = port.trim_start_matches('#').trim_end_matches(['.', ' ']).parse().unwrap_or(0);
            facts.configured.push((strip_tags(name).to_owned(), port));
        } else if line.contains("not configured") {
            facts.unconfigured.push(strip_tags(line).to_owned());
        } else if let Some((_, rest)) = line.split_once("Auto-loading save state from ") {
            facts.auto_loads.push((quoted(rest).to_owned(), rest.contains("succeeded")));
        } else if let Some((_, rest)) = line.split_once("Saving state ") {
            facts.saved.push(quoted(rest).to_owned());
        } else if let Some((_, rest)) = line.split_once("Loading state ") {
            facts.loaded.push(quoted(rest).to_owned());
        } else if line.contains("Failed to load state") || line.contains("Failed to save state") {
            facts.state_failures.push(strip_tags(line).to_owned());
        }
    }
    facts
}

/// A 32 KiB Game Boy ROM that jumps to a loop at 0x150. Gambatte does not check
/// the logo or checksums, so this is all a core needs to run frames.
pub fn test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xc3, 0x50, 0x01]);
    rom[0x150..0x152].copy_from_slice(&[0x18, 0xfe]);
    rom
}

pub struct Step {
    pub name: &'static str,
    pub result: Result<String, String>,
}

pub enum Outcome {
    /// Nothing could be exercised on this machine.
    Skipped(String),
    Steps(Vec<Step>),
}

pub struct Options<'a> {
    pub tree: &'a Path,
    pub profile: &'a Profile,
    pub workdir: &'a Path,
    pub core: &'a str,
}

/// The steps so far; a failed one ends the run.
#[derive(Default)]
struct Steps(Vec<Step>);

impl Steps {
    /// Records a step and says whether the run may go on.
    fn record(&mut self, name: &'static str, result: Result<String, String>) -> bool {
        let ok = result.is_ok();
        self.0.push(Step { name, result });
        ok
    }
}

fn poll<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = probe() {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Settings that make RetroArch run without a display, speaker or the
/// handheld's data card; everything the checks exercise stays as shipped
/// (hotkeys, automatic and numbered save states, the joypad drivers).
fn overrides(options: &Options) -> String {
    let wd = options.workdir.display();
    let cfg = &options.profile.retroarch_cfg;
    let in_tree = |key: &str| under(options.tree, Path::new(cfg.get(key).unwrap_or_default())).display().to_string();
    [
        ("video_driver", "null".to_owned()),
        ("audio_driver", "null".to_owned()),
        ("audio_enable", "false".to_owned()),
        ("video_fullscreen", "false".to_owned()),
        ("config_save_on_exit", "false".to_owned()),
        ("network_cmd_enable", "false".to_owned()),
        ("joypad_autoconfig_dir", in_tree("joypad_autoconfig_dir")),
        ("libretro_directory", in_tree("libretro_directory")),
        ("system_directory", format!("{wd}/system")),
        ("savefile_directory", format!("{wd}/saves")),
        ("savestate_directory", format!("{wd}/states")),
        ("cache_directory", format!("{wd}/cache")),
        ("rgui_config_directory", format!("{wd}/config")),
        ("core_options_path", format!("{wd}/core-options.cfg")),
    ]
    .iter()
    .map(|(key, value)| format!("{key} = \"{value}\"\n"))
    .collect()
}

/// One RetroArch process and its log, killed when dropped so no failure
/// leaves it running.
struct Session {
    child: Child,
    log: std::path::PathBuf,
}

impl Session {
    fn start(options: &Options, retroarch: &Path, number: u8) -> Result<Self> {
        let wd = options.workdir;
        let log_path = wd.join(format!("retroarch-{number}.log"));
        let log = fs::File::create(&log_path)?;
        let child = Command::new(retroarch)
            .arg("-c")
            .arg(options.profile.dir.join("retroarch.cfg"))
            .arg("--appendconfig")
            .arg(wd.join("override.cfg"))
            .arg("-v")
            .arg("-L")
            .arg(options.tree.join("usr/lib/libretro").join(options.core))
            .arg(wd.join("test.gb"))
            .env("HOME", wd.join("home"))
            .env("XDG_CONFIG_HOME", wd.join("xdg"))
            .env("XDG_CACHE_HOME", wd.join("cache"))
            .env("XDG_RUNTIME_DIR", wd.join("run"))
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("starting RetroArch")?;
        Ok(Self { child, log: log_path })
    }

    fn text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn facts(&self) -> LogFacts {
        facts(&self.text())
    }

    fn tail(&self) -> String {
        let text = self.text();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(12)..].join("\n")
    }

    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// What the daemon's Menu + Start sends: RetroArch must save and quit.
    fn terminate(&self) {
        // SAFETY: signalling the child we own; it has not been reaped while `self` lives.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        poll(timeout, || self.exited())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Reaction {
    Waiting,
    Done(String),
    Failed(String),
}

struct Hotkey {
    name: &'static str,
    chord: [&'static str; 2],
    react: fn(&LogFacts) -> Reaction,
}

fn saved_a_state(found: &LogFacts) -> Reaction {
    if let Some(failure) = found.state_failures.first() {
        return Reaction::Failed(failure.clone());
    }
    match found.saved.first() {
        None => Reaction::Waiting,
        Some(path) if Path::new(path).is_file() => Reaction::Done(format!("wrote {path}")),
        Some(path) => Reaction::Failed(format!("RetroArch logged saving {path} but the file is not there")),
    }
}

fn loaded_a_state(found: &LogFacts) -> Reaction {
    if let Some(failure) = found.state_failures.first() {
        return Reaction::Failed(failure.clone());
    }
    found.loaded.first().map_or(Reaction::Waiting, |path| Reaction::Done(format!("read {path}")))
}

/// Save then load, so the load has a state to read.
const HOTKEYS: [Hotkey; 2] = [
    Hotkey { name: "Menu + R1 saves a state", chord: ["menu", "r1"], react: saved_a_state },
    Hotkey { name: "Menu + L1 loads a state", chord: ["menu", "l1"], react: loaded_a_state },
];

pub fn run(options: &Options) -> Result<Outcome> {
    let pad = match VirtualPad::create(options.profile) {
        Ok(pad) => pad,
        Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|io| matches!(io.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied)) => {
            return Ok(Outcome::Skipped(format!("no usable /dev/uinput ({error:#}); run as root with the uinput module loaded")));
        }
        Err(error) => return Err(error),
    };
    let mut steps = Steps::default();
    let how = match &pad.tagged {
        Tagged::ByUdevd => "udev marked it a joystick".to_owned(),
        Tagged::Written(entry) => format!("no udev daemon here; wrote {}", entry.display()),
    };
    steps.record("virtual pad is registered as the handheld's gamepad", Ok(format!("{} at {} ({how})", options.profile.pad.name, pad.node.display())));
    if let Err(error) = exercise(options, pad, &mut steps) {
        steps.record("headless RetroArch session", Err(format!("{error:#}")));
    }
    Ok(Outcome::Steps(steps.0))
}

fn exercise(options: &Options, mut pad: VirtualPad, steps: &mut Steps) -> Result<()> {
    let wd = options.workdir;
    for dir in ["system", "saves", "states", "cache", "config", "home", "xdg", "run"] {
        fs::create_dir_all(wd.join(dir))?;
    }
    let retroarch = options.tree.join("usr/bin/retroarch");
    let retroarch = fs::canonicalize(&retroarch).with_context(|| format!("resolving {}", retroarch.display()))?;
    let version = Command::new(&retroarch).arg("--version").output().context("starting retroarch --version")?;
    let banner = String::from_utf8_lossy(&version.stdout).lines().next().unwrap_or_default().to_owned();
    let started = if version.status.success() && banner.contains("RetroArch") {
        Ok(banner)
    } else {
        Err(format!("--version exited with {}: {}", version.status, String::from_utf8_lossy(&version.stderr).trim()))
    };
    if !steps.record("RetroArch starts", started) {
        return Ok(());
    }
    fs::write(wd.join("test.gb"), test_rom())?;
    fs::write(wd.join("override.cfg"), overrides(options))?;

    if !first_session(options, &retroarch, &mut pad, steps)? {
        return Ok(());
    }
    second_session(options, &retroarch, steps)
}

/// The pad, the hotkeys and quitting. Returns whether the run may go on.
fn first_session(options: &Options, retroarch: &Path, pad: &mut VirtualPad, steps: &mut Steps) -> Result<bool> {
    let mut game = Session::start(options, retroarch, 1)?;
    // RetroArch configures the pad as the core starts.
    let configured = poll(Duration::from_secs(30), || {
        let found = game.facts();
        (!found.configured.is_empty() || !found.unconfigured.is_empty() || game.exited().is_some()).then_some(found)
    });
    let profile_pad = &options.profile.pad;
    let wanted = [profile_pad.display_name.as_str(), profile_pad.name.as_str()];
    let result = match configured {
        Some(found) if found.configured.iter().any(|(name, port)| wanted.contains(&name.as_str()) && *port == 1) => Ok(format!("{:?} configured in port 1", found.configured[0].0)),
        Some(found) if !found.unconfigured.is_empty() => Err(format!("RetroArch found no profile for the pad: {}", found.unconfigured.join("; "))),
        Some(found) => Err(format!("RetroArch did not configure {wanted:?} in port 1 (configured: {:?}); log tail:\n{}", found.configured, game.tail())),
        None => Err(format!("RetroArch never configured the pad; log tail:\n{}", game.tail())),
    };
    if !steps.record("RetroArch configures the pad from the shipped profile", result) {
        return Ok(false);
    }

    // The hotkeys the README lists, through the pad's real button codes.
    for hotkey in HOTKEYS {
        // RetroArch's hotkeys only act on frames it has run.
        std::thread::sleep(Duration::from_millis(500));
        pad.press(&hotkey.chord)?;
        let reaction = poll(Duration::from_secs(10), || match (hotkey.react)(&game.facts()) {
            Reaction::Waiting => None,
            done => Some(done),
        });
        let result = match reaction {
            Some(Reaction::Done(detail)) => Ok(detail),
            Some(Reaction::Failed(problem)) => Err(problem),
            Some(Reaction::Waiting) | None => Err(format!("RetroArch did not react to {}; log tail:\n{}", hotkey.chord.join(" + "), game.tail())),
        };
        if !steps.record(hotkey.name, result) {
            return Ok(false);
        }
    }

    // Quitting through RetroArch's own hotkey asks for confirmation (press twice).
    // On the handheld the daemon's Menu + Start sends SIGTERM instead; the second
    // session covers that.
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(700));
        if game.exited().is_none() {
            pad.press(&["menu", "start"])?;
        }
    }
    let result = match game.wait_for_exit(Duration::from_secs(10)) {
        Some(status) if status.success() => Ok("RetroArch quit cleanly".to_owned()),
        Some(status) => Err(format!("RetroArch quit with {status}; log tail:\n{}", game.tail())),
        None => Err(format!("RetroArch kept running after Menu + Start twice; log tail:\n{}", game.tail())),
    };
    Ok(steps.record("Menu + Start quits RetroArch", result))
}

/// Launching again resumes from the automatic state the first quit wrote, and
/// SIGTERM (what the daemon's Menu + Start sends) ends the game cleanly.
fn second_session(options: &Options, retroarch: &Path, steps: &mut Steps) -> Result<()> {
    let mut game = Session::start(options, retroarch, 2)?;
    let resumed = poll(Duration::from_secs(30), || {
        let found = game.facts();
        (!found.auto_loads.is_empty() || game.exited().is_some()).then_some(found)
    });
    let result = match resumed.as_ref().and_then(|found| found.auto_loads.first()) {
        Some((path, true)) => Ok(format!("resumed from {path}")),
        Some((path, false)) => Err(format!("loading the automatic state {path} failed; log tail:\n{}", game.tail())),
        None => Err(format!("RetroArch did not load an automatic state on launch; log tail:\n{}", game.tail())),
    };
    if !steps.record("the next launch resumes from the automatic state", result) {
        return Ok(());
    }
    game.terminate();
    let result = match game.wait_for_exit(Duration::from_secs(10)) {
        Some(status) if status.success() => Ok("RetroArch saved and quit with status 0".to_owned()),
        Some(status) => Err(format!("RetroArch quit with {status} after SIGTERM; log tail:\n{}", game.tail())),
        None => Err(format!("RetroArch kept running after SIGTERM; log tail:\n{}", game.tail())),
    };
    steps.record("SIGTERM quits RetroArch cleanly", result);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_configuration_messages() {
        let found = facts("[INFO] [Autoconf] Anbernic RG40XX-V Gamepad configured in port 1.\nnoise\n[INFO] [Autoconf]: Pad2 (1/1) not configured, using fallback\n");
        assert_eq!(found.configured, [("Anbernic RG40XX-V Gamepad".to_owned(), 1)]);
        assert_eq!(found.unconfigured, ["Pad2 (1/1) not configured, using fallback"]);
        assert_eq!(facts("Name configured in port #2").configured, [("Name".to_owned(), 2)]);
    }

    #[test]
    fn reads_state_messages() {
        let found = facts("[INFO] [State] Saving state \"/w/states/Gambatte/test.state\", 26912 bytes.\n[INFO] [State] Loading state \"/w/states/Gambatte/test.state\", 26912 bytes.\n[ERROR] Failed to load state from \"x\"\n");
        assert_eq!(found.saved, ["/w/states/Gambatte/test.state"]);
        assert_eq!(found.loaded, ["/w/states/Gambatte/test.state"]);
        assert_eq!(found.state_failures.len(), 1);
        assert_eq!(facts("nothing here"), LogFacts::default());
    }

    #[test]
    fn reads_automatic_state_messages() {
        let found = facts("[INFO] [State] Auto-loading save state from \"/w/test.state.auto\" succeeded.\n[ERROR] Auto-loading save state from \"/w/other\" failed.\n");
        assert_eq!(found.auto_loads, [("/w/test.state.auto".to_owned(), true), ("/w/other".to_owned(), false)]);
        // The automatic load is not a manual one.
        assert!(found.loaded.is_empty());
    }

    #[test]
    fn the_test_rom_is_a_valid_size_and_jumps_to_its_loop() {
        let rom = test_rom();
        assert_eq!(rom.len(), 0x8000);
        assert_eq!(&rom[0x100..0x104], &[0x00, 0xc3, 0x50, 0x01]);
        assert_eq!(&rom[0x150..0x152], &[0x18, 0xfe]);
    }
}
