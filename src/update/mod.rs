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
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
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
        "--retry",
        "2",
        "--user-agent",
        "Ghazal-updater",
    ]);
    command
}
fn choose(releases: Vec<Release>, image: &Image, channel: Channel) -> Result<Option<Candidate>> {
    let installed =
        semver::Version::parse(image.version.strip_prefix('v').unwrap_or(&image.version))
            .context("installed image is not a release version; install a tagged image first")?;
    let filename = format!("ghazal-{}-{}.raucb", image.device.id, image.memory);
    let mut candidates = Vec::new();
    for release in releases {
        if release.draft || channel == Channel::Stable && release.prerelease {
            continue;
        }
        let Ok(version) = semver::Version::parse(
            release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name),
        ) else {
            continue;
        };
        if version <= installed || channel == Channel::Stable && !version.pre.is_empty() {
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
    let mut releases = Vec::new();
    // Pagination prevents older stable releases being hidden by a busy beta channel.
    for page in 1..=10 {
        let output = curl()
            .args([
                "--max-time",
                "60",
                "--max-filesize",
                "4194304",
                "--header",
                "Accept: application/vnd.github+json",
                "--url",
                &format!(
                    "https://api.github.com/repos/sharno/ghazal/releases?per_page=100&page={page}"
                ),
            ])
            .output()?;
        if !output.status.success() {
            bail!(
                "GitHub release check failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let batch: Vec<Release> = serde_json::from_slice(&output.stdout)?;
        let end = batch.len() < 100;
        releases.extend(batch);
        if end {
            return choose(releases, &image, channel);
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
        bail!("RAUC: {}", String::from_utf8_lossy(&output.stderr));
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
        // SAFETY: flock only acts on this live file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("another update is running");
        }
        Ok(Self(file))
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.sync_all();
    }
}
pub fn busy() -> bool {
    if !Path::new(LOCK).exists() {
        return false;
    }
    Guard::acquire().is_err()
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
        let status = curl()
            .args([
                "--max-time",
                "1800",
                "--max-filesize",
                &candidate.size.to_string(),
                "--output",
            ])
            .arg(&path)
            .arg("--url")
            .arg(&candidate.url)
            .status()?;
        if !status.success() || fs::metadata(&path)?.len() != candidate.size {
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
                let _ = send.send(Event::Failed(format!("{error:#}")));
            }
        })?;
    Ok(receive)
}

/// Boot deadline and hardware watchdog protect boots that never reach userspace health.
/// Runs alongside the menu; it never delays the first frame or contacts GitHub.
fn health() -> Result<()> {
    let slot = fs::read_to_string("/proc/cmdline")?
        .split_whitespace()
        .find_map(|word| word.strip_prefix("rauc.slot=").map(str::to_owned))
        .context("not an A/B boot")?;
    if !matches!(slot.as_str(), "A" | "B") {
        bail!("invalid boot slot");
    }
    let mut watchdog = fs::OpenOptions::new().write(true).open("/dev/watchdog0")?;
    let mut timeout: libc::c_int = 16;
    // SAFETY: WDIOC_SETTIMEOUT expects a pointer to an int, which lives through the call.
    if unsafe { libc::ioctl(watchdog.as_raw_fd(), 0xc004_5706u32 as _, &mut timeout) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut confirmed = false;
    loop {
        if !confirmed {
            let healthy = Path::new("/run/ghazal/first-frame-ready").exists()
                && Path::new("/run/ghazal/daemon.sock").exists()
                && crate::sys::is_mounted(Path::new("/mnt/games"));
            if healthy {
                service()?;
                rauc(&["status", "mark-good"])?;
                confirmed = true;
                eprintln!("ghazal: OS slot {slot} confirmed healthy");
            } else if Instant::now() >= deadline {
                // Keep the descriptor open without petting; watchdog resets the board.
                eprintln!("ghazal: boot health deadline missed; awaiting watchdog reset");
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
        watchdog.write_all(b"K")?;
        std::thread::sleep(Duration::from_secs(3));
    }
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
