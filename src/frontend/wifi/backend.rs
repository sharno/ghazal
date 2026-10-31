//! Talking to iwd. `Backend` is what the screen needs; `Iwctl` implements it
//! with the iwctl binary that already ships in the runtime (its D-Bus agent is
//! how a passphrase reaches iwd), and `Exec` is the only place a process starts.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::parse;
use super::types::{ConnectRequest, Failure, Snapshot, Ssid, Visibility};

pub const IWCTL: &str = "/usr/bin/iwctl";

/// Everything blocks: callers run these on a worker thread.
pub trait Backend: Send + Sync {
    fn snapshot(&self) -> Result<Snapshot, Failure>;
    /// Starts a scan; its results show up in later snapshots.
    fn scan(&self) -> Result<(), Failure>;
    /// Joining can take a while: setting `abort` from another thread stops it.
    fn connect(&self, request: &ConnectRequest, abort: &AtomicBool) -> Result<(), Failure>;
    fn disconnect(&self) -> Result<(), Failure>;
    /// Deletes the saved credentials, which also disconnects from the network.
    fn forget(&self, ssid: &Ssid) -> Result<(), Failure>;
}

/// The backend this build should use, if the device has Wi-Fi tooling. The
/// desktop simulator gets a pretend radio so it never touches the host's.
pub fn system_backend() -> Option<Arc<dyn Backend>> {
    #[cfg(feature = "desktop")]
    if std::env::var_os("GHAZAL_WIFI_REAL").is_none() {
        return Some(Arc::new(super::fake::FakeBackend::demo()));
    }
    Path::new(IWCTL).exists().then(|| Arc::new(Iwctl::new(Process::new(IWCTL))) as Arc<dyn Backend>)
}

/// Whether a wireless interface exists: some models have no Wi-Fi chip, and a
/// USB adapter can appear later. cfg80211 gives every wireless device a
/// `phy80211` link, whether or not iwd is running yet.
pub fn adapter_present() -> bool {
    cfg!(feature = "desktop") || std::fs::read_dir("/sys/class/net").is_ok_and(|interfaces| interfaces.flatten().any(|interface| interface.path().join("phy80211").exists()))
}

pub struct Output {
    pub success: bool,
    /// Standard output, then standard error.
    pub text: String,
}

pub trait Exec: Send + Sync {
    /// Runs iwctl to completion, or kills it when `abort` is set or `timeout` passes.
    fn run(&self, args: &[String], timeout: Duration, abort: &AtomicBool) -> Result<Output, Failure>;
}

/// For commands nobody cancels.
static NEVER: AtomicBool = AtomicBool::new(false);

const QUICK: Duration = Duration::from_secs(8);
/// Association, the handshake and iwd's own retries.
const CONNECT: Duration = Duration::from_secs(40);

pub struct Iwctl<E> {
    exec: E,
    /// The station's name, found once (`wlan0` on the handhelds).
    station: Mutex<Option<String>>,
}

impl<E: Exec> Iwctl<E> {
    pub fn new(exec: E) -> Self { Self { exec, station: Mutex::new(None) } }

    /// `--dont-ask` makes a missing credential an error instead of a prompt
    /// nobody could answer. `--` keeps a network named like an option from
    /// being read as one.
    fn run(&self, options: &[String], command: &[&str], timeout: Duration, abort: &AtomicBool) -> Result<String, Failure> {
        let args: Vec<String> = ["--dont-ask".to_owned()].into_iter().chain(options.iter().cloned()).chain(["--".to_owned()]).chain(command.iter().map(|word| (*word).to_owned())).collect();
        check(self.exec.run(&args, timeout, abort)?)
    }

    fn query(&self, command: &[&str]) -> Result<String, Failure> {
        self.run(&[], command, QUICK, &NEVER)
    }

    fn station(&self) -> Result<String, Failure> {
        let mut known = self.station.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(name) = known.as_ref() { return Ok(name.clone()); }
        // With iwd not running, iwctl prints "Waiting for IWD to start..." and
        // blocks until it does, so a silent first command means no service.
        let listing = match self.query(&["station", "list"]) {
            Err(Failure::TimedOut) => return Err(Failure::Unavailable),
            other => other?,
        };
        let name = parse::station_name(&listing)?;
        Ok(known.insert(name).clone())
    }
}

impl<E: Exec> Backend for Iwctl<E> {
    fn snapshot(&self) -> Result<Snapshot, Failure> {
        let station = self.station()?;
        let info = parse::station_info(&self.query(&["station", &station, "show"])?)?;
        let listed = parse::listed_networks(&self.query(&["station", &station, "get-networks", "rssi-dbms"])?)?;
        let saved = parse::saved_networks(&self.query(&["known-networks", "list"])?)?;
        Ok(parse::assemble(info, listed, saved))
    }

