//! The H700 pad as SDL sees it: a GUID and a controller mapping line, derived
//! from the kernel's evdev capabilities and device.toml's button codes. PortMaster's
//! own database has no entry for this pad, and SDL's button numbers depend on which
//! key codes a driver reports, so the line is generated rather than written by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};

/// SDL numbers buttons from BTN_JOYSTICK up to KEY_MAX first, then the codes below it.
const BTN_JOYSTICK: u16 = 0x120;
const KEY_MAX: u16 = 0x2ff;
const ABS_MAX: u16 = 0x3f;
/// Hat axes are skipped when SDL numbers axes.
const HATS: std::ops::RangeInclusive<u16> = 0x10..=0x17;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;

/// SDL controller name, then the device.toml button that sends it. Face buttons
/// follow position, as in ROCKNIX's mapping that PortMaster's ports are made
/// against: this pad labels its right button A and its bottom button B, the
/// reverse of Xbox's A (bottom) and B (right).
const BUTTONS: [(&str, &str); 17] = [
    ("a", "b"), ("b", "a"), ("x", "y"), ("y", "x"),
    ("back", "select"), ("start", "start"), ("guide", "menu"),
    ("leftshoulder", "l1"), ("rightshoulder", "r1"), ("lefttrigger", "l2"), ("righttrigger", "r2"),
    ("leftstick", "l3"), ("rightstick", "r3"),
    ("dpup", "up"), ("dpdown", "down"), ("dpleft", "left"), ("dpright", "right"),
];

const AXES: [(&str, u16); 4] = [("leftx", ABS_X), ("lefty", ABS_Y), ("rightx", ABS_RX), ("righty", ABS_RY)];

/// What the kernel reports for one input device under `/sys/class/input`.
#[derive(Debug, PartialEq)]
pub struct Pad {
    pub name: String,
    bus: u16,
    vendor: u16,
    product: u16,
    version: u16,
    keys: BTreeSet<u16>,
    axes: BTreeSet<u16>,
}

impl Pad {
    /// The input device whose name contains `name`, among `inputN` folders of `class_dir`.
    pub fn find(class_dir: &Path, name: &str) -> Result<Self> {
        let entry = std::fs::read_dir(class_dir)
            .with_context(|| format!("reading {}", class_dir.display()))?
            .flatten()
            .find(|entry| std::fs::read_to_string(entry.path().join("name")).is_ok_and(|found| found.contains(name)))
            .with_context(|| format!("no input device named {name:?}"))?;
        Self::read(&entry.path())
    }

    fn read(dir: &Path) -> Result<Self> {
        let text = |file: &str| std::fs::read_to_string(dir.join(file)).with_context(|| format!("reading {}/{file}", dir.display()));
        let id = |file: &str| -> Result<u16> { Ok(u16::from_str_radix(text(file)?.trim(), 16)?) };
        Ok(Self {
            name: text("name")?.trim().to_owned(),
            bus: id("id/bustype")?,
            vendor: id("id/vendor")?,
            product: id("id/product")?,
            version: id("id/version")?,
            keys: parse_bitmap(&text("capabilities/key")?),
            axes: parse_bitmap(&text("capabilities/abs")?),
        })
    }

    /// SDL's joystick GUID: bus, a CRC of the name, then vendor, product and version.
    pub fn guid(&self) -> String {
        let words = [self.bus, crc16(self.name.as_bytes()), self.vendor, 0, self.product, 0, self.version, 0];
        words.iter().flat_map(|word| word.to_le_bytes()).map(|byte| format!("{byte:02x}")).collect()
    }

    fn button(&self, code: u16) -> Option<usize> {
        (BTN_JOYSTICK..KEY_MAX).chain(0..BTN_JOYSTICK).filter(|key| self.keys.contains(key)).position(|key| key == code)
    }

    fn axis(&self, code: u16) -> Option<usize> {
        (0..ABS_MAX).filter(|axis| !HATS.contains(axis) && self.axes.contains(axis)).position(|axis| axis == code)
    }

    /// An SDL_GameControllerDB line. Controls the pad lacks are left out.
    pub fn mapping(&self, buttons: &BTreeMap<String, u16>) -> String {
        let buttons = BUTTONS.iter().filter_map(|(sdl, name)| Some(format!("{sdl}:b{}", self.button(*buttons.get(*name)?)?)));
        let axes = AXES.iter().filter_map(|(sdl, code)| Some(format!("{sdl}:a{}", self.axis(*code)?)));
        let controls: Vec<String> = buttons.chain(axes).collect();
        format!("{},{},platform:Linux,{},", self.guid(), self.name, controls.join(","))
    }
}

