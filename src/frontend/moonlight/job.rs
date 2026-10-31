//! Work that must not block the menu: Moonlight's pair, list, unpair and quit
//! helpers (each talks to the host over the network) and the mDNS browse.
//! A job runs on its own thread; dropping it cancels it and kills its process.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::discover::{self, Found};

/// How a helper process ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    /// Its exit code, or `None` when a signal killed it.
    Exited(Option<i32>),
    TimedOut,
    CouldNotStart(String),
}

/// A finished helper: how it ended and everything it printed.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    pub ended: Ended,
    pub output: String,
}

pub enum Poll<T> {
    Pending,
    Ready(T),
    /// The worker died without a result.
    Lost,
}

pub struct Job<T> {
    results: Receiver<T>,
    cancel: Arc<AtomicBool>,
}

impl<T> Job<T> {
    pub fn poll(&self) -> Poll<T> {
        match self.results.try_recv() {
            Ok(result) => Poll::Ready(result),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn spawn<T: Send + 'static>(name: &str, work: impl FnOnce(&AtomicBool) -> T + Send + 'static) -> Job<T> {
    let (sender, results) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    // A thread that cannot start leaves the channel closed: the job reads as lost.
    let _ = std::thread::Builder::new().name(name.into()).spawn(move || { let _ = sender.send(work(&flag)); });
    Job { results, cancel }
}

/// Runs the helper to completion, or kills it at `limit` or when cancelled.
pub fn run(program: &Path, args: Vec<OsString>, env: &BTreeMap<String, String>, limit: Duration) -> Job<Captured> {
    let (program, env) = (program.to_owned(), env.clone());
    spawn("ghazal-moonlight", move |cancel| capture(&program, &args, &env, limit, cancel))
}

fn capture(program: &Path, args: &[OsString], env: &BTreeMap<String, String>, limit: Duration, cancel: &AtomicBool) -> Captured {
    let mut command = Command::new(program);
    command.args(args).envs(env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe { command.pre_exec(|| { crate::sys::reset_signal_mask(); Ok(()) }); }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Captured { ended: Ended::CouldNotStart(format!("{}: {error}", program.display())), output: String::new() },
    };
    let (drained, collected) = channel();
    let pipes = [child.stdout.take().map(|pipe| drain(pipe, drained.clone())), child.stderr.take().map(|pipe| drain(pipe, drained.clone()))]
        .into_iter().flatten().count();
    drop(drained);
    let deadline = Instant::now() + limit;
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ended::Exited(status.code()),
            Ok(None) if cancel.load(Ordering::Acquire) => { let _ = child.kill(); let _ = child.wait(); break Ended::Exited(None) }
            Ok(None) if Instant::now() >= deadline => { let _ = child.kill(); let _ = child.wait(); break Ended::TimedOut }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => break Ended::CouldNotStart(format!("waiting for {}: {error}", program.display())),
        }
    };
    // A helper's own children could keep a pipe open: wait for each only briefly.
    let output = (0..pipes)
        .filter_map(|_| collected.recv_timeout(Duration::from_millis(500)).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    Captured { ended, output }
}

/// Reads a helper's pipe to its end on a thread, so a chatty helper never
/// blocks on a full pipe.
fn drain(mut pipe: impl Read + Send + 'static, into: Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = into.send(bytes);
    });
}

/// Browses the LAN for GameStream hosts.
pub fn scan(window: Duration) -> Job<Result<Vec<Found>, String>> {
    spawn("ghazal-mdns", move |_| discover::browse(window))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait<T>(job: &Job<T>) -> Poll<T> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match job.poll() {
                Poll::Pending if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                other => return other,
            }
        }
    }

    fn sh(script: &str, limit: Duration) -> Job<Captured> {
        run(Path::new("/bin/sh"), vec!["-c".into(), script.into(), "helper".into()], &BTreeMap::new(), limit)
    }

    #[test]
    fn output_and_exit_code_are_captured() {
        let Poll::Ready(captured) = wait(&sh("echo out; echo err >&2; exit 3", Duration::from_secs(5))) else { panic!("no result") };
        assert_eq!(captured.ended, Ended::Exited(Some(3)));
        assert!(captured.output.contains("out") && captured.output.contains("err"));
    }

    #[test]
    fn a_helper_that_outlasts_its_limit_is_killed() {
        let started = Instant::now();
        let Poll::Ready(captured) = wait(&sh("echo started; exec sleep 30", Duration::from_millis(200))) else { panic!("no result") };
        assert_eq!(captured.ended, Ended::TimedOut);
        assert!(captured.output.contains("started") && started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn dropping_a_job_kills_its_process() {
        let marker = std::env::temp_dir().join(format!("ghazal-job-{}", std::process::id()));
        let script = format!("echo $$ > {}; exec sleep 30", marker.display());
        let job = sh(&script, Duration::from_secs(60));
        let until = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < until { std::thread::sleep(Duration::from_millis(10)); }
        let pid: i32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        drop(job);
        let until = Instant::now() + Duration::from_secs(10);
        // SAFETY: signal 0 only checks that the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < until { std::thread::sleep(Duration::from_millis(10)); }
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "helper still running");
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn a_missing_program_is_reported() {
        let job = run(Path::new("/nonexistent/moonlight"), vec![], &BTreeMap::new(), Duration::from_secs(1));
        let Poll::Ready(captured) = wait(&job) else { panic!("no result") };
        assert!(matches!(captured.ended, Ended::CouldNotStart(message) if message.contains("/nonexistent/moonlight")));
    }

    #[test]
    fn the_environment_reaches_the_helper() {
        let env = BTreeMap::from([("MOONLIGHT_TEST".to_owned(), "yes".to_owned())]);
        let job = run(Path::new("/bin/sh"), vec!["-c".into(), "echo $MOONLIGHT_TEST".into()], &env, Duration::from_secs(5));
        let Poll::Ready(captured) = wait(&job) else { panic!("no result") };
        assert_eq!(captured.output.trim(), "yes");
    }
}
