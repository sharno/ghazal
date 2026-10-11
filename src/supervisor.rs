//! Declarative service supervisor. Replaces muOS's pidfile scripts (script/var/process.sh)
//! and the frontend restart loop (script/mux/frontend.sh).
//!
//! Services are started once their `after` dependencies are up (oneshots: exited 0,
//! long-running: started) and their `wait_for` paths exist. Crashed services restart with
//! exponential backoff; a service that crash-loops is replaced by its `fallback` command.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::sys;

const CRASH_WINDOW: Duration = Duration::from_secs(60);
const CRASH_LIMIT: usize = 5;
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,
    pub exec: Vec<String>,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub restart: Restart,
    /// Services that must be up (or, for oneshots, finished successfully) first.
    #[serde(default)]
    pub after: Vec<String>,
    /// Paths that must exist before starting (e.g. a "storage mounted" marker).
    #[serde(default)]
    pub wait_for: Vec<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Run instead once the service crash-loops (5 failures within a minute).
    pub fallback: Option<Vec<String>>,
    /// stdout/stderr go here; defaults to `<log_dir>/<name>.log`.
    pub log: Option<PathBuf>,
    /// A missing executable is logged and skipped instead of blocking dependents.
    #[serde(default)]
    pub optional: bool,
}
#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Long-running process.
    #[default]
    Simple,
    /// Runs to completion; dependents start after it exits successfully.
    Oneshot,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    Always,
    #[default]
    OnFailure,
    Never,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// Processes (by name, as in /proc/<pid>/comm) to kill before starting, e.g. muOS
    /// daemons that oxmux replaces.
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(rename = "service", default)]
    pub services: Vec<ServiceConfig>,
}

fn default_log_dir() -> PathBuf {
    "/run/oxmux/log".into()
}

#[derive(Debug)]
enum State {
    Pending,
    Running { pid: i32, _log: crate::logging::LogPump },
    /// Oneshot finished successfully.
    Done,
    /// Waiting to restart.
    Backoff { until: Instant },
    /// Won't be started again (failed oneshot, restart=never, missing optional binary).
    Dead,
}

struct Service {
    cfg: ServiceConfig,
    state: State,
    crashes: Vec<Instant>,
    command: CommandChoice,
}

enum CommandChoice {
    Primary,
    Fallback(Vec<String>),
}

#[derive(Debug, PartialEq)]
enum ExitDecision { Complete, Dead, Restart }

fn exit_decision(kind: Kind, restart: Restart, success: bool) -> ExitDecision {
    match (kind, success, restart) {
        (Kind::Oneshot, true, _) => ExitDecision::Complete,
        (Kind::Oneshot, false, _) => ExitDecision::Dead,
        (Kind::Simple, _, Restart::Never) | (Kind::Simple, true, Restart::OnFailure) => ExitDecision::Dead,
        (Kind::Simple, _, _) => ExitDecision::Restart,
    }
}

fn blocked_dependency(services: &[Service], index: usize) -> Option<&str> {
    services[index].cfg.after.iter().find(|dependency| {
        services.iter().any(|service| &service.cfg.name == *dependency && matches!(service.state, State::Dead) && !service.cfg.optional)
    }).map(String::as_str)
}

/// Why the supervisor loop stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stop {
    Poweroff,
    Reboot,
    Halt,
    /// SIGINT (e.g. Ctrl+C when run by hand).
    Exit,
}

pub struct Supervisor {
    log_dir: PathBuf,
    services: Vec<Service>,
    /// Order services were started in, for reverse-order shutdown.
    started: Vec<usize>,
    signals: sys::SignalFd,
    /// When PID 1, reap every orphan, not just our children.
    pid1: bool,
}

