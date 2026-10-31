//! SDL's game controller mapping strings (`SDL_GAMECONTROLLERCONFIG`), as the
//! standalone emulators receive the handheld's pad.

use std::collections::BTreeMap;

use crate::profile::Pad;

/// SDL's `SDL_crc16`: CRC-16/ARC (reflected polynomial 0xA001) from zero.
fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |crc, byte| {
        (0..8).fold(crc ^ u16::from(*byte), |crc, _| if crc & 1 == 1 { (crc >> 1) ^ 0xA001 } else { crc >> 1 })
    })
}

/// The 16-byte joystick GUID SDL derives for the pad on Linux, as lowercase
/// hex: bus, name checksum, vendor, 0, product, 0, version, 0, all little endian.
pub fn guid(pad: &Pad) -> String {
    [pad.bus, crc16(pad.name.as_bytes()), pad.vendor, 0, pad.product, 0, pad.version, 0]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Mapping {
    pub guid: String,
    pub name: String,
    /// `a:b1` becomes ("a", "b1"); platform and other fields are kept as is.
    pub fields: BTreeMap<String, String>,
}

impl Mapping {
    /// The first mapping line in a possibly newline-separated string.
    pub fn parse(text: &str) -> Result<Self, String> {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#')).ok_or("empty controller mapping")?;
        let mut items = line.split(',').map(str::trim);
        let guid = items.next().filter(|guid| guid.len() == 32 && guid.bytes().all(|byte| byte.is_ascii_hexdigit())).ok_or("the mapping does not start with a 32-digit GUID")?;
        let name = items.next().filter(|name| !name.is_empty()).ok_or("the mapping has no controller name")?;
        let fields = items.filter_map(|item| item.split_once(':')).map(|(key, value)| (key.to_owned(), value.to_owned())).collect();
        Ok(Self { guid: guid.to_ascii_lowercase(), name: name.to_owned(), fields })
    }

    /// The pad button index of a mapped control: `b3` is 3.
    pub fn button(&self, control: &str) -> Option<u32> {
        self.fields.get(control)?.strip_prefix('b')?.parse().ok()
    }

    /// The pad axis index of a mapped control: `a2` is 2.
    pub fn axis(&self, control: &str) -> Option<u32> {
        self.fields.get(control)?.strip_prefix('a')?.parse().ok()
    }
}

/// SDL control -> the RetroArch `input_<name>_btn` it must agree with. Both
/// number buttons by position among the pad's key codes.
pub const BUTTONS: [(&str, &str); 17] = [
    ("a", "a"),
    ("b", "b"),
    ("x", "x"),
    ("y", "y"),
    ("back", "select"),
    ("start", "start"),
    ("guide", "enable_hotkey"),
    ("leftshoulder", "l"),
    ("rightshoulder", "r"),
    ("lefttrigger", "l2"),
    ("righttrigger", "r2"),
    ("leftstick", "l3"),
    ("rightstick", "r3"),
    ("dpup", "up"),
    ("dpdown", "down"),
    ("dpleft", "left"),
    ("dpright", "right"),
];

/// SDL axis -> the RetroArch `input_<name>_axis` (the positive direction).
pub const AXES: [(&str, &str); 4] = [("leftx", "l_x_plus"), ("lefty", "l_y_plus"), ("rightx", "r_x_plus"), ("righty", "r_y_plus")];

#[cfg(test)]
mod tests {
    use super::*;

    fn pad() -> Pad {
        Pad { name: "H700 Gamepad".into(), display_name: "x".into(), bus: 0x19, vendor: 18507, product: 5343, version: 0x100 }
    }

    /// The GUID in frontend.toml was captured from SDL on the handheld.
    #[test]
    fn guid_matches_the_one_sdl_derives_on_the_device() {
        assert_eq!(guid(&pad()), "1900f6a24b480000df14000000010000");
    }

    #[test]
    fn parses_a_mapping() {
        let mapping = Mapping::parse("1900f6a24b480000df14000000010000,H700 Gamepad,platform:Linux,a:b1,leftx:a0,\n").unwrap();
        assert_eq!(mapping.name, "H700 Gamepad");
        assert_eq!(mapping.button("a"), Some(1));
        assert_eq!(mapping.axis("leftx"), Some(0));
        assert_eq!(mapping.button("leftx"), None);
        assert_eq!(mapping.button("missing"), None);
    }

    #[test]
    fn rejects_malformed_mappings() {
        assert!(Mapping::parse("").is_err());
        assert!(Mapping::parse("short,H700 Gamepad,a:b1").is_err());
        assert!(Mapping::parse("1900f6a24b480000df14000000010000").is_err());
    }
}
