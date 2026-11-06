//! The handheld's time zone: one symlink on the persistent `/var`. The image's
//! `/etc/localtime` points through it, so every program started afterwards
//! keeps local time in the chosen zone. Without the link, time is UTC.
//!
//! The menu reads the zone's rules itself instead of asking libc: musl and
//! glibc load `/etc/localtime` once per process, and the menu's clock has to
//! change as soon as a zone is chosen.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// The zone files, as the runtime ships them (`nix/runtime`): the zones
/// `zone.tab` lists, plus `UTC`.
const ZONEINFO: &str = "/usr/share/zoneinfo";
const LINK: &str = "/var/lib/ghazal/localtime";

/// A zone file's name under the zone directory, like `America/New_York`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Zone(String);

impl Zone {
    /// Takes names like `zone.tab`'s: relative, with no empty, `.` or `..` part.
    pub fn parse(name: &str) -> Option<Self> {
        name.split('/').all(|part| !matches!(part, "" | "." | "..")).then(|| Self(name.to_owned()))
    }

    pub fn utc() -> Self { Self("UTC".to_owned()) }

    pub fn name(&self) -> &str { &self.0 }

    /// The continent or ocean it is filed under; `UTC` is its own.
    pub fn region(&self) -> &str { self.0.split_once('/').map_or(&self.0, |(region, _)| region) }

    /// The place, as people write it: `America/Argentina/Buenos_Aires` is
    /// `Argentina / Buenos Aires`.
    pub fn place(&self) -> String {
        self.0.split_once('/').map_or(&*self.0, |(_, place)| place).replace('_', " ").replace('/', " / ")
    }
}

/// The zones in a `zone.tab`, sorted, with UTC first. Several countries can
/// share a zone; it is listed once.
pub fn zones(table: &str) -> Vec<Zone> {
    let mut listed: Vec<Zone> = table.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split('\t').nth(2).and_then(Zone::parse))
        .collect();
    listed.sort();
    listed.dedup();
    std::iter::once(Zone::utc()).chain(listed).collect()
}

/// Where the zone files and the chosen link are.
pub struct Store {
    zoneinfo: PathBuf,
    link: PathBuf,
}

impl Store {
    pub fn system() -> Self { Self { zoneinfo: ZONEINFO.into(), link: LINK.into() } }

    /// Whether this image has zone files to choose from.
    pub fn available(&self) -> bool { self.zoneinfo.join("zone.tab").exists() }

    pub fn zones(&self) -> Result<Vec<Zone>> {
        let path = self.zoneinfo.join("zone.tab");
        Ok(zones(&std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?))
    }

    /// The chosen zone. `None` when nothing was chosen (UTC), or the link
    /// was pointed somewhere else by hand.
    pub fn current(&self) -> Option<Zone> {
        let target = std::fs::read_link(&self.link).ok()?;
        Zone::parse(target.strip_prefix(&self.zoneinfo).ok()?.to_str()?)
    }

    /// Points the link at `zone`, replacing it in one step.
    pub fn choose(&self, zone: &Zone) -> Result<()> {
        let pending = crate::sys::atomic_pending(&self.link);
        let _ = std::fs::remove_file(&pending);
        std::os::unix::fs::symlink(self.zoneinfo.join(zone.name()), &pending)?;
        std::fs::rename(&pending, &self.link)?;
        std::fs::File::open(self.link.parent().context("time zone link has no parent")?)?.sync_all()?;
        Ok(())
    }

    /// The chosen zone's rules; UTC when nothing was chosen.
    pub fn rules(&self) -> Result<Rules> {
        let Some(zone) = self.current() else { return Ok(Rules::utc()) };
        let path = self.zoneinfo.join(zone.name());
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Rules(tz::TimeZone::from_tz_data(&bytes).with_context(|| format!("{} is not a zone file", path.display()))?))
    }
}

/// A zone's offsets from UTC over time.
pub struct Rules(tz::TimeZone);

impl Rules {
    pub fn utc() -> Self { Self(tz::TimeZone::utc()) }

