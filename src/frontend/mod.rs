//! The game menu: Slint UI, ROM library, RetroArch launching.

pub mod app;
mod activity;
pub mod config;
pub mod emulator;
mod emulator_choice;
mod emulator_picker;
pub mod input;
mod keyboard;
pub mod launcher;
pub mod library;
mod moonlight;
pub mod platform;
mod settings;
mod thumbnails;
mod wifi;

pub fn benchmark_recent(config: &config::FrontendConfig) -> anyhow::Result<()> { activity::benchmark(config) }

pub fn observe_moonlight_hosts() -> anyhow::Result<()> { moonlight::observe_hosts() }
