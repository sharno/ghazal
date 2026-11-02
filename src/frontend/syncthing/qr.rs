//! This handheld's device ID as a QR code, which Syncthing's phone apps scan
//! to add a device. The ID is the plain grouped text, as Syncthing's own web
//! interface encodes it.

use qrcodegen::{QrCode, QrCodeEcc};

use crate::syncthing::DeviceId;

/// Light modules around the code that scanners need to find it.
const QUIET: i32 = 4;

/// The code's modules, quiet zone included, row by row: `true` is dark.
fn modules(id: &DeviceId) -> Option<(u32, Vec<bool>)> {
    let code = QrCode::encode_text(id.as_str(), QrCodeEcc::Medium).ok()?;
    let side = code.size() + 2 * QUIET;
    // Coordinates outside the code read as light.
    let dark = (0..side * side).map(|index| code.get_module(index % side - QUIET, index / side - QUIET)).collect();
    Some((u32::try_from(side).ok()?, dark))
}

/// One pixel per module: the panel scales it up with `image-rendering: pixelated`.
pub fn image(id: &DeviceId) -> Option<slint::Image> {
    let (side, dark) = modules(id)?;
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(side, side);
    buffer.make_mut_slice().iter_mut().zip(dark).for_each(|(pixel, dark)| {
        let level = if dark { 0 } else { 255 };
        *pixel = slint::Rgb8Pixel { r: level, g: level, b: level };
    });
    Some(slint::Image::from_rgb8(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_id_fits_a_small_code_with_its_quiet_zone() {
        let id = DeviceId::parse("HJ56AJG-RR4EN7L-B2WJ3UI-2NNTZCS-2PZ4P3W-R3SZJHF-S25SDML-LBSRGQI").unwrap();
        let (side, dark) = modules(&id).unwrap();
        // Alphanumeric mode: 63 characters need version 4 (33 modules) at medium correction.
        assert_eq!(side, 33 + 8);
        assert_eq!(dark.len(), (side * side) as usize);
        let at = |x: u32, y: u32| dark[(y * side + x) as usize];
        // The quiet zone is light; the finder pattern's corner is dark.
        assert!(!at(0, 0) && !at(3, 3));
        assert!(at(4, 4) && at(side - 5, 4) && at(4, side - 5));
    }
}
