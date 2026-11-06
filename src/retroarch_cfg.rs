//! RetroArch's configuration files: `key = "value"` lines.
//!
//! RetroArch rewrites its whole config when it exits, so Ghazal only changes a
//! few keys in place (the provisioned RetroAchievements account, the
//! Achievements switch, defaults changed since a config was made) and reads a
//! few back for the menu. Nothing here logs a
//! value: the keys it handles include passwords and login tokens.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Where provisioning leaves a RetroAchievements account until RetroArch next starts.
pub const ACCOUNT_SEED: &str = "/var/lib/ghazal/retroarch-account.cfg";
pub const CHEEVOS_ENABLE: &str = "cheevos_enable";
pub const CHEEVOS_HARDCORE: &str = "cheevos_hardcore_mode_enable";
pub const CHEEVOS_USERNAME: &str = "cheevos_username";
pub const CHEEVOS_PASSWORD: &str = "cheevos_password";
pub const CHEEVOS_TOKEN: &str = "cheevos_token";

/// Settings to write: keys of lowercase letters, digits and `_`, and values
/// RetroArch can read back from between quotes (no `"` or line breaks).
/// Only `TryFrom` builds one, so unchecked text never reaches a config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "BTreeMap<String, String>")]
pub struct Edits(BTreeMap<String, String>);

impl TryFrom<BTreeMap<String, String>> for Edits {
    type Error = String;

    fn try_from(settings: BTreeMap<String, String>) -> Result<Self, String> {
        // Errors name the key only: the value may be a password.
        match settings.iter().find(|(key, value)| !valid_key(key) || value.contains(['"', '\n', '\r', '\0'])) {
            Some((key, _)) => Err(format!("RetroArch setting {key:?} has an invalid name or a value with a quote or line break")),
            None => Ok(Self(settings)),
        }
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl Edits {
    #[cfg(test)]
    fn of<const N: usize>(pairs: [(&str, &str); N]) -> Result<Self, String> {
        pairs.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect::<BTreeMap<_, _>>().try_into()
    }

    /// The edits as a config file RetroArch can load with `--appendconfig`.
    pub fn render(&self) -> String {
        self.0.iter().map(|(key, value)| format!("{key} = \"{value}\"\n")).collect()
    }

    /// `text` with these keys set. Other lines stay as they are; a key
    /// RetroArch had written twice ends up once.
    pub fn apply(&self, text: &str) -> String {
        text.lines()
            .filter(|line| parse_line(line).is_none_or(|(key, _)| !self.0.contains_key(key)))
            .map(|line| format!("{line}\n"))
            .chain(std::iter::once(self.render()))
            .collect()
    }

    fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Reads a file of assignments. Anything that is not one is skipped.
    fn parse(text: &str) -> Result<Self, String> {
        text.lines().filter_map(parse_line).map(|(key, value)| (key.to_owned(), value.to_owned())).collect::<BTreeMap<_, _>>().try_into()
    }
}

/// `key = "value"` with the quotes removed; comments and other lines are `None`.
fn parse_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') { return None; }
    let (key, value) = line.split_once('=')?;
    let value = value.trim();
    Some((key.trim(), value.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(value)))
}

/// The value of `key`. RetroArch reads the first assignment.
pub fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().filter_map(parse_line).find(|(candidate, _)| *candidate == key).map(|(_, value)| value)
}

/// How RetroAchievements runs. Hardcore mode turns off save states, rewind and
/// cheats, which Ghazal's auto-resume and Menu + R1/L1 rely on, so the menu
/// offers it as a separate step from the default softcore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Softcore,
    Hardcore,
}

impl Mode {
    pub fn next(self) -> Self {
        match self { Self::Off => Self::Softcore, Self::Softcore => Self::Hardcore, Self::Hardcore => Self::Off }
    }

    /// Both keys are written: RetroArch's own default for hardcore is on.
    pub fn edits(self) -> Edits {
        let (enable, hardcore) = match self { Self::Off => ("false", "false"), Self::Softcore => ("true", "false"), Self::Hardcore => ("true", "true") };
        Edits(BTreeMap::from([(CHEEVOS_ENABLE.to_owned(), enable.to_owned()), (CHEEVOS_HARDCORE.to_owned(), hardcore.to_owned())]))
    }