    fn scan(&self) -> Result<(), Failure> {
        let station = self.station()?;
        match self.query(&["station", &station, "scan"]) {
            Ok(_) | Err(Failure::Busy) => Ok(()),
            Err(failure) => Err(failure),
        }
    }

    fn connect(&self, request: &ConnectRequest, abort: &AtomicBool) -> Result<(), Failure> {
        let station = self.station()?;
        // iwctl hands the passphrase to iwd when it asks. It is visible in the
        // process list for the seconds this takes, on a device with one user.
        let options: Vec<String> = request.passphrase.iter().map(|passphrase| format!("--passphrase={}", passphrase.expose())).collect();
        // A hidden network has no scan result to tell the security of.
        let (verb, security) = match request.visibility {
            Visibility::Listed => ("connect", request.security.and_then(|security| security.iwctl())),
            Visibility::Hidden => ("connect-hidden", None),
        };
        let command: Vec<&str> = ["station", &station, verb, request.ssid.as_str()].into_iter().chain(security).collect();
        match self.run(&options, &command, CONNECT, abort) {
            Ok(_) | Err(Failure::AlreadyConnected) => Ok(()),
            Err(failure) => Err(failure),
        }
    }

    fn disconnect(&self) -> Result<(), Failure> {
        let station = self.station()?;
        match self.query(&["station", &station, "disconnect"]) {
            Ok(_) | Err(Failure::NotConnected) => Ok(()),
            Err(failure) => Err(failure),
        }
    }

    fn forget(&self, ssid: &Ssid) -> Result<(), Failure> {
        match self.query(&["known-networks", ssid.as_str(), "forget"]) {
            // Already gone is what the caller wanted.
            Ok(_) | Err(Failure::Missing) => Ok(()),
            Err(failure) => Err(failure),
        }
    }
}

/// iwctl's messages for failures, matched on lowercase text. Order matters:
/// the first match wins.
static KNOWN_FAILURES: &[(&str, Failure)] = &[
    ("failed to retrieve iwd dbus objects", Failure::Unavailable),
    ("failed to connect to", Failure::Unavailable),
    ("iwd is not running", Failure::Unavailable),
    ("no devices in station mode", Failure::NoAdapter),
    ("is not in station mode", Failure::NoAdapter),
    ("no station on device", Failure::NoAdapter),
    ("already connected", Failure::AlreadyConnected),
    ("already in progress", Failure::Busy),
    ("operation failed", Failure::Rejected),
    // iwd needs a passphrase and `--dont-ask` gave it no agent to ask: the saved
    // profile is gone or does not fit the network. Same remedy as a refusal.
    ("no agent registered", Failure::Rejected),
    ("operation aborted", Failure::Cancelled),
    ("not connected", Failure::NotConnected),
    ("not supported", Failure::Unsupported),
    ("timed out", Failure::TimedOut),
    ("no network with specified parameters", Failure::Missing),
    ("invalid network name", Failure::Missing),
    ("not found", Failure::Missing),
];

fn classify(text: &str) -> Option<Failure> {
    let plain = parse::strip_ansi(text).to_lowercase();
    // "Device wlan0 not found.": the adapter, not a network.
    if plain.trim_start().starts_with("device ") && plain.contains("not found") { return Some(Failure::NoAdapter); }
    KNOWN_FAILURES.iter().find(|(phrase, _)| plain.contains(phrase)).map(|(_, failure)| failure.clone())
}

/// iwctl's exit status is not a reliable verdict on its own, and a table may
/// legitimately contain words like "failed" (as an SSID), so tables pass first.
fn check(output: Output) -> Result<String, Failure> {
    let plain = parse::strip_ansi(&output.text);
    let is_table = output.success && plain.lines().filter(|line| line.len() >= 20 && line.chars().all(|c| c == '-')).count() >= 2;
    if is_table { return Ok(output.text); }
    match (classify(&output.text), output.success) {
        (Some(failure), _) => Err(failure),
        (None, true) => Ok(output.text),
        (None, false) => Err(Failure::Other(plain.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("Wi-Fi command failed").chars().take(60).collect())),
    }
}

/// Runs the real binary.
pub struct Process {
    binary: PathBuf,
}

