mod app;
mod config;
mod input;
mod launcher;
mod library;
mod platform;
mod system_info;

mod ui {
    slint::include_modules!();
}

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};

const HELP: &str = "\
oxmux - Rust frontend for Anbernic H700 handhelds

USAGE: oxmux [--config PATH] [--probe-input]

  -c, --config PATH   config file (default: oxmux.toml next to the binary, else built-in muOS config)
      --probe-input   list input devices and print raw events, to work out button codes
  -h, --help          show this help";

fn main() -> Result<()> {
    let mut config_path: Option<PathBuf> = None;
    #[cfg(feature = "desktop")]
    let mut snapshot: Option<(PathBuf, String)> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                config_path = Some(args.next().ok_or_else(|| anyhow!("--config needs a path"))?.into())
            }
            "--probe-input" => return platform::evdev_input::probe(),
            // Dev only: `--snapshot DIR BUTTONS` renders frames to PNG without a window.
            #[cfg(feature = "desktop")]
            "--snapshot" => {
                let dir = args.next().ok_or_else(|| anyhow!("--snapshot needs DIR BUTTONS"))?;
                snapshot = Some((dir.into(), args.next().unwrap_or_default()));
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?}\n\n{HELP}"),
        }
    }

    let config = config::Config::load(config_path.as_deref())?;
    #[cfg(feature = "desktop")]
    if let Some((dir, buttons)) = snapshot {
        return platform::desktop::snapshot(config, &dir, &buttons);
    }
    platform::run(config)
}