    /// Seconds east of UTC at `unix` seconds.
    pub fn offset(&self, unix: i64) -> i32 {
        // Out of range only for a zone with no local time types at all, which
        // `from_tz_data` refuses.
        self.0.find_local_time_type(unix).map_or(0, |kind| kind.ut_offset())
    }

    /// The wall clock at `unix` seconds, as (hour, minute).
    pub fn wall_clock(&self, unix: i64) -> (u8, u8) {
        let seconds = (unix + i64::from(self.offset(unix))).rem_euclid(86_400);
        ((seconds / 3600) as u8, (seconds / 60 % 60) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "# tz zone descriptions\n\
                         AR\t-3436-05825\tAmerica/Argentina/Buenos_Aires\tBuenos Aires (BA, CF)\n\
                         US\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\n\
                         EG\t+3003+03115\tAfrica/Cairo\n\
                         CA\t+4339-07923\tAmerica/Toronto\tEastern - ON & QC (most areas)\n\
                         US\t+404251-0740023\tAmerica/New_York\n";

    #[test]
    fn the_table_lists_each_zone_once_after_utc() {
        let names: Vec<String> = zones(TABLE).iter().map(|zone| zone.name().to_owned()).collect();
        assert_eq!(names, ["UTC", "Africa/Cairo", "America/Argentina/Buenos_Aires", "America/New_York", "America/Toronto"]);
    }

    #[test]
    fn names_split_into_a_region_and_a_place() {
        let zone = Zone::parse("America/Argentina/Buenos_Aires").unwrap();
        assert_eq!((zone.region(), zone.place().as_str()), ("America", "Argentina / Buenos Aires"));
        assert_eq!((Zone::utc().region(), Zone::utc().place().as_str()), ("UTC", "UTC"));
    }

    #[test]
    fn names_that_leave_the_zone_directory_are_refused() {
        for name in ["../etc/passwd", "/etc/localtime", "America//New_York", "America/./New_York", ""] {
            assert_eq!(Zone::parse(name), None, "{name}");
        }
    }

    /// New York's rules as a TZif footer only: what slim zone files hold for recent years.
    fn new_york() -> Rules {
        let header = |version: u8, counts: [u32; 6]| -> Vec<u8> {
            [b"TZif".as_slice(), &[version], &[0; 15], &counts.map(u32::to_be_bytes).concat()].concat()
        };
        // One local time type (EST, -5 h) and its designation.
        let body = [(-18_000_i32).to_be_bytes().as_slice(), &[0, 0], b"EST\0"].concat();
        let v1 = [header(b'2', [0, 0, 0, 0, 1, 4]), body.clone()].concat();
        let v2 = [header(b'2', [0, 0, 0, 0, 1, 4]), body, b"\nEST5EDT,M3.2.0,M11.1.0\n".to_vec()].concat();
        Rules(tz::TimeZone::from_tz_data(&[v1, v2].concat()).unwrap())
    }

    #[test]
    fn the_wall_clock_follows_daylight_saving() {
        // 2026-11-05 19:08 UTC is 14:08 in New York (EST); 2026-07-01 12:00 UTC is 08:00 (EDT).
        assert_eq!(new_york().wall_clock(1_793_905_680), (14, 8));
        assert_eq!(new_york().wall_clock(1_782_907_200), (8, 0));
        assert_eq!(Rules::utc().wall_clock(1_793_905_680), (19, 8));
    }

    #[test]
    fn choosing_a_zone_points_the_link_at_its_file() {
        let root = std::env::temp_dir().join(format!("ghazal-timezone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("zoneinfo")).unwrap();
        std::fs::create_dir_all(root.join("var")).unwrap();
        let store = Store { zoneinfo: root.join("zoneinfo"), link: root.join("var/localtime") };
        assert_eq!(store.current(), None);
        for name in ["Africa/Cairo", "America/New_York"] {
            store.choose(&Zone::parse(name).unwrap()).unwrap();
            assert_eq!(store.current().as_ref().map(Zone::name), Some(name));
        }
        assert_eq!(std::fs::read_link(root.join("var/localtime")).unwrap(), root.join("zoneinfo/America/New_York"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
