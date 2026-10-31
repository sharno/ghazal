use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const LIMIT: u64 = 4 * 1024 * 1024;

struct RollingLog {
    path: PathBuf,
    file: File,
    size: u64,
}

impl RollingLog {
    fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().context("log path has no parent")?)?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata()?.len();
        let mut log = Self { path: path.to_owned(), file, size };
        if size >= LIMIT { log.rotate()?; }
        Ok(log)
    }

    fn rotate(&mut self) -> Result<()> {
        let mut source = File::open(&self.path)?;
        let size = source.metadata()?.len();
        source.seek(SeekFrom::Start(size.saturating_sub(LIMIT)))?;
        let mut previous = File::create(self.path.with_extension("log.previous"))?;
        std::io::copy(&mut source.take(LIMIT), &mut previous)?;
        previous.sync_all()?;
        self.file = OpenOptions::new().write(true).truncate(true).open(&self.path)?;
        self.size = 0;
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if self.size + bytes.len() as u64 > LIMIT { self.rotate()?; }
        self.file.write_all(bytes)?;
        self.size += bytes.len() as u64;
        // Preserve diagnostic records even if the board resets immediately.
        self.file.sync_data()?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct LogPump {
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<EmulatorReport>>,
}

#[derive(Default)]
pub struct EmulatorReport {
    pub error: Option<String>,
    pub save_warning: Option<String>,
    /// Why a Moonlight stream ended without the host closing it.
    pub stream_ended: Option<String>,
}

impl LogPump {
    /// A log that can't be opened or written (a full /var) is dropped, never
    /// the pipe: the program writing to it would die of EPIPE or SIGPIPE.
    pub fn open(path: &Path) -> Result<(std::os::fd::OwnedFd, Self)> {
        let mut log = RollingLog::open(path)
            .inspect_err(|error| crate::sys::log(format_args!("ghazal: discarding output for {}: {error:#}", path.display())))
            .ok();
        let (mut reader, writer) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let stopping = Arc::new(AtomicBool::new(false));
        let requested = stopping.clone();
        let thread = std::thread::Builder::new().name("ghazal-log".into()).spawn(move || {
            let mut bytes = [0; 16384];
            let mut diagnostic = Diagnostics::default();
            let result = (|| -> Result<()> {
                let mut drain_deadline = None;
                loop {
                    if requested.load(Ordering::Acquire) {
                        let deadline = drain_deadline.get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
                        if Instant::now() >= *deadline { return Ok(()); }
                    }
                    match reader.read(&mut bytes) {
                        Ok(0) => return Ok(()),
                        Ok(size) => {
                            diagnostic.feed(&bytes[..size]);
                            if let Some(Err(error)) = log.as_mut().map(|log| log.append(&bytes[..size])) {
                                crate::sys::log(format_args!("ghazal: log writer failed, discarding further output: {error:#}"));
                                log = None;
                            }
                        }
                        Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                            if requested.load(Ordering::Acquire) { return Ok(()); }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
                        Err(error) => return Err(error.into()),
                    }
                }
            })();
            if let Err(error) = result { crate::sys::log(format_args!("ghazal: log writer failed: {error:#}")); }
            EmulatorReport { save_warning: diagnostic.save_warning.take(), stream_ended: diagnostic.stream_ended.take(), error: diagnostic.finish() }
        })?;
        Ok((writer.into(), Self { stopping, thread: Some(thread) }))
    }

    /// Asks the worker to drain for at most 500 ms more, then close the log.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    pub fn finish(mut self) -> Result<EmulatorReport> {
        self.stop();
        self.thread.take().context("log worker already finished")?.join()
            .map_err(|_| anyhow::anyhow!("log worker panicked"))
    }
}

#[derive(Default)]
struct Diagnostics {
    partial: Vec<u8>,
    last: Option<(DiagnosticSource, String)>,
    save_warning: Option<String>,
    stream_ended: Option<String>,
}

/// What `moonlight` prints (stderr) when it cannot start a stream.
const MOONLIGHT_FAILURES: &[&str] = &[
    "Can't connect to server", "Can't find app", "Can't get app list", "Gamestream error", "You must pair with the PC first",
    "Server doesn't support", "Unsupported version", "Invalid data received", "Couldn't initialize video decoding",
    "Couldn't find decoder", "SDL_CreateRenderer failed", "SDL: could not", "Could not initialize SDL",
];

