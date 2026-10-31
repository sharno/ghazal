//! Bluetooth for gamepads (and other devices): a screen under Configuration, the
//! `ghazal bluetooth` command, and BlueZ's daemon started on demand.
//!
//! Layers, from the user down: `model` is the screen's state machine; a `Backend`
//! carries its orders out (`worker`: BlueZ over `dbus`; `backend::fake`: tests and the
//! desktop simulator); `hardware` powers the controller and keeps the preference.
//! docs/bluetooth.md describes the whole path from controller to game.

pub mod backend;
mod bluez;
mod cli;
mod dbus;
mod hardware;
pub mod model;
mod worker;

pub use cli::run;

/// Whether to offer Bluetooth in the menu: the board has a controller, or this is the simulator.
pub fn available() -> bool {
    cfg!(feature = "desktop") || hardware::present()
}
