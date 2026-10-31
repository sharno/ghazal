//! Wi-Fi setup from Configuration → Wi-Fi: status, scanning, joining open and
//! password-protected networks (with the on-screen keyboard), hidden networks
//! and forgetting saved ones. It drives iwd through its own iwctl; see
//! docs/wifi.md.
//!
//! The pieces, from the outside in: `parse` and `backend` turn iwctl into
//! typed values, `screen` is the pure state machine, `controller` runs it with
//! a worker thread. Nothing here runs at boot: the worker starts when the
//! screen opens and stops when it closes.

mod backend;
mod controller;
#[cfg(any(test, feature = "desktop"))]
mod fake;
mod parse;
mod screen;
mod types;

pub use controller::{Controller, Reaction};
pub use screen::{Notice, View};

use std::sync::OnceLock;

/// Whether to offer Wi-Fi: this image has the tooling and the handheld a
/// wireless interface.
pub fn available() -> bool {
    static TOOLING: OnceLock<bool> = OnceLock::new();
    *TOOLING.get_or_init(|| backend::system_backend().is_some()) && backend::adapter_present()
}

/// Opens the screen: the first refresh starts at once, on the worker.
pub fn open(now: std::time::Instant) -> std::io::Result<Controller> {
    let backend = backend::system_backend().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no Wi-Fi tooling on this system"))?;
    Controller::open(backend, now)
}