/// What it prints (stdout) when a running stream is cut. A graceful end,
/// closed from the host, is not one.
const MOONLIGHT_CUT: &[&str] = &[
    "Connection terminated with error", "No video received from host", "Your network connection isn't performing well",
    "The connection was unexpectedly terminated", "The connection was terminated by the host",
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DiagnosticSource { Content, Frontend, Core }

impl Diagnostics {
    fn feed(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if *byte == b'\n' {
                self.line();
            } else if self.partial.len() < 16384 {
                self.partial.push(*byte);
            }
        }
    }

    fn line(&mut self) {
        let line = String::from_utf8_lossy(&self.partial);
        if line.contains("[State] Auto save state to") && line.trim_end().ends_with("failed.") {
            self.save_warning = Some("Automatic save state failed. This core may not support save states; progress cannot automatically resume.".into());
        }
        if MOONLIGHT_CUT.iter().any(|prefix| line.starts_with(prefix)) {
            self.stream_ended = Some(line.trim_end().chars().take(500).collect());
        }
        let message = if MOONLIGHT_FAILURES.iter().any(|prefix| line.starts_with(prefix)) {
            Some((DiagnosticSource::Frontend, line.trim()))
        } else if let Some((_, body)) = line.split_once("[libretro ERROR]") {
            Some((DiagnosticSource::Core, body.trim()))
        } else if let Some((_, body)) = line.split_once("[Environ] SET_MESSAGE:") {
            let lower = body.to_ascii_lowercase();
            ["error", "not found", "missing", "failed", "required"].iter().any(|word| lower.contains(word)).then_some((DiagnosticSource::Core, body.trim()))
        } else if line.contains("[ERROR]") && !line.contains("GET_VARIABLE") {
            Some((if line.contains("[Content] Failed to load") { DiagnosticSource::Content } else { DiagnosticSource::Frontend }, line.trim()))
        } else { None };
        if let Some((priority, message)) = message {
            if self.last.as_ref().is_none_or(|(previous, _)| priority >= *previous) {
                self.last = Some((priority, message.chars().take(500).collect()));
            }
        }
        self.partial.clear();
    }

    fn finish(mut self) -> Option<String> {
        self.line();
        self.last.map(|(_, text)| text)
    }
}

impl Drop for LogPump {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_survives_generic_failure_and_split_records() {
        let mut capture = Diagnostics::default();
        capture.feed(b"[libretro ERROR] Missing BI");
        capture.feed(b"OS\n[ERROR] [Content] Failed to load content.\n");
        assert_eq!(capture.finish().as_deref(), Some("Missing BIOS"));
    }

    #[test]
    fn firmware_message_is_kept_and_unrelated_core_options_are_ignored() {
        let mut capture = Diagnostics::default();
        capture.feed(b"[INFO] [Environ] SET_MESSAGE: Firmware not found: syscard3.pce\n[ERROR] [Environ] GET_VARIABLE: invalid value\n");
        assert_eq!(capture.finish().as_deref(), Some("Firmware not found: syscard3.pce"));
    }

    #[test]
    fn moonlight_failures_and_cut_streams_are_reported_but_a_graceful_end_is_not() {
        let mut capture = Diagnostics::default();
        capture.feed(b"Connecting to 10.0.0.2...\nCan't connect to server 10.0.0.2\n");
        assert_eq!(capture.finish().as_deref(), Some("Can't connect to server 10.0.0.2"));
        let mut capture = Diagnostics::default();
        capture.feed(b"Connection is okay\nYour network connection isn't performing well. Reduce your video bitrate setting or try a faster connection.\n");
        assert!(capture.stream_ended.as_deref().is_some_and(|text| text.starts_with("Your network connection")));
        let mut capture = Diagnostics::default();
        capture.feed(b"Connection has been terminated gracefully.\n");
        assert!(capture.stream_ended.is_none() && capture.finish().is_none());
    }

    #[test]
    fn binary_and_unterminated_records_are_bounded() {
        let mut capture = Diagnostics::default();
        capture.feed(&[0xff; 65536]);
        assert_eq!(capture.partial.len(), 16384);
        capture.feed(b"\n[libretro ERROR] ");
        capture.feed(&[b'x'; 65536]);
        assert_eq!(capture.finish().unwrap().len(), 500);
    }

    #[test]
    fn rotation_bounds_current_previous_and_legacy_files() {
        let directory = std::env::temp_dir().join(format!("ghazal-log-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("service.log");
        {
            let mut log = RollingLog::open(&path).unwrap();
            let chunk = [b'x'; 16384];
            for _ in 0..=LIMIT / chunk.len() as u64 { log.append(&chunk).unwrap(); }
        }
        assert!(fs::metadata(&path).unwrap().len() <= LIMIT);
        assert_eq!(fs::metadata(path.with_extension("log.previous")).unwrap().len(), LIMIT);
        File::create(&path).unwrap().set_len(LIMIT * 3).unwrap();
        drop(RollingLog::open(&path).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(fs::metadata(path.with_extension("log.previous")).unwrap().len(), LIMIT);
        fs::remove_dir_all(directory).unwrap();
    }
}