impl Process {
    pub fn new(binary: impl Into<PathBuf>) -> Self { Self { binary: binary.into() } }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            // A read error leaves what arrived; the verdict comes from the text.
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

enum Ended {
    Exited(std::process::ExitStatus),
    Aborted,
    TimedOut,
}

impl Exec for Process {
    fn run(&self, args: &[String], timeout: Duration, abort: &AtomicBool) -> Result<Output, Failure> {
        if abort.load(Ordering::SeqCst) { return Err(Failure::Cancelled); }
        let mut child = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Failure::Unavailable,
                _ => Failure::Other(format!("Cannot run iwctl: {error}")),
            })?;
        let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
        let deadline = Instant::now() + timeout;
        let ended = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ended::Exited(status),
                Ok(None) if abort.load(Ordering::SeqCst) => break Ended::Aborted,
                // A child that cannot be waited for is as good as a hung one.
                Ok(None) if Instant::now() >= deadline => break Ended::TimedOut,
                Err(_) => break Ended::TimedOut,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        if !matches!(ended, Ended::Exited(_)) {
            let _ = child.kill();
            let _ = child.wait();
        }
        let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
        match ended {
            Ended::Exited(status) => Ok(Output { success: status.success(), text: stdout + &stderr }),
            Ended::Aborted => Err(Failure::Cancelled),
            Ended::TimedOut => Err(Failure::TimedOut),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse::fixtures::*;
    use super::*;
    use super::super::types::{Link, Passphrase, Security};

    /// Answers each command from a table keyed by the words after `--`, and
    /// remembers every full command line.
    struct Script {
        answers: Vec<(&'static str, Output)>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Script {
        fn new(answers: Vec<(&'static str, &str, bool)>) -> Self {
            Self { answers: answers.into_iter().map(|(command, text, success)| (command, Output { success, text: text.into() })).collect(), calls: Mutex::new(Vec::new()) }
        }
        fn calls(&self) -> Vec<Vec<String>> { self.calls.lock().unwrap().clone() }
    }

    impl Exec for Script {
        fn run(&self, args: &[String], _: Duration, _: &AtomicBool) -> Result<Output, Failure> {
            self.calls.lock().unwrap().push(args.to_vec());
            let command = args.iter().skip_while(|arg| *arg != "--").skip(1).cloned().collect::<Vec<_>>().join(" ");
            self.answers.iter().find(|(wanted, _)| *wanted == command).map(|(_, output)| Output { success: output.success, text: output.text.clone() }).ok_or_else(|| Failure::Other(format!("unscripted command {command}")))
        }
    }

    fn iwctl(answers: Vec<(&'static str, &str, bool)>) -> Iwctl<Script> { Iwctl::new(Script::new(answers)) }

    fn request(ssid: &str, visibility: Visibility, passphrase: Option<&str>) -> ConnectRequest {
        ConnectRequest { ssid: Ssid::new(ssid).unwrap(), visibility, security: Some(Security::Psk), passphrase: passphrase.map(|text| Passphrase::new(text).unwrap()) }
    }

    #[test]
    fn a_snapshot_joins_status_networks_and_saved_networks() {
        let backend = iwctl(vec![
            ("station list", STATION_LIST, true),
            ("station wlan0 show", STATION_SHOW, true),
            ("station wlan0 get-networks rssi-dbms", GET_NETWORKS, true),
            ("known-networks list", KNOWN_NETWORKS, true),
        ]);
        let snapshot = backend.snapshot().unwrap();
        assert!(matches!(snapshot.link, Link::Connected(_)));
        assert_eq!(snapshot.networks.len(), 5);
        // The station is looked up once, then reused.
        backend.snapshot().unwrap();
        assert_eq!(backend.exec.calls().iter().filter(|call| call.ends_with(&["station".into(), "list".into()])).count(), 1);
    }

    #[test]
    fn a_missing_daemon_or_adapter_is_fatal() {
        let down = iwctl(vec![("station list", "Failed to retrieve IWD dbus objects, quitting...\n", false)]);
        assert_eq!(down.snapshot(), Err(Failure::Unavailable));
        let none = iwctl(vec![("station list", "No devices in Station mode available.\n", true)]);
        assert_eq!(none.snapshot(), Err(Failure::NoAdapter));
    }

    #[test]
    fn a_passphrase_goes_in_its_own_option_before_the_command() {
        let backend = iwctl(vec![("station list", STATION_LIST, true), ("station wlan0 connect Home psk", "", true)]);
        backend.connect(&request("Home", Visibility::Listed, Some("-hunter2hunter2")), &NEVER).unwrap();
        let call = backend.exec.calls().pop().unwrap();
        assert_eq!(call, ["--dont-ask", "--passphrase=-hunter2hunter2", "--", "station", "wlan0", "connect", "Home", "psk"]);
    }

    #[test]
    fn a_hidden_network_uses_connect_hidden_without_a_security_word() {
        let backend = iwctl(vec![("station list", STATION_LIST, true), ("station wlan0 connect-hidden --odd name", "", true)]);
        backend.connect(&request("--odd name", Visibility::Hidden, None), &NEVER).unwrap();
        assert_eq!(backend.exec.calls().pop().unwrap(), ["--dont-ask", "--", "station", "wlan0", "connect-hidden", "--odd name"]);
    }

    #[test]
    fn iwds_errors_become_failures_whatever_the_exit_status() {
        for (text, success, expected) in [
            ("Operation failed\n", true, Failure::Rejected),
            ("Operation failed\n", false, Failure::Rejected),
            ("No Agent registered\n", false, Failure::Rejected),
            ("\x1b[1;31mOperation aborted\x1b[0m\n", false, Failure::Cancelled),
            ("No network with specified parameters was found\n", false, Failure::Missing),
            ("Invalid network name 'Gone'\n", false, Failure::Missing),
            ("Operation already in progress\n", false, Failure::Busy),
            ("garbled", false, Failure::Other("garbled".into())),
        ] {
            let backend = iwctl(vec![("station list", STATION_LIST, true), ("station wlan0 connect Home psk", text, success)]);
            assert_eq!(backend.connect(&request("Home", Visibility::Listed, None), &NEVER), Err(expected), "{text:?}");
        }
    }

    #[test]
    fn harmless_outcomes_are_successes() {
        let backend = iwctl(vec![
            ("station list", STATION_LIST, true),
            ("station wlan0 connect Home psk", "Already connected\n", false),
            ("station wlan0 scan", "Operation already in progress\n", false),
            ("station wlan0 disconnect", "Not connected\n", false),
            ("known-networks Gone forget", "Invalid network name 'Gone'\n", false),
        ]);
        assert_eq!(backend.connect(&request("Home", Visibility::Listed, None), &NEVER), Ok(()));
        assert_eq!(backend.scan(), Ok(()));
        assert_eq!(backend.disconnect(), Ok(()));
        assert_eq!(backend.forget(&Ssid::new("Gone").unwrap()), Ok(()));
    }

    #[test]
    fn a_table_naming_a_network_after_an_error_is_still_a_table() {
        let table = "  Available networks\n--------------------------------------------------------------------------------\n      Network name                      Security            Signal\n--------------------------------------------------------------------------------\n      Operation failed                  psk                 -5000   \n";
        let backend = iwctl(vec![("station list", STATION_LIST, true), ("station wlan0 show", STATION_SHOW, true), ("station wlan0 get-networks rssi-dbms", table, true), ("known-networks list", "No known networks available.\n", true)]);
        assert!(backend.snapshot().unwrap().networks.iter().any(|n| n.ssid.as_str() == "Operation failed"));
    }

    #[test]
    fn forgetting_names_the_network_as_one_argument() {
        let backend = iwctl(vec![("known-networks Grandma's house forget", "", true)]);
        backend.forget(&Ssid::new("Grandma's house").unwrap()).unwrap();
        assert_eq!(backend.exec.calls().pop().unwrap(), ["--dont-ask", "--", "known-networks", "Grandma's house", "forget"]);
    }

    #[test]
    fn the_real_process_reports_output_failure_and_missing_binaries() {
        let run = |binary: &str, args: &[&str]| Process::new(binary).run(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(), Duration::from_secs(5), &NEVER);
        let ok = run("/bin/sh", &["-c", "echo out; echo err >&2"]).unwrap();
        assert!(ok.success);
        assert_eq!((ok.text.contains("out"), ok.text.contains("err")), (true, true));
        assert!(!run("/bin/sh", &["-c", "exit 3"]).unwrap().success);
        assert_eq!(run("/nonexistent/iwctl", &[]).err(), Some(Failure::Unavailable));
    }

    #[test]
    fn the_real_process_times_out_and_can_be_aborted() {
        let sleep = vec!["-c".to_owned(), "sleep 5".to_owned()];
        let started = Instant::now();
        assert_eq!(Process::new("/bin/sh").run(&sleep, Duration::from_millis(100), &NEVER).err(), Some(Failure::TimedOut));

        let abort = Arc::new(AtomicBool::new(false));
        let setter = Arc::clone(&abort);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            setter.store(true, Ordering::SeqCst);
        });
        assert_eq!(Process::new("/bin/sh").run(&sleep, Duration::from_secs(10), &abort).err(), Some(Failure::Cancelled));
        handle.join().unwrap();
        // Already aborted: nothing is started at all.
        assert_eq!(Process::new("/nonexistent/iwctl").run(&[], Duration::from_secs(1), &abort).err(), Some(Failure::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
