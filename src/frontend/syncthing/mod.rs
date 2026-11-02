//! Configuration → Syncthing: turning it on and off, this handheld's ID as a QR
//! code, folders and devices, pairing requests and the web interface on the
//! network. It drives `crate::syncthing`; see docs/syncthing.md.
//!
//! The pieces, from the outside in: `screen` is the pure state machine,
//! `controller` runs it with a worker thread, `qr` draws the ID, and `status`
//! keeps the status bar's icon current. Nothing here runs until the menu does.

mod controller;
#[cfg(any(test, feature = "desktop"))]
mod fake;
mod qr;
mod screen;
mod status;

pub use controller::{Controller, Reaction};
pub use qr::image as qr_image;
pub use screen::{Notice, View};
pub use status::Status;

use std::sync::Arc;

use crate::syncthing::{Backend, Summary, System};

/// Whether to offer the screen: this image has Syncthing, or this is the simulator.
pub fn available() -> bool {
    cfg!(feature = "desktop") || crate::syncthing::available()
}

/// The backend this build should use. The desktop simulator gets a pretend
/// Syncthing so it never touches the host's.
fn backend() -> Arc<dyn Backend> {
    #[cfg(feature = "desktop")]
    if std::env::var_os("GHAZAL_SYNCTHING_REAL").is_none() {
        return Arc::new(fake::FakeBackend::demo());
    }
    Arc::new(System::default())
}

/// Opens the screen: the first look starts at once, on the worker.
pub fn open(now: std::time::Instant) -> std::io::Result<Controller> {
    Controller::open(backend(), now)
}

/// The status bar's question, asked on a background thread.
pub fn summary() -> Summary {
    System::default().summary()
}