    fn read(text: &str) -> Self {
        match (value(text, CHEEVOS_ENABLE), value(text, CHEEVOS_HARDCORE)) {
            (Some("true"), Some("false")) => Self::Softcore,
            // A config without the key gets RetroArch's default, which is hardcore.
            (Some("true"), _) => Self::Hardcore,
            _ => Self::Off,
        }
    }
}

/// Where the RetroAchievements login stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Account {
    Missing,
    /// A password is waiting: RetroArch signs in when a game loads, then keeps a token instead.
    SigningIn(String),
    SignedIn(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Achievements {
    pub mode: Mode,
    pub account: Account,
}

impl Default for Achievements {
    fn default() -> Self { Self { mode: Mode::Off, account: Account::Missing } }
}

impl Achievements {
    pub fn read(text: &str) -> Self {
        let filled = |key| value(text, key).filter(|value| !value.is_empty());
        let account = match (filled(CHEEVOS_USERNAME), filled(CHEEVOS_TOKEN), filled(CHEEVOS_PASSWORD)) {
            (Some(user), Some(_), _) => Account::SignedIn(user.to_owned()),
            (Some(user), None, Some(_)) => Account::SigningIn(user.to_owned()),
            _ => Account::Missing,
        };
        Self { mode: Mode::read(text), account }
    }

    /// The Configuration row's detail text.
    pub fn detail(&self) -> String {
        let mode = match self.mode { Mode::Off => "Off", Mode::Softcore => "On", Mode::Hardcore => "Hardcore" };
        match &self.account {
            Account::Missing => format!("{mode}, no account"),
            Account::SigningIn(user) => format!("{mode}, {user} (signs in at next game)"),
            Account::SignedIn(user) => format!("{mode}, {user}"),
        }
    }

    pub fn has_account(&self) -> bool { self.account != Account::Missing }
}

/// A RetroAchievements account given at provisioning time, to be merged into
/// RetroArch's config the next time it starts.
pub fn account_seed(username: &str, password: &str) -> Result<Edits> {
    // RetroAchievements names are short; the bound just keeps garbage out of the config.
    if username.is_empty() || username.len() > 64 || password.is_empty() || password.len() > 128 {
        bail!("invalid RetroAchievements account values");
    }
    let mut seed = BTreeMap::from([
        (CHEEVOS_USERNAME.to_owned(), username.to_owned()),
        (CHEEVOS_PASSWORD.to_owned(), password.to_owned()),
        // A token belongs to one account: sign in again with this password.
        (CHEEVOS_TOKEN.to_owned(), String::new()),
    ]);
    seed.extend(Mode::Softcore.edits().0);
    Edits::try_from(seed).map_err(|_| anyhow::anyhow!("invalid RetroAchievements account values"))
}

/// Defaults Ghazal changed after players' RetroArch configs already existed,
/// oldest first. RetroArch saves every setting when it exits, so a new template
/// value never reaches an existing config: each change is applied once, and a
/// player who sets it back keeps their choice. Append only.
const DEFAULT_CHANGES: &[(&str, &str)] = &[
    // Nintendo-style menu like Ghazal's own: A (right) confirms, B (bottom) goes back.
    ("menu_swap_ok_cancel_buttons", "false"),
    // Menu + Select screenshots (gamepad.cfg) land beside the saves, not the ROMs.
    ("screenshot_directory", "/mnt/games/SAVES/screenshots"),
];

/// Counts the default changes applied to `user_config`; a count written by a
/// newer OS is higher than this one knows and changes nothing.
fn default_changes_counter(user_config: &Path) -> std::path::PathBuf {
    user_config.with_file_name("ghazal-defaults-applied")
}

/// A config made from the template already has every changed default.
pub fn record_default_changes(user_config: &Path) -> Result<()> {
    crate::sys::write_atomic(&default_changes_counter(user_config), DEFAULT_CHANGES.len().to_string().as_bytes())
}

/// Applies the default changes `user_config` has not had yet.
pub fn apply_default_changes(user_config: &Path) -> Result<()> {
    let applied = match std::fs::read_to_string(default_changes_counter(user_config)) {
        Ok(text) => text.trim().parse::<usize>().context("reading how many RetroArch default changes are applied")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error).context("reading how many RetroArch default changes are applied"),
    };
    let Some(pending) = DEFAULT_CHANGES.get(applied..).filter(|pending| !pending.is_empty()) else { return Ok(()) };
    let edits = Edits::try_from(pending.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect::<BTreeMap<_, _>>())
        .map_err(anyhow::Error::msg)?;
    let current = std::fs::read_to_string(user_config).context("reading RetroArch's config")?;
    // Private: the config can hold a RetroAchievements login token.
    crate::sys::write_atomic_private(user_config, edits.apply(&current).as_bytes())?;
    record_default_changes(user_config)
}

