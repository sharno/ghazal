//! The environment a port runs in. Pure: callers read the device and pass facts in.

use std::path::Path;

/// Analog sticks per board, from ROCKNIX's device table; boards it lacks keep the
/// two-stick default. PortMaster ports choose their gptokeyb profile from it.
const STICKS: [(&str, u8); 14] = [
    ("rg28xx", 0), ("rg34xx-sp", 0), ("rg34xx-sp-v2", 0), ("rg35xx-2024", 0), ("rg35xx-2024-rev6", 0),
    ("rg35xx-plus", 0), ("rg35xx-plus-rev6", 0), ("rg35xx-sp", 0), ("rg35xx-sp-v2", 0), ("rg-sp", 0),
    ("rg40xx-v", 1), ("rg40xx-v-v2", 1), ("rg35xx-pro", 2), ("rgcubexx", 2),
];

pub fn analog_sticks(device_id: &str) -> u8 {
    STICKS.iter().find(|(id, _)| *id == device_id).map_or(2, |(_, sticks)| *sticks)
}

pub struct Spec<'a> {
    /// Writable home for ports (their configs and saves outside the game folder).
    pub home: &'a Path,
    /// Tools ports expect: python3, unzip, mount, pkill, ...
    pub bin: &'a Path,
    /// Libraries ports expect the system to provide, by their sonames.
    pub libs: &'a Path,
    /// SDL controller database holding this pad's mapping.
    pub controller_db: &'a Path,
    pub display: (u32, u32),
    pub sticks: u8,
}

/// Variables added to the inherited environment. PortMaster's control.txt
/// (config/rg40xxv-rocknix/ports) repeats what ports need from a plain shell, so
/// ports also run from SSH; these make the launcher's own children consistent.
pub fn port_env(spec: &Spec) -> Vec<(&'static str, String)> {
    let path = |path: &Path| path.display().to_string();
    let home = spec.home;
    vec![
        ("PATH", format!("{}:/usr/bin:/usr/sbin", spec.bin.display())),
        ("HOME", path(home)),
        ("XDG_DATA_HOME", path(&home.join(".local/share"))),
        ("XDG_CONFIG_HOME", path(&home.join(".config"))),
        ("XDG_CACHE_HOME", path(&home.join(".cache"))),
        ("XDG_RUNTIME_DIR", "/run".into()),
        ("LD_LIBRARY_PATH", path(spec.libs)),
        // pysdl2 (PortMaster's GUI) finds SDL2, SDL2_image, SDL2_mixer and SDL2_ttf here.
        ("PYSDL2_DLL_PATH", path(spec.libs)),
        ("SDL_GAMECONTROLLERCONFIG_FILE", path(spec.controller_db)),
        ("TERM", "linux".into()),
        ("GHAZAL_DISPLAY_WIDTH", spec.display.0.to_string()),
        ("GHAZAL_DISPLAY_HEIGHT", spec.display.1.to_string()),
        ("GHAZAL_ANALOG_STICKS", spec.sticks.to_string()),
    ]
}

/// The first line of a DRM connector's modes file: "640x480".
pub fn parse_mode(modes: &str) -> Option<(u32, u32)> {
    let (width, height) = modes.lines().next()?.split_once('x')?;
    // Interlaced modes carry a suffix ("1920x1080i").
    Some((width.parse().ok()?, height.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticks_default_to_two_and_follow_the_board_table() {
        assert_eq!(analog_sticks("rg40xx-v-v2"), 1);
        assert_eq!(analog_sticks("rg35xx-plus"), 0);
        assert_eq!(analog_sticks("rg40xx-h"), 2);
        assert_eq!(analog_sticks("something-new"), 2);
    }

    #[test]
    fn modes_are_read_from_the_first_line() {
        assert_eq!(parse_mode("640x480\n320x240\n"), Some((640, 480)));
        assert_eq!(parse_mode("1920x1080i\n"), Some((1920, 1080)));
        assert_eq!(parse_mode(""), None);
        assert_eq!(parse_mode("auto\n"), None);
    }

    #[test]
    fn environment_puts_ports_tools_first_and_homes_under_the_data_card() {
        let spec = Spec {
            home: Path::new("/mnt/games/.ghazal/ports/home"), bin: Path::new("/usr/lib/ghazal/ports/bin"),
            libs: Path::new("/usr/lib/ghazal/ports/lib"), controller_db: Path::new("/run/ghazal/ports/gamecontrollerdb.txt"),
            display: (640, 480), sticks: 1,
        };
        let env: std::collections::BTreeMap<_, _> = port_env(&spec).into_iter().collect();
        assert_eq!(env["PATH"], "/usr/lib/ghazal/ports/bin:/usr/bin:/usr/sbin");
        assert_eq!(env["XDG_DATA_HOME"], "/mnt/games/.ghazal/ports/home/.local/share");
        assert_eq!(env["LD_LIBRARY_PATH"], "/usr/lib/ghazal/ports/lib");
        assert_eq!(env["GHAZAL_ANALOG_STICKS"], "1");
    }
}
