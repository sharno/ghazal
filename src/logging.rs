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
    thread: Option<JoinHandle<()>>,
}

impl LogPump {
    pub fn open(path: &Path) -> Result<(std::os::fd::OwnedFd, Self)> {
        let mut log = RollingLog::open(path)?;
        let (mut reader, writer) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let stopping = Arc::new(AtomicBool::new(false));
        let requested = stopping.clone();
        let thread = std::thread::Builder::new().name("oxmux-log".into()).spawn(move || {
            let mut bytes = [0; 16384];
            let result = (|| -> Result<()> {
                let mut drain_deadline = None;
                loop {
                    if requested.load(Ordering::Acquire) {
                        let deadline = drain_deadline.get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
                        if Instant::now() >= *deadline { return Ok(()); }
                    }
                    match reader.read(&mut bytes) {
                        Ok(0) => return Ok(()),
                        Ok(size) => log.append(&bytes[..size])?,
                        Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                            if requested.load(Ordering::Acquire) { return Ok(()); }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
                        Err(error) => return Err(error.into()),
                    }
                }
            })();
            if let Err(error) = result { eprintln!("oxmux: log writer failed: {error:#}"); }
        })?;
        Ok((writer.into(), Self { stopping, thread: Some(thread) }))
    }
}

impl Drop for LogPump {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}
