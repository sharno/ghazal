use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ModelRc, SharedString, VecModel};

use super::config::FrontendConfig;
use super::input::Button;
use super::launcher::LaunchSpec;
use super::library::{Game, Library, System};
use crate::daemon::ctl;
use crate::hw::battery;
use crate::ui::{AppWindow, Hint, Row};
use super::activity::{Activity, Catalog, Collection, GameId};

const TOAST_DURATION: Duration = Duration::from_secs(3);
const STATUS_REFRESH: Duration = Duration::from_secs(20);
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

#[derive(Clone, Copy, PartialEq)]
enum GameList {
    System(usize),
    Favorites,
    Recent,
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

/// Footer button hints: menu open, systems list, games list.
const HINTS: [&[(&str, &str)]; 3] = [
    &[("A", "Select"), ("B", "Close")],
    &[("A", "Open"), ("≡", "Menu")],
    &[("A", "Play"), ("B", "Back"), ("Y", "Favorite"), ("≡", "Menu")],
];

/// All frontend state. After every change `sync()` pushes it into the Slint window,
/// which decides how it looks (ui/app.slint).
pub struct App {
    config: FrontendConfig,
    ui: AppWindow,
    menu_items: Vec<MenuItem>,
    catalog: Catalog,
    scan: Option<std::sync::mpsc::Receiver<Library>>,
    collection_sel: [usize; 2],
    view: View,
    system_sel: usize,
    /// Remembered cursor per system.
    game_sel: Vec<usize>,
    menu: Option<usize>,
    toast: Option<(String, Instant, bool)>,
    levels: Option<crate::levels::Levels>,
    levels_visible_until: Option<Instant>,
    status_checked: Instant,
    /// Which view the row model currently shows, so it's only rebuilt on change.
    rows_for: Option<View>,
    row_start: usize,
    hints_for: Option<usize>,
}

impl App {
    pub fn new(config: FrontendConfig, ui: AppWindow) -> anyhow::Result<Self> {
        let activity = Activity::load(&config.activity_file)?;
        let library = Library::scan(&config);
        eprintln!("oxmux: {} systems, {} games", library.systems().len(), library.game_count());
        let roots: Vec<_> = config.rom_roots.iter().map(|p| p.display().to_string()).collect();
        ui.set_empty_message(format!("No games found in {}", roots.join(", ")).into());
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
            game_sel: vec![0; library.systems().len()],
            config,
            ui,
            menu_items,
            catalog: Catalog::new(library, activity),
            scan: None,
            collection_sel: [0; 2],
            view: View::Systems,
            system_sel: 0,
            menu: None,
            toast: None,
            levels: None,
            levels_visible_until: None,
            status_checked: Instant::now(),
            rows_for: None,
            row_start: 0,
            hints_for: None,
        };
        app.refresh_status(Instant::now());
        app.refresh_levels();
        app.sync();
        Ok(app)
    }

    pub fn update(&mut self, button: Button) -> Option<Effect> {
        if self.config.daemon_socket.exists() {
            if let Err(error) = ctl::request(&self.config.daemon_socket, "activity") {
                eprintln!("oxmux: reporting input activity: {error:#}");
            }
        }
        let effect = self.handle(button);
        self.sync();
        effect
    }

