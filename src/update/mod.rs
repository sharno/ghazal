//! GitHub discovers releases; RAUC verifies, installs and selects the inactive slot.
//! Nothing touches the network on the normal boot path.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    fs,
    io::Write,
    os::fd::AsRawFd,
    path::Path,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
const IMAGE: &str = "/etc/ghazal/image.json";
const RAUC: &str = "/usr/bin/rauc";
const LOCK: &str = "/run/ghazal/update.lock";
const CHANNEL: &str = "/var/lib/ghazal/update-channel";
/// The kernel starts it at probe and pets it until userspace opens it.
const WATCHDOG: &str = "/dev/watchdog0";

/// The descriptor `health` pets, for its SIGTERM handler.
static PETTED_WATCHDOG: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Shutdown stops services first; stopping the watchdog (magic close) keeps a
/// long emulator or unmount wait from resetting a board that should power off.
extern "C" fn release_watchdog_and_exit(_: libc::c_int) {
    let fd = PETTED_WATCHDOG.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: write and _exit are async-signal-safe; the buffer is static.
    unsafe {
        if fd >= 0 { libc::write(fd, b"V".as_ptr().cast(), 1); }
        libc::_exit(0);
    }
}

/// Stops the watchdog for modes nothing pets it in: recovery and the emergency
/// console stay reachable instead of resetting a minute after boot.
pub fn disarm_watchdog() {
    match fs::OpenOptions::new().write(true).open(WATCHDOG).and_then(|mut watchdog| watchdog.write_all(b"V")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("ghazal: disarming {WATCHDOG}: {error}"),
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Channel {
    Stable,
    #[default]
    Beta,
}
impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Beta => "Beta",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Stable => Self::Beta,
            Self::Beta => Self::Stable,
        }
    }
    pub fn load() -> Result<Self> {
        match fs::read_to_string(CHANNEL) {
            Ok(value) => match value.trim() {
                "stable" => Ok(Self::Stable),
                "beta" => Ok(Self::Beta),
                _ => bail!("invalid update channel"),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save(self) -> Result<()> {
        crate::sys::write_atomic(
            Path::new(CHANNEL),
            match self {
                Self::Stable => b"stable\n",
                Self::Beta => b"beta\n",
            },
        )
    }
}
#[derive(Deserialize)]
struct Image {
    device: Device,
    memory: String,
    version: String,
}
#[derive(Deserialize)]
struct Device {
    id: String,
}
impl Image {
    fn read() -> Result<Self> {
        let image: Self = serde_json::from_slice(&fs::read(IMAGE)?)?;
        if !image
            .device
            .id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !matches!(image.memory.as_str(), "ddr3" | "ddr4")
        {
            bail!("invalid image identity");
        }
        Ok(image)
    }
    fn compatible(&self) -> String {
        format!("ghazal-{}-{}-ab1", self.device.id, self.memory)
    }
    fn installed(&self) -> Result<semver::Version> {
        parse_version(&self.version).context("installed image is not a release version; install a tagged image first")
    }
}
fn parse_version(tag: &str) -> Result<semver::Version, semver::Error> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag))
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
impl Release {
    fn version(&self) -> Option<semver::Version> {
        parse_version(&self.tag_name).ok()
    }
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}
#[derive(Clone)]
pub struct Candidate {
    pub version: String,
    url: String,
    size: u64,
}
pub enum Event {
    Status(String),
    Available(Option<Candidate>),
    Installed,
    Failed(String),
}
fn curl() -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command.env("CURL_CA_BUNDLE", "/etc/ssl/certs/ca-certificates.crt");
    command.env_remove("LD_LIBRARY_PATH").args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "20",
        // A dropped Wi-Fi link would otherwise hold the update screen (and
        // its blocked menu and power button) until the 30-minute limit.
        "--speed-limit",
        "1",
        "--speed-time",
        "60",
        "--retry",
        "2",
        "--user-agent",
        "Ghazal-updater",
    ]);
    command
}
/// Runs curl and returns its output. A failure carries a short reason the
/// update screen can show on one line; curl's own stderr, which can span
/// several lines (`--retry` warnings), stays in the error chain for the log.
fn fetch(command: &mut Command) -> Result<Vec<u8>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(anyhow::anyhow!(detail).context(curl_reason(output.status.code())))
}
/// What a player can do about curl's exit codes (curl(1), EXIT CODES).
fn curl_reason(code: Option<i32>) -> &'static str {
    match code {
        Some(6) => "No internet: could not look up GitHub; check Wi-Fi",
        Some(7 | 28) => "GitHub did not answer; check Wi-Fi and try again",
        Some(22) => "GitHub refused the request; try again later",
        Some(35 | 60) => "Secure connection to GitHub failed; the clock may be wrong",
        Some(23) => "Could not save the download; check the data card",
        Some(63) => "GitHub sent more than expected",
        _ => "Could not reach GitHub",
    }
}
/// GitHub lists releases newest first, each with about 80 assets (~180 KB of
/// JSON), so pages stay small and paging stops at the first page with nothing
/// newer than this image: a check's download does not grow with every release.
/// It waits for a whole page, not the first older release, because an old
/// release recreated later sorts first.
const PAGE: usize = 10;
fn wants_next_page(batch: &[Release], installed: &semver::Version) -> bool {
    batch.len() == PAGE && batch.iter().any(|release| release.version().is_some_and(|version| version > *installed))
}
fn choose(releases: Vec<Release>, image: &Image, installed: &semver::Version, channel: Channel) -> Result<Option<Candidate>> {
    let filename = format!("ghazal-{}-{}.raucb", image.device.id, image.memory);
    let mut candidates = Vec::new();
    for release in releases {
        if release.draft || channel == Channel::Stable && release.prerelease {
            continue;
        }
        let Some(version) = release.version() else {
            continue;
        };
        if version <= *installed || channel == Channel::Stable && !version.pre.is_empty() {
            continue;
        }
        if let Some(asset) = release
            .assets
            .into_iter()
            .find(|asset| asset.name == filename)
        {
            // Only public release assets from this repository may be fetched.
            let prefix = format!(
                "https://github.com/sharno/ghazal/releases/download/{}/",
                release.tag_name
            );
            if asset.browser_download_url != format!("{prefix}{filename}")
                || asset.size == 0
                || asset.size > 2 * 1024 * 1024 * 1024
            {
                bail!("invalid release asset");
            }
            candidates.push((
                version,
                Candidate {
                    version: release.tag_name,
                    url: asset.browser_download_url,
                    size: asset.size,
                },
            ));
        }
    }
    Ok(candidates
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, candidate)| candidate))
}
pub fn check(channel: Channel) -> Result<Option<Candidate>> {
    let image = Image::read()?;
    let installed = image.installed()?;
    let mut releases = Vec::new();
    for page in 1..=100 {
        let body = fetch(curl().args([
            "--max-time",
            "60",
            "--max-filesize",
            "4194304",
            "--header",
            "Accept: application/vnd.github+json",
            "--url",
            &format!("https://api.github.com/repos/sharno/ghazal/releases?per_page={PAGE}&page={page}"),
        ]))?;
        let batch: Vec<Release> = serde_json::from_slice(&body).context("GitHub's release list was not understood")?;
        let more = wants_next_page(&batch, &installed);
        releases.extend(batch);
        if !more {
            return choose(releases, &image, &installed, channel);
        }
    }
    bail!("release listing exceeded the pagination limit")
}
fn rauc(args: &[&str]) -> Result<()> {
    let output = Command::new(RAUC)
        .env_remove("LD_LIBRARY_PATH")
        .args(args)
        .output()?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(anyhow::anyhow!(detail).context(format!("rauc {} failed", args.join(" "))));
    }
    Ok(())
}
fn service() -> Result<()> {
    crate::sys::write_atomic(Path::new("/run/ghazal/update-requested"), b"1")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if rauc(&["status"]).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("RAUC service did not become ready");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
struct Guard(fs::File);
impl Guard {
    fn acquire() -> Result<Self> {
        fs::create_dir_all("/run/ghazal")?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(LOCK)?;
        // `busy()` holds the lock for an instant while probing it from the
        // daemon and the menu; only a lock held longer is another update.
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            if lock_now(&file) { return Ok(Self(file)); }
            if Instant::now() >= deadline { bail!("another update is running"); }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.sync_all();
    }
}
fn lock_now(file: &fs::File) -> bool {
    // SAFETY: flock only acts on this live file descriptor.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// One instant probe; the lock is released when the file closes.
pub fn busy() -> bool {
    fs::OpenOptions::new().write(true).open(LOCK).is_ok_and(|file| !lock_now(&file))
}
fn install(candidate: Candidate, events: &Sender<Event>) -> Result<()> {
    let _guard = Guard::acquire()?;
    if Path::new("/run/ghazal/update-installed").exists() {
        bail!("restart before installing another update");
    }
    if crate::emulator::active()? {
        bail!("close the game before updating");
    }
    let battery =
        crate::hw::battery::read(None).context("cannot read battery; cannot safely install")?;
    if !battery.charging && battery.percent < 30 {
        bail!("charge to at least 30% or connect power");
    }
    let image = Image::read()?;
    // The larger data volume holds the bundle; persistent /var remains bounded.
    if !crate::sys::is_mounted(Path::new("/mnt/games")) {
        bail!("data card is not mounted");
    }
    let directory = Path::new("/mnt/games/.ghazal/updates");
    fs::create_dir_all(directory)?;
    // SAFETY: statvfs is a C structure with integer fields and no invalid zero values.
    let mut space: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: the path is NUL-terminated and space is a valid writable structure.
    if unsafe { libc::statvfs(c"/mnt/games".as_ptr(), &mut space) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if (space.f_bavail as u128) * (space.f_frsize as u128)
        < u128::from(candidate.size) + 64 * 1024 * 1024
    {
        bail!("not enough free space on the data card for this update");
    }
    let path = directory.join("pending.raucb");
    let _ = events.send(Event::Status("Downloading update…".into()));
    let result = (|| -> Result<()> {
        fetch(
            curl()
                .args(["--max-time", "1800", "--max-filesize", &candidate.size.to_string(), "--output"])
                .arg(&path)
                .arg("--url")
                .arg(&candidate.url),
        )?;
        if fs::metadata(&path)?.len() != candidate.size {
            bail!("update download incomplete; the installed OS is unchanged");
        }
        fs::File::open(&path)?.sync_all()?;
        let _ = events.send(Event::Status("Verifying update signature…".into()));
        let info = Command::new(RAUC)
            .env_remove("LD_LIBRARY_PATH")
            .args([
                "info",
                "--output-format=json",
                "--keyring",
                "/etc/rauc/release.pem",
            ])
            .arg(&path)
            .output()?;
        if !info.status.success() {
            bail!("update signature verification failed");
        }
        let manifest: serde_json::Value = serde_json::from_slice(&info.stdout)?;
        if manifest["compatible"].as_str() != Some(&image.compatible())
            || manifest["version"].as_str() != Some(&candidate.version)
        {
            bail!("signed update does not match this device or selected release");
        }
        service()?;
        let _ = events.send(Event::Status("Installing inactive OS slot…".into()));
        rauc(&["install", path.to_str().context("invalid bundle path")?])?;
        crate::sys::write_atomic(
            Path::new("/run/ghazal/update-installed"),
            candidate.version.as_bytes(),
        )?;
        Ok(())
    })();
    // Installation failure never makes the inactive slot the selected boot target.
    let removed = fs::remove_file(&path);
    result?;
    removed?;
    Ok(())
}
pub fn spawn_check(channel: Channel) -> Result<Receiver<Event>> {
    spawn(move |events| {
        let candidate = check(channel)?;
        let _ = events.send(Event::Available(candidate));
        Ok(())
    })
}
pub fn spawn_install(candidate: Candidate) -> Result<Receiver<Event>> {
    spawn(move |events| {
        install(candidate, events)?;
        let _ = events.send(Event::Installed);
        Ok(())
    })
}
fn spawn(
    work: impl FnOnce(&Sender<Event>) -> Result<()> + Send + 'static,
) -> Result<Receiver<Event>> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("ghazal-update".into())
        .spawn(move || {
            if let Err(error) = work(&send) {
                eprintln!("ghazal-update: {error:#}");
                // The outermost message only: the screen has one line for it.
                let _ = send.send(Event::Failed(error.to_string()));
            }
        })?;
    Ok(receive)
}

/// The OS slot U-Boot booted, from `rauc.slot=` on the kernel command line.
#[derive(Clone, Copy, Debug)]
enum Slot { A, B }

impl Slot {
    fn booted() -> Result<Self> {
        let cmdline = fs::read_to_string("/proc/cmdline")?;
        match cmdline.split_whitespace().find_map(|word| word.strip_prefix("rauc.slot=")) {
            Some("A") => Ok(Self::A),
            Some("B") => Ok(Self::B),
            Some(_) => bail!("invalid boot slot"),
            None => bail!("not an A/B boot"),
        }
    }
}

impl std::fmt::Display for Slot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self { Self::A => "A", Self::B => "B" })
    }
}

