//! Configuration → Time zone: a region, then a place in it. Choosing one is
//! the app's job (`crate::timezone::Store`); this is only the two lists.

use super::app::navigate;
use super::input::Button;
use crate::timezone::Zone;

pub type Hints = &'static [(&'static str, &'static str)];
const REGION_HINTS: Hints = &[("A", "Open"), ("B", "Back")];
const PLACE_HINTS: Hints = &[("A", "Choose"), ("B", "Back")];

pub struct Line {
    pub label: String,
    pub detail: String,
}

pub enum Press {
    Stay,
    Leave,
    Choose(Zone),
}

enum Level {
    Regions { cursor: usize },
    /// `back` is the regions' cursor to return to.
    Places { region: String, cursor: usize, back: usize },
}

pub struct Screen {
    /// Sorted with UTC first, as `crate::timezone::zones` returns them.
    zones: Vec<Zone>,
    current: Zone,
    level: Level,
}

impl Screen {
    /// Opens on the current zone's region.
    pub fn new(zones: Vec<Zone>, current: Zone) -> Self {
        let cursor = regions(&zones).iter().position(|region| *region == current.region()).unwrap_or(0);
        Self { zones, current, level: Level::Regions { cursor } }
    }

    fn regions(&self) -> Vec<&str> { regions(&self.zones) }

    fn places(&self, region: &str) -> Vec<&Zone> { self.zones.iter().filter(|zone| zone.region() == region).collect() }

    pub fn len(&self) -> usize {
        match &self.level {
            Level::Regions { .. } => self.regions().len(),
            Level::Places { region, .. } => self.places(region).len(),
        }
    }

    pub fn cursor(&self) -> usize {
        match self.level { Level::Regions { cursor } | Level::Places { cursor, .. } => cursor }
    }

    pub fn hints(&self) -> Hints {
        match self.level { Level::Regions { .. } => REGION_HINTS, Level::Places { .. } => PLACE_HINTS }
    }

    pub fn line(&self, index: usize) -> Option<Line> {
        match &self.level {
            Level::Regions { .. } => {
                let region = *self.regions().get(index)?;
                // The current zone's place, so the region holding it stands out.
                let detail = match (region == self.current.region(), self.current.name().contains('/')) {
                    (true, true) => self.current.place(),
                    (true, false) => "Current".to_owned(),
                    (false, _) => String::new(),
                };
                Some(Line { label: region.to_owned(), detail })
            }
            Level::Places { region, .. } => {
                let zone = *self.places(region).get(index)?;
                let detail = if *zone == self.current { "Current" } else { "" };
                Some(Line { label: zone.place(), detail: detail.to_owned() })
            }
        }
    }

    pub fn press(&mut self, button: Button) -> Press {
        let len = self.len();
        match (&mut self.level, button) {
            (Level::Regions { .. }, Button::B) => Press::Leave,
            (Level::Places { back, .. }, Button::B) => {
                self.level = Level::Regions { cursor: *back };
                Press::Stay
            }
            (Level::Regions { cursor }, Button::A) => {
                let cursor = *cursor;
                let Some(region) = self.regions().get(cursor).map(|region| (*region).to_owned()) else { return Press::Stay };
                match self.places(&region).as_slice() {
                    // UTC is a region of one: choose it at once.
                    [zone] if zone.region() == zone.name() => Press::Choose((*zone).clone()),
                    places => {
                        let place = places.iter().position(|zone| **zone == self.current).unwrap_or(0);
                        self.level = Level::Places { region, cursor: place, back: cursor };
                        Press::Stay
                    }
                }
            }
            (Level::Places { region, cursor, .. }, Button::A) => {
                let (region, cursor) = (region.clone(), *cursor);
                self.places(&region).get(cursor).map_or(Press::Stay, |zone| Press::Choose((*zone).clone()))
            }
            (Level::Regions { cursor } | Level::Places { cursor, .. }, button) => {
                *cursor = navigate(*cursor, len, button);
                Press::Stay
            }
        }
    }
}

/// Each zone's region once, in the zones' order.
fn regions(zones: &[Zone]) -> Vec<&str> {
    let mut regions: Vec<&str> = zones.iter().map(Zone::region).collect();
    regions.dedup();
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(current: &str) -> Screen {
        let zones = ["UTC", "Africa/Cairo", "America/Chicago", "America/New_York", "America/Toronto", "Europe/Berlin"];
        Screen::new(zones.iter().map(|name| Zone::parse(name).unwrap()).collect(), Zone::parse(current).unwrap())
    }

    fn labels(screen: &Screen) -> Vec<(String, String)> {
        (0..screen.len()).filter_map(|index| screen.line(index)).map(|line| (line.label, line.detail)).collect()
    }

    #[test]
    fn regions_open_on_the_current_zone() {
        let mut screen = screen("America/New_York");
        assert_eq!(screen.cursor(), 2);
        assert_eq!(labels(&screen)[2], ("America".to_owned(), "New York".to_owned()));
        assert!(matches!(screen.press(Button::A), Press::Stay));
        assert_eq!(screen.cursor(), 1);
        assert_eq!(labels(&screen), [("Chicago", ""), ("New York", "Current"), ("Toronto", "")].map(|(label, detail)| (label.to_owned(), detail.to_owned())));
    }

    #[test]
    fn a_place_is_chosen_and_b_goes_back_to_its_region() {
        let mut screen = screen("UTC");
        screen.press(Button::Down);
        screen.press(Button::Down);
        screen.press(Button::A);
        screen.press(Button::Down);
        assert!(matches!(screen.press(Button::A), Press::Choose(zone) if zone.name() == "America/New_York"));
        assert!(matches!(screen.press(Button::B), Press::Stay));
        assert_eq!((screen.cursor(), screen.hints()), (2, REGION_HINTS));
        assert!(matches!(screen.press(Button::B), Press::Leave));
    }

    #[test]
    fn utc_is_chosen_from_the_regions() {
        let mut screen = screen("Europe/Berlin");
        assert_eq!(screen.cursor(), 3);
        screen.press(Button::Down);
        assert!(matches!(screen.press(Button::A), Press::Choose(zone) if zone == Zone::utc()));
        assert_eq!(labels(&self::screen("UTC"))[0], ("UTC".to_owned(), "Current".to_owned()));
    }
}
