//! HDMI sound. The card (the H616 audio hub feeding the DW-HDMI I2S input) does
//! not exist at boot: its device-tree nodes ship disabled, so it cannot shift
//! the codec's card number or cost boot time. The first request that wants HDMI
//! sound applies a device-tree overlay through configfs, which creates the card.
//! Without the overlay support or the card, sound stays on the speaker.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::device::HdmiAudio;

const CONFIGFS: &str = "/sys/kernel/config";
const OVERLAY_NAME: &str = "ghazal-hdmi-audio";
/// Registering the card takes a few probes and deferrals after the overlay applies.
const CARD_WAIT: Duration = Duration::from_secs(3);

/// `/proc/asound/cards` lists `" 1 [HDMI          ]: ..."`; the bracket holds the card id.
fn lists_card(cards: &str, id: &str) -> bool {
    cards.lines().filter_map(|line| line.split_once('[')?.1.split_once(']')).any(|(listed, _)| listed.trim() == id)
}

pub fn card_present(config: &HdmiAudio) -> bool {
    std::fs::read_to_string("/proc/asound/cards").is_ok_and(|cards| lists_card(&cards, &config.card))
}

/// What emulators open while HDMI carries the sound. `plughw` converts the
/// formats the HDMI link does not take natively.
pub fn alsa_device(config: &HdmiAudio) -> String {
    format!("plughw:{},0", config.card)
}

/// Makes the HDMI card exist, applying the overlay when it does not yet.
pub fn enable(config: &HdmiAudio) -> Result<()> {
    if card_present(config) { return Ok(()); }
    let Some(overlay) = &config.overlay else { bail!("no HDMI sound card and no overlay to create it") };
    apply_overlay(overlay)?;
    let deadline = Instant::now() + CARD_WAIT;
    while !card_present(config) {
        if Instant::now() >= deadline { bail!("the HDMI sound card did not appear after applying {}", overlay.display()); }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn apply_overlay(overlay: &Path) -> Result<()> {
    // Read first: a kernel built without HDMI sound has no overlay, and then configfs stays untouched.
    let blob = std::fs::read(overlay).with_context(|| format!("reading {}", overlay.display()))?;
    let configfs = Path::new(CONFIGFS);
    if !crate::sys::is_mounted(configfs) {
        crate::sys::mount("none", configfs, "configfs", 0, "").context("configfs is needed to apply the HDMI sound overlay")?;
    }
    let directory = configfs.join("device-tree/overlays").join(OVERLAY_NAME);
    // The directory outlives a failed card: applying twice would fail with EEXIST.
    if directory.exists() { return Ok(()); }
    std::fs::create_dir(&directory).context("device-tree overlays are not supported by this kernel")?;
    // The kernel applies the blob when the file is closed; `status` reports the outcome.
    let applied = std::fs::write(directory.join("dtbo"), blob)
        .and_then(|()| std::fs::read_to_string(directory.join("status")));
    match applied {
        Ok(status) if status.trim() == "applied" => Ok(()),
        outcome => {
            let _ = std::fs::remove_dir(&directory);
            bail!("overlay {} was not applied ({})", overlay.display(), outcome.map_or_else(|error| error.to_string(), |status| status.trim().to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS: &str = " 0 [Codec          ]: sun4i-codec - H616 Audio Codec\n                      h616-audio-codec\n 1 [HDMI           ]: sunxi-snd-mach - HDMI\n                      HDMI\n";

    #[test]
    fn cards_are_matched_by_their_id() {
        assert!(lists_card(CARDS, "HDMI"));
        assert!(lists_card(CARDS, "Codec"));
        assert!(!lists_card(CARDS, "ahubdam"));
        assert!(!lists_card("", "HDMI"));
        // The long name on the continuation line is not an id.
        assert!(!lists_card(CARDS, "h616-audio-codec"));
    }

    #[test]
    fn emulators_open_the_plug_device_of_the_card() {
        assert_eq!(alsa_device(&HdmiAudio { card: "HDMI".into(), overlay: None }), "plughw:HDMI,0");
    }
}
