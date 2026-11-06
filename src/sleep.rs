//! Configuration → Sleep: whether a short press of Power puts the handheld to
//! sleep, and how long it may sleep before the RTC alarm wakes it to power off.
//! Powering off then stops a running game cleanly, which writes its saves.
//!
//! The image chooses the starting value (`[power].sleep` in system.toml); the
//! player's choice is kept by the daemon in `settings.toml`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The choices, in the order Configuration cycles through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Sleep {
    Off,
    OneHour,
    TwoHours,
    ThreeHours,
    SixHours,
    TwelveHours,
}

const CHOICES: [Sleep; 6] = [Sleep::Off, Sleep::OneHour, Sleep::TwoHours, Sleep::ThreeHours, Sleep::SixHours, Sleep::TwelveHours];

/// One row per choice: stored id, Configuration detail and the longest sleep.
struct Row { id: &'static str, detail: &'static str, hours: Option<u64> }

impl Sleep {
    fn row(self) -> Row {
        let row = |id, detail, hours| Row { id, detail, hours };
        match self {
            Self::Off => row("off", "Off", None),
            Self::OneHour => row("1h", "On, power off after 1 h", Some(1)),
            Self::TwoHours => row("2h", "On, power off after 2 h", Some(2)),
            Self::ThreeHours => row("3h", "On, power off after 3 h", Some(3)),
            Self::SixHours => row("6h", "On, power off after 6 h", Some(6)),
            Self::TwelveHours => row("12h", "On, power off after 12 h", Some(12)),
        }
    }

    pub fn id(self) -> &'static str { self.row().id }

    pub fn describe(self) -> &'static str { self.row().detail }

    /// How long a sleep may last before the handheld powers off; None when
    /// sleep is off.
    pub fn limit(self) -> Option<Duration> { self.row().hours.map(|hours| Duration::from_secs(hours * 3600)) }

    pub fn parse(id: &str) -> Option<Self> { CHOICES.into_iter().find(|choice| choice.id() == id) }

    /// `CHOICES` lists the variants in declaration order, so the discriminant
    /// is the index.
    pub fn step(self, forward: bool) -> Self {
        let index = self as usize;
        CHOICES[if forward { index + 1 } else { index + CHOICES.len() - 1 } % CHOICES.len()]
    }
}

impl TryFrom<String> for Sleep {
    type Error = String;
    fn try_from(id: String) -> Result<Self, String> {
        Self::parse(&id).ok_or_else(|| {
            let ids: Vec<&str> = CHOICES.iter().map(|choice| choice.id()).collect();
            format!("unknown sleep choice {id:?}; expected one of {}", ids.join(", "))
        })
    }
}

impl From<Sleep> for String {
    fn from(sleep: Sleep) -> Self { sleep.id().to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_round_trip_and_cycle_both_ways() {
        for (index, choice) in CHOICES.into_iter().enumerate() {
            assert_eq!(choice as usize, index);
            assert_eq!(Sleep::parse(choice.id()), Some(choice));
            assert_eq!(choice.step(true).step(false), choice);
        }
        assert_eq!(Sleep::Off.step(false), Sleep::TwelveHours);
        assert_eq!(Sleep::Off.step(true), Sleep::OneHour);
        assert_eq!(Sleep::parse("4h"), None);
        assert_eq!(Sleep::TwoHours.limit(), Some(Duration::from_secs(7200)));
        assert_eq!(Sleep::Off.limit(), None);
    }

    #[test]
    fn stored_as_its_id() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct File { sleep: Sleep }
        let text = toml::to_string(&File { sleep: Sleep::ThreeHours }).unwrap();
        assert_eq!(text.trim(), "sleep = \"3h\"");
        assert_eq!(toml::from_str::<File>(&text).unwrap(), File { sleep: Sleep::ThreeHours });
        assert!(toml::from_str::<File>("sleep = \"forever\"").is_err());
    }
}