impl SupervisorConfig {
    pub fn validate(&self) -> Result<()> {
        let cfg = self;
        let mut names = std::collections::HashSet::new();
        for s in &cfg.services {
            if !names.insert(s.name.as_str()) {
                bail!("duplicate service {:?}", s.name);
            }
            if s.exec.is_empty() {
                bail!("service {:?} has an empty exec", s.name);
            }
            if s.fallback.as_ref().is_some_and(|command| command.is_empty()) {
                bail!("service {:?} has an empty fallback", s.name);
            }
        }
        for service in &cfg.services {
            for dependency in &service.after {
                if !names.contains(dependency.as_str()) {
                    bail!("service {:?} is after unknown service {dependency:?}", service.name);
                }
            }
        }
        let mut ordered = std::collections::HashSet::new();
        loop {
            let before = ordered.len();
            for service in &cfg.services {
                if service.after.iter().all(|dependency| ordered.contains(dependency.as_str())) {
                    ordered.insert(service.name.as_str());
                }
            }
            if ordered.len() == cfg.services.len() { break; }
            if before == ordered.len() { bail!("service dependency graph contains a cycle"); }
        }
        Ok(())
    }
}

impl Supervisor {
    pub fn new(cfg: &SupervisorConfig, pid1: bool) -> Result<Self> {
        cfg.validate()?;
        std::fs::create_dir_all(&cfg.log_dir).with_context(|| format!("creating {}", cfg.log_dir.display()))?;
        for name in &cfg.replaces {
            sys::kill_by_name(name);
        }
        Ok(Self {
            log_dir: cfg.log_dir.clone(),
            services: cfg
                .services
                .iter()
                .map(|c| Service { cfg: c.clone(), state: State::Pending, crashes: Vec::new(), command: CommandChoice::Primary })
                .collect(),
            started: Vec::new(),
            signals: sys::SignalFd::new()?,
            pid1,
        })
    }

    /// Runs until a shutdown signal arrives, then stops all services in reverse order.
    /// SIGUSR2 = poweroff, SIGTERM = reboot, SIGUSR1 = halt (busybox init conventions).
    pub fn run(&mut self) -> Result<Stop> {
        log(format_args!("supervising {} services", self.services.len()));
        let stop = loop {
            self.start_ready();
            let timeout = self.next_timeout();
            match self.signals.wait(timeout)? {
                Some(libc::SIGCHLD) | None => {}
                Some(libc::SIGUSR2) => break Stop::Poweroff,
                Some(libc::SIGTERM) => break Stop::Reboot,
                Some(libc::SIGUSR1) => break Stop::Halt,
                Some(libc::SIGINT) => break Stop::Exit,
                Some(_) => {}
            }
            self.reap();
        };
        log(format_args!("stopping services ({stop:?})"));
        self.stop_all();
        Ok(stop)
    }

    fn start_ready(&mut self) {
        let now = Instant::now();
        loop {
            let mut changed = false;
            for index in 0..self.services.len() {
                if !matches!(self.services[index].state, State::Pending | State::Backoff { .. }) { continue; }
                let failed = blocked_dependency(&self.services, index).map(str::to_string);
                if let Some(dependency) = failed {
                    log(format_args!("{} cannot start: required dependency {dependency} is dead", self.services[index].cfg.name));
                    self.services[index].state = State::Dead;
                    changed = true;
                }
            }
            for i in 0..self.services.len() {
                let ready = match &self.services[i].state {
                    State::Pending => self.deps_met(i) && self.services[i].cfg.wait_for.iter().all(|p| p.exists()),
                    State::Backoff { until } => now >= *until && self.deps_met(i),
                    _ => false,
                };
                if ready {
                    self.spawn(i);
                    changed = true;
                }
            }
            // Spawn failures and optional services can unblock dependents in
            // either declaration order. Resolve the graph before sleeping.
            if !changed { break; }
        }
    }

    fn deps_met(&self, i: usize) -> bool {
        self.services[i].cfg.after.iter().all(|dep| {
            let d = self.services.iter().find(|s| &s.cfg.name == dep).expect("validated in new()");
            match (d.cfg.kind, &d.state) {
                (Kind::Oneshot, State::Done) => true,
                (Kind::Simple, State::Running { .. }) => true,
                // A dead optional dependency doesn't block dependents.
                (_, State::Dead) => d.cfg.optional,
                _ => false,
            }
        })
    }

