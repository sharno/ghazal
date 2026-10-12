use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ModelRc, SharedString, VecModel};

use super::activity::{Activity, Catalog, Collection, GameId};
use super::config::FrontendConfig;
use super::input::Button;
use super::launcher::LaunchSpec;
use super::library::{Game, Library, System};
use crate::daemon::ctl;
use crate::hw::battery;
use crate::ui::{AppWindow, Hint, Row};

const TOAST_DURATION: Duration = Duration::from_secs(3);
const STATUS_REFRESH: Duration = Duration::from_secs(20);
/// The daemon only needs to know the user is present, not every key repeat.
const ACTIVITY_REPORT: Duration = Duration::from_secs(1);
const SCAN_POLL: Duration = Duration::from_millis(50);
const PAGE: isize = 8;
/// Rows handed to the UI at once, and how close the cursor may get to either end.
const ROW_WINDOW: usize = 48;
const ROW_MARGIN: usize = 16;

pub enum Effect {
    Launch { spec: LaunchSpec, game: GameId },
    Quit,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Systems,
    Games(GameList),
}

/// A list of games. Also each entry of the systems list.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GameList {
    Favorites,
    Recent,
    System(usize),
}

impl GameList {
    /// Collections come first in the systems list.
    const COLLECTIONS: [GameList; 2] = [GameList::Favorites, GameList::Recent];

    fn at(index: usize) -> Self {
        match Self::COLLECTIONS.get(index) {
            Some(collection) => *collection,
            None => Self::System(index - Self::COLLECTIONS.len()),
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Favorites => 0,
            Self::Recent => 1,
            Self::System(system) => system + Self::COLLECTIONS.len(),
        }
    }
}

#[derive(Clone, Copy)]
enum MenuItem {
    Rescan,
    Sleep,
    Restart,
    PowerOff,
    Quit,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            Self::Rescan => "Rescan library",
            Self::Sleep => "Sleep",
            Self::Restart => "Restart",
            Self::PowerOff => "Power off",
            Self::Quit => "Quit oxmux",
        }
    }
}

type Hints = &'static [(&'static str, &'static str)];
const MENU_HINTS: Hints = &[("A", "Select"), ("B", "Close")];
const SYSTEM_HINTS: Hints = &[("A", "Open"), ("≡", "Menu")];
const GAME_HINTS: Hints = &[("A", "Play"), ("B", "Back"), ("Y", "Favorite"), ("◀▶", "System"), ("≡", "Menu")];
const COLLECTION_HINTS: Hints = &[("A", "Play"), ("B", "Back"), ("Y", "Favorite"), ("≡", "Menu")];

struct Toast {
    text: String,
    shown: Instant,
    error: bool,
}

struct Scan {
    results: std::sync::mpsc::Receiver<Library>,
    announce: bool,
}

/// All frontend state. After every change `sync()` pushes it into the Slint window,
/// which decides how it looks (ui/app.slint).
pub struct App {
    config: FrontendConfig,
    ui: AppWindow,
    menu_items: Vec<MenuItem>,
    catalog: Catalog,
    scan: Option<Scan>,
    view: View,
    /// Cursor in the systems list.
    system_cursor: usize,
    /// Remembered cursor per game list.
    game_cursors: BTreeMap<GameList, usize>,
    /// Cursor in the open menu.
    menu: Option<usize>,
    toast: Option<Toast>,
    levels: Option<crate::levels::Levels>,
    levels_visible_until: Option<Instant>,
    status_checked: Instant,
    activity_reported: Option<Instant>,
    /// Which view the row model currently shows, so it's only rebuilt on change.
    rows_for: Option<View>,
    row_start: usize,
    hints_for: Option<Hints>,
}