/// Recovery for a missing or full data card: the other slot shares that card,
/// so these boots must not use up A/B attempts. This is what RAUC's mark-good
/// does with the `boot-attempts` in config/updates/system.conf.in.
pub fn keep_booted_slot() -> Result<()> {
    let slot = Slot::booted()?;
    let status = Command::new("/usr/bin/fw_setenv").arg(format!("BOOT_{slot}_LEFT")).arg("3").status()?;
    if !status.success() { bail!("fw_setenv exited with {status}"); }
    Ok(())
}

/// Boot deadline and hardware watchdog protect boots that never reach userspace health.
/// Runs alongside the menu; it never delays the first frame or contacts GitHub.
fn health() -> Result<()> {
    let slot = Slot::booted()?;
    let mut watchdog = fs::OpenOptions::new().write(true).open(WATCHDOG)?;
    PETTED_WATCHDOG.store(watchdog.as_raw_fd(), std::sync::atomic::Ordering::Relaxed);
    // SAFETY: installs a handler that only calls async-signal-safe functions.
    unsafe { libc::signal(libc::SIGTERM, release_watchdog_and_exit as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    let mut timeout: libc::c_int = 16;
    // SAFETY: WDIOC_SETTIMEOUT expects a pointer to an int, which lives through the call.
    if unsafe { libc::ioctl(watchdog.as_raw_fd(), 0xc004_5706u32 as _, &mut timeout) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut confirmation = Confirmation::Waiting;
    loop {
        confirmation = match confirmation {
            Confirmation::Waiting if healthy() => {
                // Starting RAUC can outlast the 16 s timeout: keep petting here.
                Confirmation::Running(std::thread::spawn(|| { service()?; rauc(&["status", "mark-good"]) }))
            }
            Confirmation::Waiting if Instant::now() >= deadline => {
                // Keep the descriptor open without petting; watchdog resets the board.
                eprintln!("ghazal: boot health deadline missed; awaiting watchdog reset");
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            Confirmation::Running(marking) if marking.is_finished() => {
                marking.join().map_err(|_| anyhow::anyhow!("marking the slot good panicked"))??;
                eprintln!("ghazal: OS slot {slot} confirmed healthy");
                Confirmation::Done
            }
            unchanged => unchanged,
        };
        watchdog.write_all(b"K")?;
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Marking the booted slot good happens once, off the petting thread.
enum Confirmation {
    Waiting,
    Running(std::thread::JoinHandle<Result<()>>),
    Done,
}

fn healthy() -> bool {
    Path::new("/run/ghazal/first-frame-ready").exists()
        && Path::new("/run/ghazal/daemon.sock").exists()
        && crate::sys::is_mounted(Path::new("/mnt/games"))
}
pub fn run(args: &[String]) -> Result<()> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["health"] => health(),
        ["check"] => {
            match check(Channel::load()?)? {
                Some(candidate) => println!("Update available: {}", candidate.version),
                None => println!("Already up to date"),
            };
            Ok(())
        }
        ["install"] => {
            let candidate = check(Channel::load()?)?.context("already up to date")?;
            let (send, _) = mpsc::channel();
            install(candidate, &send)?;
            println!("Installed. Restart to use the new version.");
            Ok(())
        }
        _ => bail!("usage: ghazal update check|install|health"),
    }
}

/// UI state owns the candidate only while it can be installed, and the worker
/// only while an operation is running. No contradictory busy/ready flags.
pub struct Menu {
    phase: Phase,
    channel: Channel,
    pub cursor: usize,
    status: String,
}
enum Phase {
    Idle,
    Available(Candidate),
    Working(Receiver<Event>),
    Restart,
}
impl Default for Menu {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            channel: Channel::default(),
            cursor: 0,
            status: "Check for a new release over Wi-Fi".into(),
        }
    }
}
impl Menu {
    pub fn open(&mut self) -> Result<()> {
        self.channel = Channel::load()?;
        Ok(())
    }
    pub fn working(&self) -> bool {
        matches!(self.phase, Phase::Working(_))
    }
    pub fn row(&self, index: usize) -> (String, String) {
        match index {
            0 => ("Release channel".into(), self.channel.label().into()),
            1 => ("Check for updates".into(), String::new()),
            2 => match &self.phase {
                Phase::Available(candidate) => ("Install update".into(), candidate.version.clone()),
                Phase::Restart => ("Restart into updated OS".into(), String::new()),
                _ => ("Install update".into(), "Check first".into()),
            },
            _ => (self.status.clone(), String::new()),
        }
    }
    pub fn activate(&mut self) -> Result<bool> {
        if self.working() {
            return Ok(false);
        }
        match self.cursor {
            0 => {
                let next = self.channel.next();
                next.save()?;
                self.channel = next;
                self.phase = Phase::Idle;
                self.status = "Channel changed; check for updates".into();
            }
            1 if !matches!(self.phase, Phase::Restart) => {
                self.phase = Phase::Working(spawn_check(self.channel)?);
                self.status = "Checking GitHub…".into();
            }
            2 => match &self.phase {
                Phase::Available(candidate) => {
                    self.phase = Phase::Working(spawn_install(candidate.clone())?);
                    self.status = "Preparing update…".into();
                }
                Phase::Restart => return Ok(true),
                _ => {}
            },
            _ => {}
        }
        Ok(false)
    }
    pub fn poll(&mut self) -> bool {
        let Phase::Working(receiver) = &self.phase else {
            return false;
        };
        let mut events = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    let final_event = !matches!(event, Event::Status(_));
                    events.push(event);
                    if final_event {
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    events.push(Event::Failed("Update worker stopped unexpectedly".into()));
                    break;
                }
            }
        }
        let changed = !events.is_empty();
        for event in events {
            match event {
                Event::Status(message) => self.status = message,
                Event::Available(candidate) => {
                    self.status = if candidate.is_some() {
                        "Update available; select Install update".into()
                    } else {
                        "Already up to date".into()
                    };
                    self.phase = candidate.map_or(Phase::Idle, Phase::Available);
                }
                Event::Installed => {
                    self.phase = Phase::Restart;
                    self.status = "Update installed; restart when ready".into();
                }
                Event::Failed(error) => {
                    self.phase = Phase::Idle;
                    self.status = error;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release { tag_name: tag.into(), draft: false, prerelease: true, assets: Vec::new() }
    }

    #[test]
    fn paging_stops_at_a_page_with_nothing_newer() {
        let installed = semver::Version::parse("0.1.0-beta.13").unwrap();
        let page = |tags: &[&str]| tags.iter().copied().cycle().take(PAGE).map(release).collect::<Vec<_>>();
        assert!(wants_next_page(&page(&["v0.1.0-beta.14", "v0.1.0-beta.12"]), &installed));
        assert!(!wants_next_page(&page(&["v0.1.0-beta.13", "v0.1.0-beta.2", "nightly"]), &installed));
        assert!(!wants_next_page(&[release("v0.1.0-beta.14")], &installed));
    }

    #[test]
    fn curl_failures_read_as_one_short_line() {
        let error = fetch(Command::new("sh").args(["-c", "printf 'Warning: retrying\\ncurl: (6) Could not resolve host\\n' >&2; exit 6"])).unwrap_err();
        assert_eq!(error.to_string(), curl_reason(Some(6)));
        assert!(format!("{error:#}").contains("Could not resolve host"));
    }
}