    fn spawn(&mut self, i: usize) {
        let svc = &mut self.services[i];
        let argv = match &svc.command {
            CommandChoice::Fallback(fallback) => fallback.clone(),
            CommandChoice::Primary => svc.cfg.exec.clone(),
        };
        let log_path = svc.cfg.log.clone().unwrap_or_else(|| self.log_dir.join(format!("{}.log", svc.cfg.name)));

        let result = (|| -> Result<(i32, crate::logging::LogPump)> {
            let (log, pump) = crate::logging::LogPump::open(&log_path)?;
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..]).envs(&svc.cfg.env).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
            // SAFETY: only async-signal-safe calls between fork and exec.
            unsafe {
                cmd.pre_exec(|| {
                    sys::reset_signal_mask();
                    if libc::setsid() < 0 { return Err(std::io::Error::last_os_error()); }
                    Ok(())
                });
            }
            let child = cmd.spawn().with_context(|| format!("starting {:?}", argv[0]))?;
            Ok((child.id() as i32, pump))
        })();

        match result {
            Ok((pid, pump)) => {
                log(format_args!("started {} (pid {pid})", svc.cfg.name));
                svc.state = State::Running { pid, _log: pump };
                self.started.retain(|&s| s != i);
                self.started.push(i);
            }
            Err(e) if svc.cfg.optional && !Path::new(&argv[0]).exists() => {
                log(format_args!("skipping optional {}: {e:#}", svc.cfg.name));
                svc.state = State::Dead;
            }
            Err(e) => {
                log(format_args!("{}: {e:#}", svc.cfg.name));
                self.failed(i);
            }
        }
    }

    fn reap(&mut self) {
        while let Some((pid, status)) = sys::reap_any() {
            let Some(i) = self.services.iter().position(|s| matches!(s.state, State::Running { pid: running, .. } if running == pid)) else {
                if !self.pid1 {
                    log(format_args!("reaped unknown pid {pid}"));
                }
                continue;
            };
            let svc = &mut self.services[i];
            // A service owns its session, including any helper processes. Do
            // not leave helpers behind when restarting a departed leader.
            sys::kill(-pid, libc::SIGKILL);
            let ok = status == 0;
            log(format_args!("{} exited with status {status}", svc.cfg.name));
            match exit_decision(svc.cfg.kind, svc.cfg.restart, ok) {
                ExitDecision::Complete => svc.state = State::Done,
                ExitDecision::Dead => svc.state = State::Dead,
                ExitDecision::Restart => self.failed(i),
            }
        }
    }

    fn failed(&mut self, i: usize) {
        self.services[i].failed(Instant::now());
    }

    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut timeout: Option<Duration> = None;
        for (i, s) in self.services.iter().enumerate() {
            let t = match &s.state {
                State::Backoff { until } if self.deps_met(i) => until.saturating_duration_since(now),
                State::Pending if self.deps_met(i) => Duration::from_millis(250),
                _ => continue,
            };
            timeout = Some(timeout.map_or(t, |x| x.min(t)));
        }
        timeout
    }

    fn stop_all(&mut self) {
        for &i in self.started.iter().rev() {
            if let State::Running { pid, .. } = self.services[i].state {
                sys::kill(-pid, libc::SIGTERM);
            }
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while self.services.iter().any(|s| matches!(s.state, State::Running { .. })) && Instant::now() < deadline {
            let _ = self.signals.wait(Some(Duration::from_millis(100)));
            while let Some((pid, _)) = sys::reap_any() {
                if let Some(s) = self.services.iter_mut().find(|s| matches!(s.state, State::Running { pid: running, .. } if running == pid)) {
                    sys::kill(-pid, libc::SIGKILL);
                    s.state = State::Dead;
                }
            }
        }
        for s in &mut self.services {
            if let State::Running { pid, .. } = s.state {
                log(format_args!("killing {} (pid {pid})", s.cfg.name));
                sys::kill(-pid, libc::SIGKILL);
                s.state = State::Dead;
            }
        }
        while sys::reap_any().is_some() {}
    }
}