    fn handle(&mut self, button: Button) -> Option<Effect> {
        if let Some(sel) = self.menu {
            match button {
                Button::Up => self.menu = Some(step(sel, self.menu_items.len(), -1, true)),
                Button::Down => self.menu = Some(step(sel, self.menu_items.len(), 1, true)),
                Button::B | Button::Menu | Button::Start => self.menu = None,
                Button::A => {
                    self.menu = None;
                    return self.activate(self.menu_items[sel]);
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
            View::Systems => {
                let len = self.catalog.library().systems().len() + 2;
                match button {
                    Button::A => self.view = View::Games(match self.system_sel {
                        0 => GameList::Favorites,
                        1 => GameList::Recent,
                        index => GameList::System(index - 2),
                    }),
                    b => self.system_sel = navigate(self.system_sel, len, b),
                }
            }
            View::Games(list) => {
                let len = self.game_count(list);
                let selected = self.selection(list).min(len.saturating_sub(1));
                match button {
                    Button::B => self.view = View::Systems,
                    Button::A if len > 0 => {
                        let (system, game) = self.game_at(list, selected)?;
                        let id = GameId::new(system, game);
                        let (system, game) = id.resolve(self.catalog.library())?;
                        match LaunchSpec::retroarch(&self.config, system, game) {
                            Ok(spec) => return Some(Effect::Launch { spec, game: id }),
                            Err(e) => self.error(format!("{e:#}")),
                        }
                    }
                    // Left/Right on the game list jumps between systems.
                    Button::Y if len > 0 => {
                        let (system, game) = self.game_at(list, selected)?;
                        let id = GameId::new(system, game);
                        match self.catalog.toggle_favorite(id) {
                            Ok(added) => self.toast = Some((if added { "Added to Favorites" } else { "Removed from Favorites" }.into(), Instant::now(), false)),
                            Err(error) => self.error(format!("Saving favorite: {error:#}")),
                        }
                        self.rows_for = None;
                    }
                    Button::Left | Button::Right if matches!(list, GameList::System(_)) => {
                        let GameList::System(sys) = list else { unreachable!() };
                        let dir = if button == Button::Left { -1 } else { 1 };
                        let next = step(sys, self.catalog.library().systems().len(), dir, true);
                        self.system_sel = next + 2;
                        self.view = View::Games(GameList::System(next));
                    }
                    b => *self.selection_mut(list) = navigate(selected, len, b),
                }
            }
        }
        None
    }

    pub fn launch_started(&mut self, game: GameId) {
        // Persist before waiting for the emulator: history survives a reset
        // during gameplay, and failed spawns never enter this callback.
        if let Err(error) = self.catalog.launched(game) {
            eprintln!("oxmux: saving history: {error:#}");
            self.error(format!("Saving history: {error:#}"));
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

    /// Periodic housekeeping: toast expiry, clock and battery.
    pub fn tick(&mut self, now: Instant) {
        if self.levels_visible_until.is_some_and(|until| now >= until) {
            self.levels_visible_until = None;
            self.ui.set_levels_visible(false);
        }
        if let Some(receiver) = &self.scan {
            match receiver.try_recv() {
                Ok(library) => {
                    self.scan = None;
                    self.game_sel = vec![0; library.systems().len()];
                    self.system_sel = self.system_sel.min(library.systems().len() + 1);
                    self.collection_sel = [0; 2];
                    self.view = View::Systems;
                    self.rows_for = None;
                    self.toast = Some((format!("Found {} games", library.game_count()), now, false));
                    self.catalog.replace_library(library);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {},
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.scan = None;
                    self.error("Library scan worker failed".into());
                }
            }
        }
        if self.toast.as_ref().is_some_and(|(_, at, _)| now >= *at + TOAST_DURATION) {
            self.toast = None;
        }
        if now >= self.status_checked + STATUS_REFRESH {
            self.refresh_status(now);
        }
        self.sync();
    }

    pub fn next_deadline(&self) -> Instant {
        let mut status = if self.scan.is_some() { Instant::now() + Duration::from_millis(50) } else { self.status_checked + STATUS_REFRESH };
        if let Some(until) = self.levels_visible_until { status = status.min(until); }
        match &self.toast {
            Some((_, at, _)) => status.min(*at + TOAST_DURATION),
            None => status,
        }
    }

    pub fn refresh_levels(&mut self) {
        match crate::levels::read(&self.config.daemon_socket) {
            Ok(Some(levels)) => {
                if self.levels.is_some_and(|previous| previous != levels) {
                    self.levels_visible_until = Some(Instant::now() + TOAST_DURATION);
                    self.ui.set_levels_visible(true);
                }
                self.levels = Some(levels);
                self.ui.set_brightness_level(u8::from(levels.brightness).into());
                self.ui.set_volume_level(u8::from(levels.volume).into());
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

    fn activate(&mut self, item: MenuItem) -> Option<Effect> {
        match item {
            MenuItem::Rescan => {
                if self.scan.is_none() {
                    let config = self.config.clone();
                    let (sender, receiver) = std::sync::mpsc::channel();
                    match std::thread::Builder::new().name("oxmux-library".into()).spawn(move || {
                        let _ = sender.send(Library::scan(&config));
                    }) {
                        Ok(_) => {
                            self.scan = Some(receiver);
                            self.toast = Some(("Scanning library…".into(), Instant::now(), false));
                        }
                        Err(error) => self.error(format!("Starting library scan: {error}")),
                    }
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

    fn error(&mut self, msg: String) {
        self.toast = Some((msg, Instant::now(), true));
    }

    fn game_count(&self, list: GameList) -> usize {
        match list {
            GameList::System(index) => self.catalog.library().systems()[index].games.len(),
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

    fn selection(&self, list: GameList) -> usize {
        match list {
            GameList::System(index) => self.game_sel[index],
            GameList::Favorites => self.collection_sel[0],
            GameList::Recent => self.collection_sel[1],
        }
    }

    fn selection_mut(&mut self, list: GameList) -> &mut usize {
        match list {
            GameList::System(index) => &mut self.game_sel[index],
            GameList::Favorites => &mut self.collection_sel[0],
            GameList::Recent => &mut self.collection_sel[1],
        }
    }

    /// Pushes state into the UI. Slint only repaints what actually changed.
    fn sync(&mut self) {
        let ui = &self.ui;
        let (title, selected, total) = match self.view {
            View::Systems => ("Systems", self.system_sel, self.catalog.library().systems().len() + 2),
            View::Games(list) => {
                let title = match list {
                    GameList::System(index) => self.catalog.library().systems()[index].name.as_str(),
                    GameList::Favorites => "Favorites",
                    GameList::Recent => "Recent Games",
                };
                let len = self.game_count(list);
                (title, self.selection(list).min(len.saturating_sub(1)), len)
            }
        };

        // The UI only gets a window of ROW_WINDOW rows around the cursor. It's moved
        // once the cursor gets within ROW_MARGIN of either edge, so the rows that are
        // on screen (or scrolling out) are always inside it.
        let max_start = total.saturating_sub(ROW_WINDOW);
        let s = self.row_start;
        let window_ok = self.rows_for == Some(self.view)
            && (s == 0 || selected >= s + ROW_MARGIN)
            && (s == max_start || selected < s + ROW_WINDOW - ROW_MARGIN);
        if !window_ok {
            let start = selected.saturating_sub(ROW_WINDOW / 2).min(max_start);
            let range = start..(start + ROW_WINDOW).min(total);
            let rows: Vec<Row> = match self.view {
                View::Systems => range.map(|index| match index {
                    0 => Row { label: "Favorites".into(), detail: self.game_count(GameList::Favorites).to_string().into() },
                    1 => Row { label: "Recent Games".into(), detail: self.game_count(GameList::Recent).to_string().into() },
                    index => {
                        let system = &self.catalog.library().systems()[index - 2];
                        Row { label: system.name.as_str().into(), detail: system.games.len().to_string().into() }
                    }
                }).collect(),
                View::Games(list) => range
                    .filter_map(|index| self.game_at(list, index).map(|(system, game)| Row {
                        label: game.title.as_str().into(),
                        detail: format!("{}{}", if self.catalog.is_favorite(&GameId::new(system, game)) { "★ " } else { "" }, if matches!(list, GameList::System(_)) { "" } else { system.name.as_str() }).into(),
                    }))
                    .collect(),
            };
            ui.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
            ui.set_row_offset(start as i32);
            ui.set_row_count(total as i32);
            self.row_start = start;
            self.rows_for = Some(self.view);
        }

        ui.set_heading(title.into());
        ui.set_empty_message(match self.view {
            View::Games(GameList::Favorites) => "Press Y on a game to add a favorite".to_owned(),
            View::Games(GameList::Recent) => "Games you launch will appear here".to_owned(),
            _ => format!("No games found in {}", self.config.rom_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")),
        }.into());
        ui.set_selected(selected as i32);
        ui.set_menu_selected(self.menu.map_or(-1, |m| m as i32));

        let (toast, is_error) = self.toast.as_ref().map_or(("", false), |(t, _, e)| (t.as_str(), *e));
        ui.set_toast(toast.into());
        ui.set_toast_error(is_error);
        ui.set_levels_visible(self.levels.is_some() && (self.menu.is_some() || self.levels_visible_until.is_some()));

        let hint_set = match (self.menu.is_some(), self.view) {
            (true, _) => 0,
            (false, View::Systems) => 1,
            (false, View::Games(_)) => 2,
        };
        if self.hints_for != Some(hint_set) {
            let hints: Vec<Hint> = HINTS[hint_set]
                .iter()
                .map(|(g, l)| Hint { glyph: (*g).into(), label: (*l).into() })
                .collect();
            ui.set_hints(ModelRc::new(VecModel::from(hints)));
            self.hints_for = Some(hint_set);
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
fn navigate(sel: usize, len: usize, button: Button) -> usize {
    match button {
        Button::Up => step(sel, len, -1, true),
        Button::Down => step(sel, len, 1, true),
        Button::L1 => step(sel, len, -PAGE, false),
        Button::R1 => step(sel, len, PAGE, false),
        _ => sel,
    }
}