impl App {
    pub fn new(config: FrontendConfig, ui: AppWindow) -> anyhow::Result<Self> {
        // Damaged favorites/history must not keep the menu from starting.
        let (activity, activity_warning) = match Activity::load(&config.activity_file) {
            Ok(activity) => (activity, None),
            Err(error) => {
                let aside = Activity::set_aside(&config.activity_file)?;
                eprintln!("oxmux: {error:#}; moved it to {}", aside.display());
                (Activity::load(&config.activity_file)?, Some(format!("Favorites and history were unreadable; kept them as {}", aside.display())))
            }
        };
        // Power entries need the daemon; Quit only makes sense when launched from muOS.
        let mut menu_items = vec![MenuItem::Rescan];
        if config.daemon_socket.exists() {
            if ctl::request(&config.daemon_socket, "status").is_ok_and(|reply| reply.split_whitespace().any(|field| field == "suspend=1")) {
                menu_items.push(MenuItem::Sleep);
            }
            menu_items.extend([MenuItem::Restart, MenuItem::PowerOff]);
        }
        if std::env::var_os("OXMUX_EMBEDDED").is_none() {
            menu_items.push(MenuItem::Quit);
        }
        ui.set_menu_items(ModelRc::new(VecModel::from(
            menu_items.iter().map(|m| SharedString::from(m.label())).collect::<Vec<_>>(),
        )));

        let mut app = Self {
            config,
            ui,
            menu_items,
            // Draw the menu first; a cold SD-card scan of thousands of games takes ~0.5 s.
            catalog: Catalog::new(Library::from_systems(Vec::new()), activity),
            scan: None,
            view: View::Systems,
            system_cursor: 0,
            game_cursors: BTreeMap::new(),
            menu: None,
            toast: None,
            levels: None,
            levels_visible_until: None,
            status_checked: Instant::now(),
            activity_reported: None,
            rows_for: None,
            row_start: 0,
            hints_for: None,
        };
        app.start_scan(false);
        if let Some(warning) = activity_warning { app.error(warning); }
        app.refresh_status(Instant::now());
        app.refresh_levels();
        app.sync();
        Ok(app)
    }

    pub fn update(&mut self, button: Button) -> Option<Effect> {
        self.report_activity();
        let effect = self.handle(button);
        self.sync();
        effect
    }

    /// The frontend may grab the gamepad, hiding input from the daemon's idle timer.
    fn report_activity(&mut self) {
        let now = Instant::now();
        if self.activity_reported.is_some_and(|at| now < at + ACTIVITY_REPORT) || !self.config.daemon_socket.exists() {
            return;
        }
        self.activity_reported = Some(now);
        if let Err(error) = ctl::request(&self.config.daemon_socket, "activity") {
            eprintln!("oxmux: reporting input activity: {error:#}");
        }
    }

    fn handle(&mut self, button: Button) -> Option<Effect> {
        if let Some(cursor) = self.menu {
            match button {
                Button::Up => self.menu = Some(step(cursor, self.menu_items.len(), -1, true)),
                Button::Down => self.menu = Some(step(cursor, self.menu_items.len(), 1, true)),
                Button::B | Button::Menu | Button::Start => self.menu = None,
                Button::A => {
                    self.menu = None;
                    return self.activate(self.menu_items[cursor]);
                }
                _ => {}
            }
            return None;
        }
        if matches!(button, Button::Menu | Button::Start) {
            self.menu = Some(0);
            return None;
        }
        match self.view {
            View::Systems => match button {
                Button::A => self.view = View::Games(GameList::at(self.system_cursor)),
                button => self.system_cursor = navigate(self.system_cursor, self.system_entries(), button),
            },
            View::Games(list) => {
                let len = self.game_count(list);
                let cursor = self.cursor(list);
                match button {
                    Button::B => self.view = View::Systems,
                    Button::A => return self.launch(list, cursor),
                    Button::Y => self.toggle_favorite(list, cursor),
                    // Left/Right in a system's games jump to the neighbouring system.
                    Button::Left | Button::Right => if let GameList::System(system) = list {
                        let next = step(system, self.catalog.library().systems().len(), if button == Button::Left { -1 } else { 1 }, true);
                        let next = GameList::System(next);
                        self.system_cursor = next.index();
                        self.view = View::Games(next);
                    },
                    button => { self.game_cursors.insert(list, navigate(cursor, len, button)); }
                }
            }
        }
        None
    }

    fn launch(&mut self, list: GameList, cursor: usize) -> Option<Effect> {
        let (system, game) = self.game_at(list, cursor)?;
        let id = GameId::new(system, game);
        match LaunchSpec::new(&self.config, system, game) {
            Ok(spec) => Some(Effect::Launch { spec, game: id }),
            Err(error) => {
                self.error(format!("{error:#}"));
                None
            }
        }
    }