impl Service {
    fn failed(&mut self, now: Instant) {
        let svc = self;
        svc.crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        svc.crashes.push(now);
        if svc.cfg.kind == Kind::Oneshot || svc.cfg.restart == Restart::Never {
            svc.state = State::Dead;
            return;
        }
        if svc.crashes.len() >= CRASH_LIMIT && matches!(svc.command, CommandChoice::Primary) {
            if let Some(fallback) = &svc.cfg.fallback {
                log(format_args!("{} is crash-looping; switching to its fallback", svc.cfg.name));
                svc.command = CommandChoice::Fallback(fallback.clone());
                svc.crashes.clear();
                svc.state = State::Backoff { until: now };
                return;
            }
        }
        let exp = svc.crashes.len().saturating_sub(1).min(5) as u32;
        let delay = (Duration::from_secs(1) * 2u32.pow(exp)).min(MAX_BACKOFF);
        svc.state = State::Backoff { until: now + delay };
    }

}

impl Drop for Supervisor {
    fn drop(&mut self) { self.stop_all(); }
}

fn log(args: std::fmt::Arguments) {
    eprintln!("oxmux-supervisor: {args}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str) -> Service {
        Service {
            cfg: ServiceConfig { name: name.into(), exec: vec!["/bin/true".into()], kind: Kind::Simple,
                restart: Restart::Always, after: Vec::new(), wait_for: Vec::new(), env: BTreeMap::new(),
                fallback: None, log: None, optional: false },
            state: State::Pending, crashes: Vec::new(), command: CommandChoice::Primary,
        }
    }

    #[test]
    fn clean_exit_always_restarts_through_crash_policy() {
        assert_eq!(exit_decision(Kind::Simple, Restart::Always, true), ExitDecision::Restart);
        assert_eq!(exit_decision(Kind::Simple, Restart::OnFailure, true), ExitDecision::Dead);
        assert_eq!(exit_decision(Kind::Simple, Restart::Never, false), ExitDecision::Dead);
    }

    #[test]
    fn oneshot_completion_is_distinct_from_failure() {
        assert_eq!(exit_decision(Kind::Oneshot, Restart::Always, true), ExitDecision::Complete);
        assert_eq!(exit_decision(Kind::Oneshot, Restart::Always, false), ExitDecision::Dead);
    }

    #[test]
    fn restarts_back_off_and_have_a_cap() {
        let mut service = service("app");
        let now = Instant::now();
        for seconds in [1, 2, 4, 8, 16, 30, 30] {
            service.failed(now);
            assert!(matches!(service.state, State::Backoff { until } if until == now + Duration::from_secs(seconds)));
        }
    }

    #[test]
    fn fifth_restart_selects_owned_fallback_command() {
        let mut service = service("app");
        service.cfg.fallback = Some(vec!["/bin/rescue".into()]);
        let now = Instant::now();
        for _ in 0..5 { service.failed(now); }
        assert!(matches!(&service.command, CommandChoice::Fallback(command) if command == &["/bin/rescue"]));
        assert!(service.crashes.is_empty());
    }

    #[test]
    fn old_failures_expire() {
        let mut service = service("app");
        let now = Instant::now();
        service.failed(now);
        service.failed(now + CRASH_WINDOW);
        assert_eq!(service.crashes.len(), 1);
    }

    #[test]
    fn required_dead_dependency_blocks_but_optional_does_not() {
        let mut dependency = service("storage");
        dependency.state = State::Dead;
        let mut app = service("app");
        app.cfg.after = vec!["storage".into()];
        let mut services = vec![dependency, app];
        assert_eq!(blocked_dependency(&services, 1), Some("storage"));
        services[0].cfg.optional = true;
        assert_eq!(blocked_dependency(&services, 1), None);
    }

    #[test]
    fn dependency_cycles_are_rejected_before_process_setup() {
        let mut a = service("a").cfg;
        let mut b = service("b").cfg;
        a.after = vec!["b".into()];
        b.after = vec!["a".into()];
        let cfg = SupervisorConfig { log_dir: "/unused".into(), replaces: Vec::new(), services: vec![a, b] };
        assert!(Supervisor::new(&cfg, false).is_err());
    }
}
