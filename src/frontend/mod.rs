//! The game menu: Slint UI, ROM library, RetroArch launching.

pub mod app;
mod activity;
mod bluetooth;
pub mod config;
pub mod emulator;
pub mod emulator_choice;
mod emulator_picker;
mod indicators;
pub mod input;
mod keyboard;
pub mod launcher;
pub mod library;
mod moonlight;
pub mod netplay;
pub mod platform;
pub mod retroarch_menu;
mod settings;
mod syncthing;
mod thumbnails;
mod timezone;
mod wifi;

pub fn benchmark_recent(config: &config::FrontendConfig) -> anyhow::Result<()> { activity::benchmark(config) }

pub fn observe_moonlight_hosts() -> anyhow::Result<()> { moonlight::observe_hosts() }
