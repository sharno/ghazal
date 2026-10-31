//! A radio that exists only in memory: it drives the screen's tests and the
//! desktop simulator, which must never touch the host's Wi-Fi.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::backend::Backend;
use super::parse::{self, Listed, Saved, StationInfo};
use super::types::{Band, ConnectRequest, Connection, Failure, Link, Security, Signal, Snapshot, Ssid, Visibility};

struct Visible {
    ssid: Ssid,
    security: Security,
    signal: Signal,
    /// None for open networks.
    secret: Option<String>,
    visibility: Visibility,
}

struct State {
    link: Link,
    visible: Vec<Visible>,
    saved: Vec<Saved>,
    /// Snapshots left that still report a scan in progress.
    scanning: u32,
    /// Fails the next operation of any kind.
    fail_next: Option<Failure>,
    calls: Vec<String>,
}

pub struct FakeBackend {
    state: Mutex<State>,
    /// How long connecting takes.
    delay: Duration,
}

impl FakeBackend {
    pub fn new(delay: Duration) -> Self {
        let state = State { link: Link::Disconnected, visible: Vec::new(), saved: Vec::new(), scanning: 0, fail_next: None, calls: Vec::new() };
        Self { state: Mutex::new(state), delay }
    }

    /// What the simulator shows. Unknown networks take the password `ghazal123`.
    #[cfg(feature = "desktop")]
    pub fn demo() -> Self {
        Self::new(Duration::from_millis(200))
            .with_network("Tomato24", Security::Psk, -44, Some("ghazal123"), Visibility::Listed)
            .with_network("Cafe Wi-Fi", Security::Psk, -62, Some("ghazal123"), Visibility::Listed)
            .with_network("Guest", Security::Open, -71, None, Visibility::Listed)
            .with_network("Campus", Security::Enterprise, -55, None, Visibility::Listed)
            .with_network("Neighbours 5G", Security::Psk, -78, Some("ghazal123"), Visibility::Listed)
            .with_network("Hidden lab", Security::Psk, -60, Some("ghazal123"), Visibility::Hidden)
            .saved("Tomato24", Security::Psk)
            .saved("Grandma's house", Security::Psk)
            .connected_to("Tomato24")
    }

    pub fn with_network(self, name: &str, security: Security, dbm: i32, secret: Option<&str>, visibility: Visibility) -> Self {
        self.state().visible.push(Visible { ssid: Ssid::new(name).expect("fixture name"), security, signal: Signal::from_dbm(dbm), secret: secret.map(str::to_owned), visibility });
        self
    }

    pub fn saved(self, name: &str, security: Security) -> Self {
        self.state().saved.push(Saved { ssid: Ssid::new(name).expect("fixture name"), security, visibility: Visibility::Listed });
        self
    }

    pub fn connected_to(self, name: &str) -> Self {
        let ssid = Ssid::new(name).expect("fixture name");
        {
            let mut state = self.state();
            let visible = state.visible.iter().find(|network| network.ssid == ssid).map(|network| (network.signal, network.security));
            state.link = Link::Connected(Connection { ssid, address: Some("192.168.1.133".parse().expect("address")), signal: visible.map(|v| v.0), band: Some(Band::Ghz5), security: visible.map_or(Security::Psk, |v| v.1) });
        }
        self
    }

    #[cfg(test)]
    pub fn fail_next(&self, failure: Failure) { self.state().fail_next = Some(failure); }

    #[cfg(test)]
    pub fn calls(&self) -> Vec<String> { self.state().calls.clone() }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records the call and applies a queued failure.
    fn begin(&self, call: String) -> Result<(), Failure> {
        let mut state = self.state();
        state.calls.push(call);
        state.fail_next.take().map_or(Ok(()), Err)
    }
}

impl Backend for FakeBackend {
    fn snapshot(&self) -> Result<Snapshot, Failure> {
        self.begin("snapshot".into())?;
        let mut state = self.state();
        let scanning = state.scanning > 0;
        state.scanning = state.scanning.saturating_sub(1);
        let listed = state.visible.iter().filter(|network| network.visibility == Visibility::Listed).map(|network| Listed { ssid: network.ssid.clone(), security: network.security, signal: network.signal }).collect();
        let saved = state.saved.iter().map(|known| Saved { ssid: known.ssid.clone(), security: known.security, visibility: known.visibility }).collect();
        Ok(parse::assemble(StationInfo { scanning, link: state.link.clone() }, listed, saved))
    }

    fn scan(&self) -> Result<(), Failure> {
        self.begin("scan".into())?;
        self.state().scanning = 2;
        Ok(())
    }

    fn connect(&self, request: &ConnectRequest, abort: &AtomicBool) -> Result<(), Failure> {
        self.begin(format!("connect {}{}{}", request.ssid, if request.visibility == Visibility::Hidden { " hidden" } else { "" }, if request.passphrase.is_some() { " with passphrase" } else { "" }))?;
        self.state().link = Link::Connecting;
        let deadline = Instant::now() + self.delay;
        while Instant::now() < deadline {
            if abort.load(Ordering::SeqCst) {
                self.state().link = Link::Disconnected;
                return Err(Failure::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut state = self.state();
        let outcome = match state.visible.iter().find(|network| network.ssid == request.ssid && (request.visibility == Visibility::Hidden || network.visibility == Visibility::Listed)) {
            None => Err(Failure::Missing),
            Some(network) => match (&network.secret, state.saved.iter().any(|known| known.ssid == request.ssid), &request.passphrase) {
                (None, _, _) | (Some(_), true, _) => Ok((network.signal, network.security)),
                (Some(secret), false, Some(given)) if given.expose() == secret => Ok((network.signal, network.security)),
                (Some(_), false, _) => Err(Failure::Rejected),
            },
        };
        match outcome {
            Ok((signal, security)) => {
                if !state.saved.iter().any(|known| known.ssid == request.ssid) { state.saved.push(Saved { ssid: request.ssid.clone(), security, visibility: request.visibility }); }
                state.link = Link::Connected(Connection { ssid: request.ssid.clone(), address: Some("192.168.1.134".parse().expect("address")), signal: Some(signal), band: Some(Band::Ghz5), security });
                Ok(())
            }
            Err(failure) => {
                state.link = Link::Disconnected;
                Err(failure)
            }
        }
    }

    fn disconnect(&self) -> Result<(), Failure> {
        self.begin("disconnect".into())?;
        self.state().link = Link::Disconnected;
        Ok(())
    }

    fn forget(&self, ssid: &Ssid) -> Result<(), Failure> {
        self.begin(format!("forget {ssid}"))?;
        let mut state = self.state();
        state.saved.retain(|known| &known.ssid != ssid);
        if matches!(&state.link, Link::Connected(connection) if &connection.ssid == ssid) { state.link = Link::Disconnected; }
        Ok(())
    }
}