    fn toggle_favorite(&mut self, list: GameList, cursor: usize) {
        let Some((system, game)) = self.game_at(list, cursor) else { return };
        let id = GameId::new(system, game);
        match self.catalog.toggle_favorite(id) {
            Ok(added) => self.notice(if added { "Added to Favorites" } else { "Removed from Favorites" }),
            Err(error) => self.error(format!("Saving favorite: {error:#}")),
        }
        self.rows_for = None;
    }

    pub fn launch_started(&mut self, game: GameId) {
        // Persist before waiting for the emulator: history survives a reset
        // during gameplay, and failed spawns never enter this callback.
        if let Err(error) = self.catalog.launched(game.clone()) {
            eprintln!("oxmux: saving history: {error:#}");
            self.error(format!("Saving history: {error:#}"));
        }
        // Launching reorders Recent Games; keep the cursor on the game played.
        if let View::Games(list) = self.view {
            if let Some(index) = (0..self.game_count(list)).find(|&i| self.game_at(list, i).is_some_and(|(system, g)| GameId::new(system, g) == game)) {
                self.game_cursors.insert(list, index);
            }
        }
        self.rows_for = None;
    }

    pub fn launch_finished(&mut self, result: anyhow::Result<std::process::ExitStatus>) {
        let result = result.and_then(|status| {
            if status.success() { Ok(()) } else { anyhow::bail!("emulator exited with {status}") }
        });
        if let Err(e) = result {
            eprintln!("oxmux: launch failed: {e:#}");
            self.error(format!("{e:#}"));
        }
        self.refresh_status(Instant::now());
        self.sync();
    }

    /// Periodic housekeeping: scan results, toast expiry, clock and battery.
    pub fn tick(&mut self, now: Instant) {
        if self.levels_visible_until.is_some_and(|until| now >= until) {
            self.levels_visible_until = None;
        }
        self.receive_scan();
        if self.toast.as_ref().is_some_and(|toast| now >= toast.shown + TOAST_DURATION) {
            self.toast = None;
        }
        if now >= self.status_checked + STATUS_REFRESH {
            self.refresh_status(now);
        }
        self.sync();
    }