/// The kernel prints a bitmap as hex words, most significant first, 64 bits each.
fn parse_bitmap(text: &str) -> BTreeSet<u16> {
    text.split_whitespace().rev().enumerate()
        .flat_map(|(word, hex)| {
            let bits = u64::from_str_radix(hex, 16).unwrap_or(0);
            (0..64u16).filter(move |bit| bits >> bit & 1 == 1).filter_map(move |bit| u16::try_from(word * 64).ok()?.checked_add(bit))
        })
        .collect()
}

/// SDL's name checksum: CRC-16/ARC (polynomial 0xA001, reflected, initial 0).
fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |crc, byte| {
        (0..8).fold(crc ^ u16::from(*byte), |crc, _| if crc & 1 == 1 { crc >> 1 ^ 0xA001 } else { crc >> 1 })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RG40XX-V v2 pad, as `cat /proc/bus/input/devices` and sysfs report it.
    fn h700() -> Pad {
        Pad {
            name: "H700 Gamepad".into(), bus: 0x19, vendor: 0x484b, product: 0x14df, version: 0x100,
            keys: parse_bitmap("f00000000 0 0 0 7fdb000000000000 0 0 0 0"),
            axes: parse_bitmap("1b"),
        }
    }

    fn device_toml_buttons() -> BTreeMap<String, u16> {
        [("a", 305), ("b", 304), ("x", 308), ("y", 307), ("l1", 310), ("r1", 311), ("l2", 312), ("r2", 313),
         ("select", 314), ("start", 315), ("menu", 316), ("l3", 317), ("r3", 318),
         ("up", 544), ("down", 545), ("left", 546), ("right", 547)]
            .into_iter().map(|(name, code)| (name.to_owned(), code)).collect()
    }

    #[test]
    fn guid_matches_the_one_sdl_reports_for_this_pad() {
        // The same GUID as the PSP system's SDL_GAMECONTROLLERCONFIG in frontend.toml.
        assert_eq!(h700().guid(), "1900f6a24b480000df14000000010000");
    }

    #[test]
    fn bitmaps_decode_most_significant_word_first() {
        let keys = parse_bitmap("f00000000 0 0 0 7fdb000000000000 0 0 0 0");
        assert_eq!(keys.iter().copied().filter(|code| *code < 0x200).collect::<Vec<_>>(), [304, 305, 307, 308, 310, 311, 312, 313, 314, 315, 316, 317, 318]);
        assert_eq!(keys.iter().copied().filter(|code| *code >= 0x200).collect::<Vec<_>>(), [544, 545, 546, 547]);
        assert_eq!(parse_bitmap("1b").into_iter().collect::<Vec<_>>(), [0, 1, 3, 4]);
    }

    #[test]
    fn buttons_are_numbered_the_way_sdl_numbers_them() {
        let pad = h700();
        assert_eq!([pad.button(304), pad.button(305), pad.button(307), pad.button(308), pad.button(318), pad.button(544), pad.button(547)],
            [Some(0), Some(1), Some(2), Some(3), Some(12), Some(13), Some(16)]);
        assert_eq!(pad.button(306), None);
        assert_eq!([pad.axis(ABS_X), pad.axis(ABS_Y), pad.axis(ABS_RX), pad.axis(ABS_RY)], [Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn face_buttons_follow_position() {
        let mapping = h700().mapping(&device_toml_buttons());
        assert_eq!(mapping, "1900f6a24b480000df14000000010000,H700 Gamepad,platform:Linux,\
            a:b0,b:b1,x:b2,y:b3,back:b8,start:b9,guide:b10,leftshoulder:b4,rightshoulder:b5,\
            lefttrigger:b6,righttrigger:b7,leftstick:b11,rightstick:b12,\
            dpup:b13,dpdown:b14,dpleft:b15,dpright:b16,leftx:a0,lefty:a1,rightx:a2,righty:a3,");
    }

    #[test]
    fn controls_the_pad_lacks_are_left_out() {
        let mut pad = h700();
        pad.axes = parse_bitmap("3");
        pad.keys.remove(&318);
        let mapping = pad.mapping(&device_toml_buttons());
        assert!(!mapping.contains("rightstick") && !mapping.contains("rightx"));
        assert!(mapping.contains("lefty:a1"));
    }
}