/// Merges a pending account seed into RetroArch's user config and deletes the
/// seed once the config is written. Returns whether a seed was applied.
/// The config is rewritten privately: it holds the password until RetroArch
/// has signed in, and a login token afterwards.
pub fn apply_seed(user_config: &Path, seed: &Path) -> Result<bool> {
    let text = match std::fs::read_to_string(seed) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("reading the pending RetroArch account"),
    };
    let edits = Edits::parse(&text).map_err(|_| anyhow::anyhow!("the pending RetroArch account is malformed"))?;
    if let Some(key) = edits.keys().find(|key| !key.starts_with("cheevos_")) {
        bail!("the pending RetroArch account sets {key}, which is not an achievements setting");
    }
    let current = std::fs::read_to_string(user_config).unwrap_or_default();
    crate::sys::write_atomic_private(user_config, edits.apply(&current).as_bytes())?;
    std::fs::remove_file(seed).context("removing the applied RetroArch account")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ghazal-cfg-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn edits_replace_keys_in_place_and_leave_other_lines_alone() {
        let before = "# mine\nvideo_driver = \"gl\"\ncheevos_enable = \"false\"\ncheevos_enable = \"false\"\naudio_driver = \"alsa\"\n";
        let after = Edits::of([("cheevos_enable", "true"), ("cheevos_username", "Sharno")]).unwrap().apply(before);
        assert_eq!(after, "# mine\nvideo_driver = \"gl\"\naudio_driver = \"alsa\"\ncheevos_enable = \"true\"\ncheevos_username = \"Sharno\"\n");
        assert_eq!(value(&after, "cheevos_enable"), Some("true"));
        assert_eq!(value(&after, "missing"), None);
        // A file RetroArch wrote without a final newline still gets separate lines.
        assert_eq!(Edits::of([("a", "1")]).unwrap().apply("b = \"2\""), "b = \"2\"\na = \"1\"\n");
    }

    #[test]
    fn values_cannot_break_out_of_their_quotes() {
        assert!(Edits::of([("cheevos_password", "pa\"ss")]).is_err());
        assert!(Edits::of([("cheevos_password", "pa\nss")]).is_err());
        assert!(Edits::of([("Bad-Key", "x")]).is_err());
        assert!(Edits::of([("", "x")]).is_err());
        let error = Edits::of([("cheevos_password", "hunter\"2")]).unwrap_err();
        assert!(!error.contains("hunter"), "errors must not echo values");
        assert!(toml::from_str::<Edits>("netplay_nat_traversal = \"false\"").is_ok());
        assert!(toml::from_str::<Edits>("netplay_nat_traversal = \"fa\\\"lse\"").is_err());
    }

    #[test]
    fn achievements_mode_cycles_and_round_trips_through_the_config() {
        assert_eq!([Mode::Off.next(), Mode::Softcore.next(), Mode::Hardcore.next()], [Mode::Softcore, Mode::Hardcore, Mode::Off]);
        for mode in [Mode::Off, Mode::Softcore, Mode::Hardcore] {
            assert_eq!(Achievements::read(&mode.edits().render()).mode, mode);
        }
        // RetroArch's default for a missing hardcore key is on.
        assert_eq!(Achievements::read("cheevos_enable = \"true\"\n").mode, Mode::Hardcore);
        assert_eq!(Achievements::read("").mode, Mode::Off);
    }

    #[test]
    fn account_state_follows_username_token_and_password() {
        let read = |text: &str| Achievements::read(text).account;
        assert_eq!(read(""), Account::Missing);
        assert_eq!(read("cheevos_username = \"\"\ncheevos_token = \"abc\"\n"), Account::Missing);
        assert_eq!(read("cheevos_username = \"sharno\"\ncheevos_password = \"x\"\ncheevos_token = \"\"\n"), Account::SigningIn("sharno".into()));
        assert_eq!(read("cheevos_username = \"sharno\"\ncheevos_password = \"\"\ncheevos_token = \"abc\"\n"), Account::SignedIn("sharno".into()));
        let status = Achievements { mode: Mode::Softcore, account: Account::SignedIn("sharno".into()) };
        assert_eq!(status.detail(), "On, sharno");
        assert_eq!(Achievements::default().detail(), "Off, no account");
    }

    #[test]
    fn a_provisioned_account_is_merged_once_and_the_seed_is_removed() {
        let dir = temp("seed");
        let (config, seed) = (dir.join("retroarch.cfg"), dir.join("seed.cfg"));
        std::fs::write(&config, "video_driver = \"gl\"\ncheevos_token = \"stale\"\ncheevos_hardcore_mode_enable = \"true\"\n").unwrap();
        crate::sys::write_atomic_private(&seed, account_seed("sharno", "s3cret pass").unwrap().render().as_bytes()).unwrap();

        assert!(apply_seed(&config, &seed).unwrap());
        let merged = std::fs::read_to_string(&config).unwrap();
        assert_eq!(value(&merged, "video_driver"), Some("gl"));
        assert_eq!(value(&merged, CHEEVOS_USERNAME), Some("sharno"));
        assert_eq!(value(&merged, CHEEVOS_PASSWORD), Some("s3cret pass"));
        assert_eq!(value(&merged, CHEEVOS_TOKEN), Some(""));
        assert_eq!(Achievements::read(&merged).mode, Mode::Softcore);
        assert_eq!(Achievements::read(&merged).account, Account::SigningIn("sharno".into()));
        assert!(!seed.exists());
        assert!(!apply_seed(&config, &seed).unwrap());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o077, 0, "the config holds a password");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_seed_may_only_touch_achievements_settings() {
        let dir = temp("hostile");
        let (config, seed) = (dir.join("retroarch.cfg"), dir.join("seed.cfg"));
        std::fs::write(&config, "video_driver = \"gl\"\n").unwrap();
        std::fs::write(&seed, "system_directory = \"/\"\n").unwrap();
        assert!(apply_seed(&config, &seed).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "video_driver = \"gl\"\n");
        assert!(seed.exists());
        assert!(account_seed("", "pw").is_err());
        assert!(account_seed("name", "p\"w").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_user_config_is_created_from_the_seed() {
        let dir = temp("fresh");
        let (config, seed) = (dir.join("nested/retroarch.cfg"), dir.join("seed.cfg"));
        std::fs::write(&seed, account_seed("sharno", "pw123456").unwrap().render()).unwrap();
        assert!(apply_seed(&config, &seed).unwrap());
        assert_eq!(value(&std::fs::read_to_string(&config).unwrap(), CHEEVOS_ENABLE), Some("true"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_changes_apply_once_and_then_leave_the_players_choice() {
        let dir = temp("defaults");
        let config = dir.join("retroarch.cfg");
        std::fs::write(&config, "video_driver = \"gl\"\nmenu_swap_ok_cancel_buttons = \"true\"\n").unwrap();
        apply_default_changes(&config).unwrap();
        assert_eq!(value(&std::fs::read_to_string(&config).unwrap(), "menu_swap_ok_cancel_buttons"), Some("false"));
        // The player swaps it back in RetroArch's menu: that choice stays.
        std::fs::write(&config, "menu_swap_ok_cancel_buttons = \"true\"\n").unwrap();
        apply_default_changes(&config).unwrap();
        assert_eq!(value(&std::fs::read_to_string(&config).unwrap(), "menu_swap_ok_cancel_buttons"), Some("true"));
        // A newer OS slot applied more changes than this one knows.
        std::fs::write(dir.join("ghazal-defaults-applied"), "99").unwrap();
        apply_default_changes(&config).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "menu_swap_ok_cancel_buttons = \"true\"\n");
        // A config made from the template is left as the template made it.
        std::fs::remove_file(dir.join("ghazal-defaults-applied")).unwrap();
        record_default_changes(&config).unwrap();
        apply_default_changes(&config).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "menu_swap_ok_cancel_buttons = \"true\"\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The template already has every changed default, so a fresh config needs none of them.
    #[test]
    fn the_template_has_every_changed_default() {
        let template = include_str!("../config/rg40xxv-rocknix/retroarch.cfg");
        for (key, changed) in DEFAULT_CHANGES {
            assert!(valid_key(key) && !changed.contains('"'), "{key}");
            assert_eq!(value(template, key), Some(*changed), "{key}");
        }
    }
}