    fn receive_scan(&mut self) {
        let Some(scan) = &self.scan else { return };
        let library = match scan.results.try_recv() {
            Ok(library) => library,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.scan = None;
                self.error("Library scan worker failed".into());
                return;
            }
        };
        let announce = self.scan.take().is_some_and(|scan| scan.announce);
        eprintln!("oxmux: {} systems, {} games", library.systems().len(), library.game_count());
        if announce {
            self.notice(&format!("Found {} games", library.game_count()));
        }
        // System indices may have changed; keep only the collections' cursors.
        self.game_cursors.retain(|list, _| !matches!(list, GameList::System(_)));
        if matches!(self.view, View::Games(GameList::System(_))) {
            self.view = View::Systems;
        }
        self.catalog.replace_library(library);
        self.system_cursor = self.system_cursor.min(self.system_entries() - 1);
        self.rows_for = None;
    }

    pub fn next_deadline(&self) -> Instant {
        let mut deadline = if self.scan.is_some() { Instant::now() + SCAN_POLL } else { self.status_checked + STATUS_REFRESH };
        if let Some(until) = self.levels_visible_until { deadline = deadline.min(until); }
        if let Some(toast) = &self.toast { deadline = deadline.min(toast.shown + TOAST_DURATION); }
        deadline
    }

    pub fn refresh_levels(&mut self) {
        match crate::levels::read(&self.config.daemon_socket) {
            Ok(Some(levels)) => {
                if self.levels.is_some_and(|previous| previous != levels) {
                    self.levels_visible_until = Some(Instant::now() + TOAST_DURATION);
                }
                self.levels = Some(levels);
                self.ui.set_brightness_level(u8::from(levels.brightness).into());
                self.ui.set_volume_level(u8::from(levels.volume).into());
                self.sync();
            }
            Ok(None) => {},
            Err(error) => eprintln!("oxmux: reading live levels: {error:#}"),
        }
    }

    fn refresh_status(&mut self, now: Instant) {
        self.status_checked = now;
        let (h, m) = battery::local_time();
        self.ui.set_clock(format!("{h:02}:{m:02}").into());
        match battery::read(None) {
            Some(b) => {
                self.ui.set_battery_percent(b.percent as i32);
                self.ui.set_battery_charging(b.charging);
            }
            None => self.ui.set_battery_percent(-1),
        }
    }

    /// Scans in a worker so input and drawing continue. Only user-requested
    /// rescans announce their result.
    fn start_scan(&mut self, announce: bool) -> bool {
        let config = self.config.clone();
        let (sender, results) = std::sync::mpsc::channel();
        match std::thread::Builder::new().name("oxmux-library".into()).spawn(move || {
            let _ = sender.send(Library::scan(&config));
        }) {
            Ok(_) => {
                self.scan = Some(Scan { results, announce });
                true
            }
            Err(error) => {
                self.error(format!("Starting library scan: {error}"));
                false
            }
        }
    }

    fn activate(&mut self, item: MenuItem) -> Option<Effect> {
        match item {
            MenuItem::Rescan => {
                if self.scan.is_none() && self.start_scan(true) {
                    self.notice("Scanning library…");
                }
                None
            }
            MenuItem::Sleep => self.power("suspend"),
            MenuItem::Restart => self.power("reboot"),
            MenuItem::PowerOff => self.power("poweroff"),
            MenuItem::Quit => Some(Effect::Quit),
        }
    }

    /// Asks the daemon to sleep/reboot/power off.
    fn power(&mut self, command: &str) -> Option<Effect> {
        if let Err(e) = ctl::request(&self.config.daemon_socket, command) {
            self.error(format!("{e:#}"));
        }
        None
    }

    fn notice(&mut self, text: &str) {
        self.toast = Some(Toast { text: text.into(), shown: Instant::now(), error: false });
    }

    fn error(&mut self, text: String) {
        self.toast = Some(Toast { text, shown: Instant::now(), error: true });
    }

    fn system_entries(&self) -> usize {
        GameList::COLLECTIONS.len() + self.catalog.library().systems().len()
    }

    fn game_count(&self, list: GameList) -> usize {
        match list {
            GameList::System(index) => self.catalog.library().systems().get(index).map_or(0, |system| system.games.len()),
            GameList::Favorites => self.catalog.collection_len(Collection::Favorites),
            GameList::Recent => self.catalog.collection_len(Collection::Recent),
        }
    }

    fn game_at(&self, list: GameList, index: usize) -> Option<(&System, &Game)> {
        match list {
            GameList::System(system) => {
                let system = self.catalog.library().systems().get(system)?;
                Some((system, system.games.get(index)?))
            }
            GameList::Favorites => self.catalog.collection_game(Collection::Favorites, index),
            GameList::Recent => self.catalog.collection_game(Collection::Recent, index),
        }
    }

    /// The list's remembered cursor, kept inside the list as it shrinks.
    fn cursor(&self, list: GameList) -> usize {
        self.game_cursors.get(&list).copied().unwrap_or(0).min(self.game_count(list).saturating_sub(1))
    }

    fn list_title(&self, list: GameList) -> &str {
        match list {
            GameList::System(index) => self.catalog.library().systems().get(index).map_or("", |system| system.name.as_str()),
            GameList::Favorites => "Favorites",
            GameList::Recent => "Recent Games",
        }
    }

    fn row(&self, view: View, index: usize) -> Option<Row> {
        Some(match view {
            View::Systems => {
                let list = GameList::at(index);
                Row { label: self.list_title(list).into(), detail: self.game_count(list).to_string().into() }
            }
            View::Games(list) => {
                let (system, game) = self.game_at(list, index)?;
                let favorite = if self.catalog.is_favorite(&GameId::new(system, game)) { "★" } else { "" };
                // Collections mix systems, so name the game's system.
                let detail = match list {
                    GameList::System(_) => favorite.to_owned(),
                    _ => format!("{favorite} {}", system.name).trim_start().to_owned(),
                };
                Row { label: game.title.as_str().into(), detail: detail.into() }
            }
        })
    }

    fn hints(&self) -> Hints {
        match (self.menu, self.view) {
            (Some(_), _) => MENU_HINTS,
            (None, View::Systems) => SYSTEM_HINTS,
            (None, View::Games(GameList::System(_))) => GAME_HINTS,
            (None, View::Games(_)) => COLLECTION_HINTS,
        }
    }

    /// Pushes state into the UI. Slint only repaints what actually changed.
    fn sync(&mut self) {
        let (cursor, total) = match self.view {
            View::Systems => (self.system_cursor, self.system_entries()),
            View::Games(list) => (self.cursor(list), self.game_count(list)),
        };

        // The UI only gets a window of ROW_WINDOW rows around the cursor. It's moved
        // once the cursor gets within ROW_MARGIN of either edge, so the rows that are
        // on screen are always inside it.
        let max_start = total.saturating_sub(ROW_WINDOW);
        let s = self.row_start;
        let window_ok = self.rows_for == Some(self.view)
            && (s == 0 || cursor >= s + ROW_MARGIN)
            && (s == max_start || cursor < s + ROW_WINDOW - ROW_MARGIN);
        if !window_ok {
            let start = cursor.saturating_sub(ROW_WINDOW / 2).min(max_start);
            let rows: Vec<Row> = (start..(start + ROW_WINDOW).min(total)).filter_map(|index| self.row(self.view, index)).collect();
            self.ui.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
            self.ui.set_row_offset(start as i32);
            self.ui.set_row_count(total as i32);
            self.row_start = start;
            self.rows_for = Some(self.view);
        }

        let ui = &self.ui;
        ui.set_heading(match self.view {
            View::Systems => "Systems",
            View::Games(list) => self.list_title(list),
        }.into());
        ui.set_position(if total > 0 { format!("{} / {total}", cursor + 1) } else { String::new() }.into());
        ui.set_empty_message(match self.view {
            _ if self.scan.is_some() => "Loading games…".to_owned(),
            View::Games(GameList::Favorites) => "Press Y on a game to add a favorite".to_owned(),
            View::Games(GameList::Recent) => "Games you launch will appear here".to_owned(),
            _ => format!("No games found in {}", self.config.rom_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")),
        }.into());
        ui.set_selected(cursor as i32);
        ui.set_menu_selected(self.menu.map_or(-1, |m| m as i32));

        let (toast, is_error) = self.toast.as_ref().map_or(("", false), |toast| (toast.text.as_str(), toast.error));
        ui.set_toast(toast.into());
        ui.set_toast_error(is_error);
        ui.set_levels_visible(self.levels.is_some() && (self.menu.is_some() || self.levels_visible_until.is_some()));

        let hints = self.hints();
        if self.hints_for != Some(hints) {
            let model: Vec<Hint> = hints.iter().map(|(glyph, label)| Hint { glyph: (*glyph).into(), label: (*label).into() }).collect();
            ui.set_hints(ModelRc::new(VecModel::from(model)));
            self.hints_for = Some(hints);
        }
    }
}

