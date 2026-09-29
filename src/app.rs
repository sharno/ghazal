use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ModelRc, SharedString, VecModel};

use crate::config::Config;
use crate::input::Button;
use crate::launcher::LaunchSpec;
use crate::library::Library;
use crate::system_info;
use crate::ui::{AppWindow, Hint, Row};

const TOAST_DURATION: Duration = Duration::from_secs(3);
const STATUS_REFRESH: Duration = Duration::from_secs(20);
const PAGE: isize = 8;
/// Rows handed to the UI at once, and how close the cursor may get to either end.
const ROW_WINDOW: usize = 48;
const ROW_MARGIN: usize = 16;

pub enum Effect {
    Launch(LaunchSpec),
    Quit,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Systems,
    Games(usize),
}

#[derive(Clone, Copy)]
enum MenuItem {
    Rescan,
    Quit,
}

const MENU: [(MenuItem, &str); 2] = [(MenuItem::Rescan, "Rescan library"), (MenuItem::Quit, "Quit oxmux")];

/// Footer button hints: menu open, systems list, games list.
const HINTS: [&[(&str, &str)]; 3] = [
    &[("A", "Select"), ("B", "Close")],
    &[("A", "Open"), ("≡", "Menu")],
    &[("A", "Play"), ("B", "Back"), ("◂▸", "System"), ("≡", "Menu")],
];

/// All frontend state. After every change `sync()` pushes it into the Slint window,
/// which decides how it looks (ui/app.slint).
pub struct App {
    config: Config,
    ui: AppWindow,
    library: Library,
    view: View,
    system_sel: usize,
    /// Remembered cursor per system.
    game_sel: Vec<usize>,
    menu: Option<usize>,
    toast: Option<(String, Instant, bool)>,
    status_checked: Instant,
    /// Which view the row model currently shows, so it's only rebuilt on change.
    rows_for: Option<View>,
    row_start: usize,
    hints_for: Option<usize>,
}

impl App {
    pub fn new(config: Config, ui: AppWindow) -> Self {
        let library = Library::scan(&config);
        eprintln!("oxmux: {} systems, {} games", library.systems.len(), library.game_count());
        let roots: Vec<_> = config.rom_roots.iter().map(|p| p.display().to_string()).collect();
        ui.set_empty_message(format!("No games found in {}", roots.join(", ")).into());
        ui.set_menu_items(ModelRc::new(VecModel::from(
            MENU.iter().map(|(_, label)| SharedString::from(*label)).collect::<Vec<_>>(),
        )));

        let mut app = Self {
            game_sel: vec![0; library.systems.len()],
            config,
            ui,
            library,
            view: View::Systems,
            system_sel: 0,
            menu: None,
            toast: None,
            status_checked: Instant::now(),
            rows_for: None,
            row_start: 0,
            hints_for: None,
        };
        app.refresh_status(Instant::now());
        app.sync();
        app
    }

    pub fn update(&mut self, button: Button) -> Option<Effect> {
        let effect = self.handle(button);
        self.sync();
        effect
    }

    fn handle(&mut self, button: Button) -> Option<Effect> {
        if let Some(sel) = self.menu {
            match button {
                Button::Up => self.menu = Some(step(sel, MENU.len(), -1, true)),
                Button::Down => self.menu = Some(step(sel, MENU.len(), 1, true)),
                Button::B | Button::Menu | Button::Start => self.menu = None,
                Button::A => {
                    self.menu = None;
                    return self.activate(MENU[sel].0);
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
                let len = self.library.systems.len();
                match button {
                    Button::A if len > 0 => self.view = View::Games(self.system_sel),
                    b => self.system_sel = navigate(self.system_sel, len, b),
                }
            }
            View::Games(sys) => {
                let len = self.library.systems[sys].games.len();
                match button {
                    Button::B => self.view = View::Systems,
                    Button::A => {
                        let system = &self.library.systems[sys];
                        let game = &system.games[self.game_sel[sys]];
                        match LaunchSpec::retroarch(&self.config, system, game) {
                            Ok(spec) => return Some(Effect::Launch(spec)),
                            Err(e) => self.error(format!("{e:#}")),
                        }
                    }
                    // Left/Right on the game list jumps between systems.
                    Button::Left | Button::Right => {
                        let dir = if button == Button::Left { -1 } else { 1 };
                        self.system_sel = step(sys, self.library.systems.len(), dir, true);
                        self.view = View::Games(self.system_sel);
                    }
                    b => self.game_sel[sys] = navigate(self.game_sel[sys], len, b),
                }
            }
        }
        None
    }

    pub fn launch_finished(&mut self, result: anyhow::Result<()>) {
        if let Err(e) = result {
            eprintln!("oxmux: launch failed: {e:#}");
            self.error(format!("{e:#}"));
        }
        self.refresh_status(Instant::now());
        self.sync();
    }

    /// Periodic housekeeping: toast expiry, clock and battery.
    pub fn tick(&mut self, now: Instant) {
        if self.toast.as_ref().is_some_and(|(_, at, _)| now >= *at + TOAST_DURATION) {
            self.toast = None;
        }
        if now >= self.status_checked + STATUS_REFRESH {
            self.refresh_status(now);
        }
        self.sync();
    }

    pub fn next_deadline(&self) -> Instant {
        let status = self.status_checked + STATUS_REFRESH;
        match &self.toast {
            Some((_, at, _)) => status.min(*at + TOAST_DURATION),
            None => status,
        }
    }

    fn refresh_status(&mut self, now: Instant) {
        self.status_checked = now;
        let (h, m) = system_info::local_time();
        self.ui.set_clock(format!("{h:02}:{m:02}").into());
        match system_info::battery() {
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
                self.library = Library::scan(&self.config);
                self.game_sel = vec![0; self.library.systems.len()];
                self.system_sel = self.system_sel.min(self.library.systems.len().saturating_sub(1));
                self.view = View::Systems;
                self.rows_for = None;
                self.toast = Some((format!("Found {} games", self.library.game_count()), Instant::now(), false));
                None
            }
            MenuItem::Quit => Some(Effect::Quit),
        }
    }

    fn error(&mut self, msg: String) {
        self.toast = Some((msg, Instant::now(), true));
    }

    /// Pushes state into the UI. Slint only repaints what actually changed.
    fn sync(&mut self) {
        let ui = &self.ui;
        let (title, selected, total) = match self.view {
            View::Systems => ("Systems", self.system_sel, self.library.systems.len()),
            View::Games(sys) => {
                let system = &self.library.systems[sys];
                (system.name.as_str(), self.game_sel[sys], system.games.len())
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
                View::Systems => self.library.systems[range]
                    .iter()
                    .map(|s| Row { label: s.name.as_str().into(), detail: s.games.len().to_string().into() })
                    .collect(),
                View::Games(sys) => self.library.systems[sys].games[range]
                    .iter()
                    .map(|g| Row { label: g.title.as_str().into(), detail: SharedString::new() })
                    .collect(),
            };
            ui.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
            ui.set_row_offset(start as i32);
            ui.set_row_count(total as i32);
            self.row_start = start;
            self.rows_for = Some(self.view);
        }

        ui.set_heading(title.into());
        ui.set_selected(selected as i32);
        ui.set_menu_selected(self.menu.map_or(-1, |m| m as i32));

        let (toast, is_error) = self.toast.as_ref().map_or(("", false), |(t, _, e)| (t.as_str(), *e));
        ui.set_toast(toast.into());
        ui.set_toast_error(is_error);

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
