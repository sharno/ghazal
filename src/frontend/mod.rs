//! The game menu: Slint UI, ROM library, RetroArch launching.

pub mod app;
mod activity;
pub mod config;
pub mod input;
pub mod launcher;
pub mod library;
pub mod platform;
mod settings;
mod thumbnails;

pub fn benchmark_recent(config: &config::FrontendConfig) -> anyhow::Result<()> { activity::benchmark(config) }