fn step(i: usize, len: usize, delta: isize, wrap: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let next = i as isize + delta;
    if wrap {
        next.rem_euclid(len as isize) as usize
    } else {
        next.clamp(0, len as isize - 1) as usize
    }
}

/// Up/Down move one row (wrapping), L1/R1 jump a page (clamped).
fn navigate(cursor: usize, len: usize, button: Button) -> usize {
    match button {
        Button::Up => step(cursor, len, -1, true),
        Button::Down => step(cursor, len, 1, true),
        Button::L1 => step(cursor, len, -PAGE, false),
        Button::R1 => step(cursor, len, PAGE, false),
        _ => cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_list_entries_round_trip() {
        for index in 0..6 {
            assert_eq!(GameList::at(index).index(), index);
        }
        assert!(GameList::at(0) == GameList::Favorites && GameList::at(1) == GameList::Recent && GameList::at(2) == GameList::System(0));
    }

    #[test]
    fn navigation_wraps_rows_and_clamps_pages() {
        assert_eq!(navigate(0, 5, Button::Up), 4);
        assert_eq!(navigate(4, 5, Button::Down), 0);
        assert_eq!(navigate(2, 5, Button::L1), 0);
        assert_eq!(navigate(2, 5, Button::R1), 4);
        assert_eq!(navigate(0, 0, Button::Down), 0);
    }
}
