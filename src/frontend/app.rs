use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::activity::{Activity, Catalog, Collection, GameId, LoadError};
use super::bluetooth;
use super::config::{FrontendConfig, Launch};
use super::emulator_choice::Choices;
use super::emulator_picker::{self, Picker, Step};
use super::input::Button;
use super::keyboard;
use super::launcher::LaunchSpec;
use super::library::{Game, Library, System};
use super::moonlight::{self, Notice};
use super::netplay::{self, Choosing, Pick, Polled, Role, Scanning, Session, Target};
use super::settings::{Settings, StartupMode};
use super::syncthing;
use super::thumbnails::{self, Art, Loader, Progress};
use super::timezone;
use super::indicators;
use super::wifi;
use crate::daemon::ctl;
use crate::hdmi;
use crate::hw::battery;
use crate::ports;
use crate::retroarch_cfg::{Achievements, Mode};
use crate::timezone::{Rules, Store, Zone};
use crate::ui::{AppWindow, Hint, KeyCap, KeyboardState, Row};

const TOAST_DURATION: Duration = Duration::from_secs(3);
const STATUS_REFRESH: Duration = Duration::from_secs(20);
/// Status icons read a few small files, so they can follow changes closely.
const INDICATOR_REFRESH: Duration = Duration::from_secs(5);
/// The daemon only needs to know the user is present, not every key repeat.
const ACTIVITY_REPORT: Duration = Duration::from_secs(1);
const SCAN_POLL: Duration = Duration::from_millis(50);
pub(crate) const PAGE: isize = 8;
/// Rows handed to the UI at once, and how close the cursor may get to either end.
const ROW_WINDOW: usize = 48;
const ROW_MARGIN: usize = 16;

pub enum Effect {
    Launch { spec: LaunchSpec, purpose: LaunchPurpose },
    Quit,
}

pub enum LaunchPurpose { Game(GameId), RetroArchSettings, Stream, PortMaster }

#[derive(Clone, Copy, PartialEq)]
enum View {
    Home,
    Systems,
    Games(GameList),
    Applications,
    Moonlight,
    Configuration,
    Information,
    Updates,
    Wifi,
    Bluetooth,
    Syncthing,
    TimeZone,
    /// Hosts to join; leaving returns to that game list, or to Applications.
    Netplay(Option<GameList>),
}

/// Looking for netplay hosts, then choosing one.
enum Join {
    Scanning(Scanning),
    Choosing(Choosing),
}

/// The game a game-actions menu was opened on, checked again when an action is chosen.
struct GameMenu {
    list: GameList,
    cursor: usize,
    id: GameId,
}

#[derive(Clone, Copy, PartialEq)]
enum HomeItem { Explore, Collection, History, Applications, Information, Configuration, Reboot, Shutdown }
impl HomeItem {
    fn entries(power_available: bool) -> Vec<Self> {
        let mut items = vec![Self::Explore, Self::Collection, Self::History, Self::Applications, Self::Information, Self::Configuration];
        if power_available { items.extend([Self::Reboot, Self::Shutdown]); }
        items
    }
    fn label(self) -> &'static str {
        match self {
            Self::Explore => "Explore Content", Self::Collection => "Collection", Self::History => "History",
            Self::Applications => "Applications", Self::Information => "Information", Self::Configuration => "Configuration",
            Self::Reboot => "Reboot", Self::Shutdown => "Shutdown",
        }
    }
    fn glyph(self) -> &'static str {
        match self {
            Self::Explore => "▤", Self::Collection => "★", Self::History => "↶", Self::Applications => "▦",
            Self::Information => "i", Self::Configuration => "⚙", Self::Reboot => "↻", Self::Shutdown => "○",
        }
    }
}

/// A list of games. Also each entry of the systems list.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GameList {
    Favorites,
    Recent,
    System(usize),
}

enum StartupState {
    WaitingForLibrary(StartupMode),
    Complete,
}

impl GameList {
    fn parent(self) -> View { match self { Self::System(_) => View::Systems, _ => View::Home } }
}

#[derive(Clone, Copy, PartialEq)]
enum MenuItem {
    Rescan,
    Options,
    Sleep,
    Restart,
    PowerOff,
    Quit,
    Thumbnails,
    DownloadThumbnails,
    Back,
    Brightness,
    Volume,
    Startup,
    RetroArchSettings,
    SystemUpdate,
    PortMaster,
    InstallPortMaster,
    Wifi,
    Bluetooth,
    Syncthing,
    TimeZone,
    /// Shown with the daemon's current preset.
    Lights(crate::hw::lights::Preset),
    Moonlight,
    /// RetroAchievements off, on or hardcore; its row shows the account.
    Achievements,
    /// RetroArch's Achievements page, where the account is entered.
    AchievementsAccount,
    JoinNetplay,
    /// The game actions menu (Applications' JoinNetplay finds the game itself).
    PlayGame,
    /// Opens the emulator picker for the game; only systems with alternatives list it.
    ChooseEmulator,
    HostNetplay,
    JoinGameNetplay,
    /// Shown only on boards with an HDMI connector.
    Hdmi(hdmi::Policy),
}

impl MenuItem {
    fn label(self, settings: &Settings) -> &'static str {
        match self {
            Self::Rescan => "Rescan library",
            Self::Options => "Options",
            Self::Sleep => "Sleep",
            Self::Restart => "Restart",
            Self::PowerOff => "Power off",
            Self::Quit => "Quit ghazal",
            Self::Thumbnails if settings.show_thumbnails => "Thumbnails: On",
            Self::Thumbnails => "Thumbnails: Off",
            Self::DownloadThumbnails => "Download thumbnails",
            Self::Back => "Back",
            Self::Brightness => "Brightness",
            Self::Volume => "Volume",
            Self::Startup => match settings.startup {
                StartupMode::Menu => "Startup: Main Menu",
                StartupMode::LastGame => "Startup: Last Game",
            },
            Self::RetroArchSettings => "RetroArch settings",
            Self::SystemUpdate => "System update",
            Self::PortMaster => "PortMaster",
            Self::InstallPortMaster => "Install PortMaster",
            Self::Wifi => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::Syncthing => "Syncthing",
            Self::TimeZone => "Time zone",
            Self::Lights(preset) => preset.menu_label(),
            Self::Moonlight => "Moonlight game streaming",
            Self::Achievements => "Achievements",
            Self::AchievementsAccount => "Achievements account",
            Self::JoinNetplay | Self::JoinGameNetplay => "Join netplay",
            Self::PlayGame => "Play",
            Self::ChooseEmulator => "Emulator",
            Self::HostNetplay => "Host netplay",
            Self::Hdmi(policy) => policy.label(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MenuPage {
    Main,
    Options,
    /// Actions for the game chosen with X (see `App::game_menu`).
    Game,
}

#[derive(Clone, Copy)]
struct MenuState {
    page: MenuPage,
    cursor: usize,
}

const OPTION_ITEMS: [MenuItem; 4] = [MenuItem::Startup, MenuItem::Thumbnails, MenuItem::DownloadThumbnails, MenuItem::Back];
const APPLICATION_ITEMS: [MenuItem; 4] = [MenuItem::Moonlight, MenuItem::Rescan, MenuItem::DownloadThumbnails, MenuItem::JoinNetplay];
/// Poll interval while art is loading or downloading.
const BACKGROUND_POLL: Duration = Duration::from_millis(30);
/// The Bluetooth worker reports every second; this is how soon the screen notices.
const BLUETOOTH_POLL: Duration = Duration::from_millis(150);

type Hints = &'static [(&'static str, &'static str)];
const MENU_HINTS: Hints = &[("A", "Select"), ("B", "Close")];
const OPTIONS_HINTS: Hints = &[("A", "Select"), ("B", "Back")];
const HOME_HINTS: Hints = &[("A", "Open"), ("B", "Explore"), ("≡", "Menu")];
const SYSTEM_HINTS: Hints = &[("A", "Open"), ("B", "Back"), ("≡", "Menu")];
const CONFIGURATION_HINTS: Hints = &[("A", "Change"), ("◀▶", "Adjust"), ("B", "Back")];
const INFORMATION_HINTS: Hints = &[("B", "Back")];
const GAME_HINTS: Hints = &[("A", "Play"), ("B", "Back"), ("Y", "Favorite"), ("X", "Actions"), ("◀▶", "System"), ("≡", "Menu")];
const COLLECTION_HINTS: Hints = &[("A", "Play"), ("B", "Back"), ("Y", "Favorite"), ("X", "Actions"), ("≡", "Menu")];
const NETPLAY_HINTS: Hints = &[("A", "Join"), ("Y", "Refresh"), ("B", "Back")];
const COMPACT_GAME_HINTS: Hints = &[("A", "Play"), ("B", "Back"), ("Y", "Favorite"), ("X", "Actions")];

/// What the UI's overlay list shows (`Overlay` in ui/app.slint's `menu-*` properties).
struct Overlay {
    title: String,
    labels: Vec<String>,
    cursor: usize,
    /// Rows may be long (emulator names), so the panel is wider.
    wide: bool,
}

struct Toast {
    text: String,
    shown: Instant,
    error: bool,
}

struct Scan {
    results: std::sync::mpsc::Receiver<Library>,
    announce: bool,
}

/// The Applications entry for PortMaster: launch it, or install it first.
struct PortsMenu {
    dir: std::path::PathBuf,
    state: ports::PortMaster,
    install: Option<std::sync::mpsc::Receiver<ports::Progress>>,
    /// What the install is doing, shown again whenever the toast expires.
    step: &'static str,
    /// PortMaster installs and removes ports: list them again when it exits.
    rescan_on_return: bool,
}

impl PortsMenu {
    fn new(config: &FrontendConfig) -> Option<Self> {
        let dir = config.ports.as_ref()?.ports_dir(&config.rom_roots)?;
        Some(Self { state: ports::portmaster_state(&dir), dir, install: None, step: "", rescan_on_return: false })
    }
}

/// All frontend state. After every change `sync()` pushes it into the Slint window,
/// which decides how it looks (ui/app.slint).
pub struct App {
    config: FrontendConfig,
    ui: AppWindow,
    /// Main menu entries; the power ones depend on the daemon.
    main_items: Vec<MenuItem>,
    home_items: Vec<HomeItem>,
    home_cursor: usize,
    application_cursor: usize,
    /// Only while the Moonlight screens are open.
    moonlight: Option<moonlight::Moonlight>,
    configuration_cursor: usize,
    updater: crate::update::Menu,
    /// Open only while Configuration → Bluetooth is on screen: it owns a worker thread.
    bluetooth: Option<bluetooth::Screen>,
    /// Open only while Configuration → Time zone is on screen.
    time_zone: Option<timezone::Screen>,
    /// The chosen zone's rules, for the status bar's clock.
    clock: Rules,
    information_cursor: usize,
    information_rows: Vec<Row>,
    /// RetroAchievements as RetroArch's config has it; read when Configuration opens.
    achievements: Achievements,
    join: Option<Join>,
    game_menu: Option<GameMenu>,
    settings: Settings,
    startup: StartupState,
    catalog: Catalog,
    /// Which emulator runs each game (see `emulator_choice`).
    emulators: Choices,
    /// The emulator picker, when open over a game list.
    picker: Option<Picker>,
    scan: Option<Scan>,
    view: View,
    /// Cursor in the systems list.
    system_cursor: usize,
    /// Remembered cursor per game list.
    game_cursors: BTreeMap<GameList, usize>,
    menu: Option<MenuState>,
    /// Labels currently in the UI's menu model.
    menu_labels: Vec<String>,
    art: Option<Loader>,
    /// The game whose art is shown or loading, and the pending request.
    art_for: Option<(GameList, usize)>,
    art_pending: Option<u64>,
    download: Option<std::sync::mpsc::Receiver<Progress>>,
    ports: Option<PortsMenu>,
    toast: Option<Toast>,
    levels: Option<crate::levels::Levels>,
    /// None on boards without stick lights.
    lights: Option<crate::hw::lights::Preset>,
    levels_visible_until: Option<Instant>,
    /// None on boards without HDMI, which hides its entry.
    hdmi: Option<hdmi::Status>,
    status_checked: Instant,
    indicators_checked: Instant,
    activity_reported: Option<Instant>,
    /// Which view the row model currently shows, so it's only rebuilt on change.
    rows_for: Option<View>,
    row_start: usize,
    hints_for: Option<Hints>,
    /// Open while View::Wifi is shown; its worker thread lives and dies with it.
    wifi: Option<wifi::Controller>,
    /// What the Wi-Fi screen last drew, refreshed whenever it reacts.
    wifi_view: Option<wifi::View>,
    /// Open while View::Syncthing is shown; its worker thread lives and dies with it.
    syncthing: Option<syncthing::Controller>,
    /// What the Syncthing screen last drew, refreshed whenever it reacts.
    syncthing_view: Option<syncthing::View>,
    /// The QR code of this handheld's Syncthing ID, made once per ID.
    syncthing_code: Option<(crate::syncthing::DeviceId, slint::Image)>,
    /// Syncthing's status icon, asked for in the background.
    syncthing_status: syncthing::Status,
    /// Keys currently in the keyboard's UI model, so it is only rebuilt on change.
    keyboard_keys: Vec<keyboard::KeyView>,
}

impl App {
    pub fn new(config: FrontendConfig, ui: AppWindow) -> anyhow::Result<Self> {
        // Damaged favorites/history must not keep the menu from starting.
        let (activity, activity_warning) = match Activity::load(&config.activity_file) {
            Ok(activity) => (activity, None),
            // The supervisor restarts the menu; the next read usually succeeds.
            Err(LoadError::Unreadable(error)) => return Err(error.context("reading favorites and history")),
            Err(LoadError::Damaged(error)) => {
                let aside = Activity::set_aside(&config.activity_file)?;
                eprintln!("ghazal: {error:#}; moved it to {}", aside.display());
                (Activity::load(&config.activity_file).map_err(LoadError::into_error)?, Some(format!("Favorites and history were unreadable; kept them as {}", aside.display())))
            }
        };
        // Power entries need the daemon; Quit only makes sense when launched from muOS.
        let daemon_status = if config.daemon_socket.exists() {
            ctl::request(&config.daemon_socket, "status").inspect_err(|error| eprintln!("ghazal: daemon unavailable: {error:#}")).ok()
        } else { None };
        let mut main_items = vec![MenuItem::Rescan, MenuItem::Options];
        if let Some(reply) = &daemon_status {
            if reply.split_whitespace().any(|field| field == "suspend=1") {
                main_items.push(MenuItem::Sleep);
            }
            main_items.extend([MenuItem::Restart, MenuItem::PowerOff]);
        }
        if std::env::var_os("GHAZAL_EMBEDDED").is_none() {
            main_items.push(MenuItem::Quit);
        }
        let settings = Settings::load(&config.settings_file);
        // Unusable choices must not keep the menu from starting: games run with their systems' defaults.
        let (emulators, emulators_warning) = Choices::open(&config.emulators_file());
        let startup = StartupState::WaitingForLibrary(settings.startup);
        let home_items = HomeItem::entries(daemon_status.is_some());
        let art = Loader::new().inspect_err(|error| eprintln!("ghazal: art loader: {error:#}")).ok();
        let ports = PortsMenu::new(&config);

        let mut app = Self {
            config,
            ui,
            main_items,
            home_items,
            home_cursor: 0,
            application_cursor: 0,
            moonlight: None,
            configuration_cursor: 0,
            updater: crate::update::Menu::default(),
            bluetooth: None,
            time_zone: None,
            clock: Store::system().rules().unwrap_or_else(|error| {
                eprintln!("ghazal: time zone: {error:#}; showing UTC");
                Rules::utc()
            }),
            information_cursor: 0,
            information_rows: Vec::new(),
            achievements: Achievements::default(),
            join: None,
            game_menu: None,
            settings,
            startup,
            // Draw the menu first; a cold SD-card scan of thousands of games takes ~0.5 s.
            catalog: Catalog::new(Library::from_systems(Vec::new()), activity),
            emulators,
            picker: None,
            scan: None,
            view: View::Home,
            system_cursor: 0,
            game_cursors: BTreeMap::new(),
            menu: None,
            menu_labels: Vec::new(),
            art,
            art_for: None,
            art_pending: None,
            download: None,
            ports,
            toast: None,
            levels: None,
            lights: daemon_status.as_deref().and_then(ctl::lights),
            levels_visible_until: None,
            hdmi: None,
            status_checked: Instant::now(),
            indicators_checked: Instant::now(),
            activity_reported: None,
            rows_for: None,
            row_start: 0,
            hints_for: None,
            wifi: None,
            wifi_view: None,
            syncthing: None,
            syncthing_view: None,
            syncthing_code: None,
            syncthing_status: syncthing::Status::new(Instant::now()),
            keyboard_keys: Vec::new(),
        };
        app.start_scan(false);
        if let Ok(warning) = std::fs::read_to_string("/run/ghazal/provision-warning") { app.error(warning); }
        if let Some(warning) = activity_warning { app.error(warning); }
        if let Some(warning) = emulators_warning { app.error(warning); }
        app.refresh_status(Instant::now());
        app.refresh_levels();
        app.sync();
        Ok(app)
    }

    pub fn update(&mut self, button: Button) -> Option<Effect> {
        // An intentional menu interaction cancels the pending automatic launch.
        self.startup = StartupState::Complete;
        self.report_activity();
        let effect = self.handle(button);
        self.sync();
        effect
    }

    pub fn disable_startup(&mut self) { self.startup = StartupState::Complete; }

    /// Consume the startup decision once, after the current card has been scanned.
    /// Both platforms execute this through their normal display/input handoff.
    pub fn take_startup_effect(&mut self) -> Option<Effect> {
        if self.scan.is_some() { return None; }
        let StartupState::WaitingForLibrary(mode) = std::mem::replace(&mut self.startup, StartupState::Complete) else { return None; };
        if mode == StartupMode::Menu { return None; }
        let effect = self.startup_launch();
        self.sync();
        effect
    }

    fn startup_launch(&mut self) -> Option<Effect> {
        let Some(id) = self.catalog.last_played().cloned() else {
            self.notice("No last game yet; choose a game from Explore Content");
            return None;
        };
        let Some((system, game)) = id.resolve(self.catalog.library()) else {
            self.error("Last game is unavailable on this card; staying in Main Menu".into());
            return None;
        };
        match LaunchSpec::new(&self.config, self.emulators.resolve(system, game), game) {
            Ok(spec) => Some(Effect::Launch { spec, purpose: LaunchPurpose::Game(id) }),
            Err(error) => { self.error(format!("Starting last game: {error:#}")); None }
        }
    }

    /// The frontend may grab the gamepad, hiding input from the daemon's idle timer.
    fn report_activity(&mut self) {
        let now = Instant::now();
        if self.activity_reported.is_some_and(|at| now < at + ACTIVITY_REPORT) || !self.config.daemon_socket.exists() {
            return;
        }
        self.activity_reported = Some(now);
        if let Err(error) = ctl::request(&self.config.daemon_socket, "activity") {
            eprintln!("ghazal: reporting input activity: {error:#}");
        }
    }

    fn handle(&mut self, button: Button) -> Option<Effect> {
        if self.view == View::Updates && self.updater.working() { return None; }
        if crate::update::busy() { return None; }
        if let Some(picker) = self.picker.take() {
            self.press_in_picker(picker, button);
            return None;
        }
        if let Some(MenuState { page, cursor }) = self.menu {
            let items = self.page_items(page);
            match button {
                Button::Up => self.menu = Some(MenuState { page, cursor: step(cursor, items.len(), -1, true) }),
                Button::Down => self.menu = Some(MenuState { page, cursor: step(cursor, items.len(), 1, true) }),
                Button::B if page == MenuPage::Options => self.open_menu(MenuPage::Main),
                Button::B | Button::Menu | Button::Start => self.menu = None,
                Button::A => return self.activate(items[cursor]),
                _ => {}
            }
            return None;
        }
        // The on-screen keyboard uses Start to confirm and Menu to cancel.
        if matches!(button, Button::Menu | Button::Start) && !self.wifi.as_ref().is_some_and(wifi::Controller::typing) {
            self.open_menu(MenuPage::Main);
            return None;
        }
        match self.view {
            View::Home => match button {
                Button::A => match self.home_items[self.home_cursor] {
                    HomeItem::Explore => self.view = View::Systems,
                    HomeItem::Collection => self.view = View::Games(GameList::Favorites),
                    HomeItem::History => self.view = View::Games(GameList::Recent),
                    HomeItem::Applications => { self.refresh_ports(); self.view = View::Applications; }
                    HomeItem::Configuration => { self.refresh_achievements(); self.view = View::Configuration; }
                    HomeItem::Information => { self.refresh_information(); self.view = View::Information; }
                    HomeItem::Reboot => return self.power("reboot"),
                    HomeItem::Shutdown => return self.power("poweroff"),
                },
                Button::B => self.home_cursor = 0,
                button => self.home_cursor = navigate(self.home_cursor, self.home_items.len(), button),
            },
            View::Systems => match button {
                Button::A if self.system_entries() > 0 => self.view = View::Games(GameList::System(self.system_cursor)),
                Button::B => self.view = View::Home,
                button => self.system_cursor = navigate(self.system_cursor, self.system_entries(), button),
            },
            View::Applications => match button {
                Button::B => self.view = View::Home,
                Button::A => return self.application_items().get(self.application_cursor).copied().and_then(|item| self.activate(item)),
                button => self.application_cursor = navigate(self.application_cursor, self.application_items().len(), button),
            },
            View::Moonlight => {
                let step = self.moonlight.as_mut().map_or(moonlight::Step::Leave, |session| session.handle(button));
                self.relay_moonlight_notice();
                self.rows_for = None;
                match step {
                    moonlight::Step::Stay => {}
                    moonlight::Step::Leave => { self.moonlight = None; self.view = View::Applications; }
                    moonlight::Step::Launch(spec) => return Some(Effect::Launch { spec, purpose: LaunchPurpose::Stream }),
                }
            }
            View::Configuration => match button {
                Button::B => self.view = View::Home,
                Button::A | Button::Left | Button::Right => {
                    let item = self.configuration_items()[self.configuration_cursor];
                    match item {
                        MenuItem::Brightness | MenuItem::Volume => self.adjust_level(item, button != Button::Left),
                        MenuItem::Lights(_) => { self.step_lights(button != Button::Left); self.rows_for = None; }
                        MenuItem::Hdmi(_) => self.step_hdmi(button != Button::Left),
                        MenuItem::Startup => { self.activate(item); self.rows_for = None; }
                        MenuItem::Thumbnails if button == Button::A => { self.activate(item); self.rows_for = None; }
                        MenuItem::SystemUpdate | MenuItem::Wifi | MenuItem::Bluetooth | MenuItem::Syncthing | MenuItem::TimeZone if button == Button::A => { self.activate(item); }
                        MenuItem::RetroArchSettings if button == Button::A => return self.activate(item),
                        MenuItem::Achievements if button == Button::A => { self.cycle_achievements(); self.rows_for = None; }
                        MenuItem::AchievementsAccount if button == Button::A => return self.activate(item),
                        _ => {},
                    }
                }
                button => self.configuration_cursor = navigate(self.configuration_cursor, self.configuration_items().len(), button),
            },
            View::Updates => match button {
                Button::B => self.view = View::Configuration,
                Button::A => match self.updater.activate() {
                    Ok(true) => return self.power("reboot"),
                    Ok(false) => self.rows_for = None,
                    Err(error) => self.error(format!("{error:#}")),
                },
                button => self.updater.cursor = navigate(self.updater.cursor, 4, button),
            },
            View::Wifi => self.wifi_button(button),
            View::Syncthing => self.syncthing_button(button),
            View::TimeZone => self.time_zone_button(button),
            View::Bluetooth => match self.bluetooth.as_mut().map(|screen| screen.press(button)) {
                Some(bluetooth::Press::Stay(notice)) => if let Some(notice) = notice { self.show(notice) },
                Some(bluetooth::Press::Leave) | None => { self.bluetooth = None; self.view = View::Configuration; }
            },
            View::Information => match button {
                Button::B => self.view = View::Home,
                button => self.information_cursor = navigate(self.information_cursor, self.information_rows.len(), button),
            },
            View::Netplay(back) => match button {
                Button::B => self.leave_netplay(back),
                Button::A => return self.join_chosen(back),
                // A host that started after the scan is missed until the next one.
                Button::Y => if let Some(Join::Choosing(hosts)) = &self.join {
                    let target = hosts.target().clone();
                    self.start_join(target, back);
                },
                button => if let Some(Join::Choosing(hosts)) = &mut self.join { hosts.cursor = navigate(hosts.cursor, hosts.len(), button); },
            },
            View::Games(list) => {
                let len = self.game_count(list);
                let cursor = self.cursor(list);
                match button {
                    Button::B => self.view = list.parent(),
                    Button::A => return self.launch(list, cursor),
                    Button::Y => self.toggle_favorite(list, cursor),
                    Button::X => self.open_game_menu(list, cursor),
                    // Left/Right in a system's games jump to the neighbouring system.
                    Button::Left | Button::Right => if let GameList::System(system) = list {
                        let next = step(system, self.catalog.library().systems().len(), if button == Button::Left { -1 } else { 1 }, true);
                        self.system_cursor = next;
                        self.view = View::Games(GameList::System(next));
                    },
                    button => { self.game_cursors.insert(list, navigate(cursor, len, button)); }
                }
            }
        }
        None
    }

    /// Opens Configuration → Wi-Fi. Its worker starts here, not at boot.
    fn open_wifi(&mut self) {
        match wifi::open(Instant::now()) {
            Ok(controller) => {
                self.wifi_view = Some(controller.view());
                self.wifi = Some(controller);
                self.view = View::Wifi;
                self.rows_for = None;
            }
            Err(error) => self.error(format!("Wi-Fi: {error}")),
        }
    }

    fn wifi_button(&mut self, button: Button) {
        let Some(controller) = &mut self.wifi else {
            // Not reachable by design; never leave the user on a screen with no owner.
            self.view = View::Configuration;
            return;
        };
        let reaction = controller.handle(button, Instant::now());
        self.wifi_reacted(reaction);
    }

    /// Takes in what the Wi-Fi screen did: a new picture, a message, or its end.
    fn wifi_reacted(&mut self, reaction: wifi::Reaction) {
        match reaction.notice {
            Some(wifi::Notice::Info(text)) => self.notice(&text),
            Some(wifi::Notice::Error(text)) => self.error(text),
            None => {}
        }
        if !reaction.changed { return; }
        self.rows_for = None;
        match self.wifi.as_ref() {
            Some(controller) if !controller.closed() => self.wifi_view = Some(controller.view()),
            _ => {
                self.wifi = None;
                self.wifi_view = None;
                self.view = View::Configuration;
            }
        }
    }

    fn poll_wifi(&mut self, now: Instant) {
        let Some(controller) = &mut self.wifi else { return };
        let reaction = controller.poll(now);
        self.wifi_reacted(reaction);
    }

    /// For the snapshot renderer: the Wi-Fi screen is waiting on its worker.
    #[cfg(feature = "desktop")]
    pub fn wifi_working(&self) -> bool { self.wifi.as_ref().is_some_and(wifi::Controller::working) }

    /// Opens Configuration → Syncthing. Its worker starts here, not at boot.
    fn open_syncthing(&mut self) {
        match syncthing::open(Instant::now()) {
            Ok(controller) => {
                self.syncthing_view = Some(controller.view(crate::syncthing::clock()));
                self.syncthing = Some(controller);
                self.view = View::Syncthing;
                self.rows_for = None;
            }
            Err(error) => self.error(format!("Syncthing: {error}")),
        }
    }

    /// Opens Configuration → Time zone on the current zone.
    fn open_time_zone(&mut self) {
        let store = Store::system();
        match store.zones() {
            Ok(zones) => {
                self.time_zone = Some(timezone::Screen::new(zones, store.current().unwrap_or_else(Zone::utc)));
                self.view = View::TimeZone;
                self.rows_for = None;
            }
            Err(error) => self.error(format!("Time zone: {error:#}")),
        }
    }

    fn time_zone_button(&mut self, button: Button) {
        let press = self.time_zone.as_mut().map_or(timezone::Press::Leave, |screen| screen.press(button));
        self.rows_for = None;
        match press {
            timezone::Press::Stay => {}
            timezone::Press::Leave => { self.time_zone = None; self.view = View::Configuration; }
            timezone::Press::Choose(zone) => {
                let store = Store::system();
                match store.choose(&zone).and_then(|()| store.rules()) {
                    Ok(rules) => {
                        self.clock = rules;
                        self.refresh_status(Instant::now());
                        self.notice(&format!("Time zone: {}", zone.place()));
                    }
                    Err(error) => self.error(format!("Saving time zone: {error:#}")),
                }
                self.time_zone = None;
                self.view = View::Configuration;
            }
        }
    }

    fn syncthing_button(&mut self, button: Button) {
        let Some(controller) = &mut self.syncthing else {
            // Not reachable by design; never leave the user on a screen with no owner.
            self.view = View::Configuration;
            return;
        };
        let reaction = controller.handle(button, Instant::now());
        self.syncthing_reacted(reaction, Instant::now());
    }

    /// Takes in what the Syncthing screen did: a new picture, a message, or its end.
    fn syncthing_reacted(&mut self, reaction: syncthing::Reaction, now: Instant) {
        match reaction.notice {
            Some(syncthing::Notice::Info(text)) => self.notice(&text),
            Some(syncthing::Notice::Error(text)) => self.error(text),
            None => {}
        }
        if !reaction.changed { return; }
        self.rows_for = None;
        match self.syncthing.as_ref() {
            Some(controller) if !controller.closed() => {
                self.syncthing_view = Some(controller.view(crate::syncthing::clock()));
                // The screen just looked: the status icon needs no question of its own.
                if controller.summary().is_some_and(|summary| self.syncthing_status.record(summary, now)) { self.refresh_indicators(now); }
            }
            _ => {
                self.syncthing = None;
                self.syncthing_view = None;
                self.view = View::Configuration;
            }
        }
    }

    fn poll_syncthing(&mut self, now: Instant) {
        if let Some(controller) = &mut self.syncthing {
            let reaction = controller.poll(now);
            self.syncthing_reacted(reaction, now);
        } else if self.syncthing_status.poll(now, syncthing::summary) {
            self.refresh_indicators(now);
        }
    }

    /// For the snapshot renderer: the Syncthing screen is waiting on its worker.
    #[cfg(feature = "desktop")]
    pub fn syncthing_working(&self) -> bool { self.syncthing.as_ref().is_some_and(syncthing::Controller::working) }

    /// The QR code for this handheld's ID, made when the ID first shows.
    fn syncthing_code(&mut self, id: &crate::syncthing::DeviceId) -> Option<slint::Image> {
        if self.syncthing_code.as_ref().is_none_or(|(shown, _)| shown != id) {
            self.syncthing_code = syncthing::qr_image(id).map(|image| (id.clone(), image));
        }
        self.syncthing_code.as_ref().map(|(_, image)| image.clone())
    }

    fn launch(&mut self, list: GameList, cursor: usize) -> Option<Effect> {
        let (system, game) = self.game_at(list, cursor)?;
        let id = GameId::new(system, game);
        match LaunchSpec::new(&self.config, self.emulators.resolve(system, game), game) {
            Ok(spec) => Some(Effect::Launch { spec, purpose: LaunchPurpose::Game(id) }),
            Err(error) => {
                self.error(format!("{error:#}"));
                None
            }
        }
    }

    /// Emulator in a game's actions menu: offer the emulators of its system.
    fn open_emulator_picker(&mut self) {
        match self.menu_game().map(|(system, game)| Picker::open(system, game, &self.emulators)) {
            Some(Ok(picker)) => self.picker = Some(picker),
            Some(Err(reason)) => self.notice(&reason),
            None => self.error("The library changed; open the game again".into()),
        }
    }

    /// A button while the picker is open. The library may have been rescanned
    /// since it opened; its game is then gone and the picker closes.
    fn press_in_picker(&mut self, picker: Picker, button: Button) {
        let Some((system, game)) = picker.game().resolve(self.catalog.library()) else { return };
        match picker.press(button, system, game, &mut self.emulators) {
            Step::Open(picker) => self.picker = Some(picker),
            Step::Closed => {}
            Step::Chosen(message) => { self.notice(&message); self.rows_for = None; }
            Step::Failed(message) => self.error(message),
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

    pub fn session_started(&mut self, purpose: LaunchPurpose) {
        let game = match purpose {
            LaunchPurpose::Game(game) => game,
            LaunchPurpose::PortMaster => {
                if let Some(ports) = &mut self.ports { ports.rescan_on_return = true; }
                return;
            }
            LaunchPurpose::RetroArchSettings | LaunchPurpose::Stream => return,
        };
        // Persist before waiting for the emulator: history survives a reset
        // during gameplay, and failed spawns never enter this callback.
        if let Err(error) = self.catalog.launched(game.clone()) {
            eprintln!("ghazal: saving history: {error:#}");
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
            eprintln!("ghazal: emulator session: {e:#}");
            self.error(format!("{e:#}"));
        }
        if self.ports.as_mut().is_some_and(|ports| std::mem::take(&mut ports.rescan_on_return)) {
            self.refresh_ports();
            self.start_scan(false);
        }
        self.refresh_status(Instant::now());
        // RetroArch's menu may have signed in or changed the achievements settings.
        if self.view == View::Configuration { self.refresh_achievements(); self.rows_for = None; }
        self.sync();
    }

    /// Periodic housekeeping: scan results, art, downloads, toast expiry, clock and battery.
    pub fn tick(&mut self, now: Instant) {
        if self.levels_visible_until.is_some_and(|until| now >= until) {
            self.levels_visible_until = None;
        }
        self.receive_scan();
        self.receive_art();
        self.receive_download();
        self.receive_ports_install();
        self.receive_hosts();
        if self.updater.poll() { self.rows_for = None; }
        if self.moonlight.as_mut().is_some_and(moonlight::Moonlight::poll) {
            self.relay_moonlight_notice();
            self.rows_for = None;
        }
        self.poll_wifi(now);
        self.poll_bluetooth();
        self.poll_syncthing(now);
        if self.toast.as_ref().is_some_and(|toast| now >= toast.shown + TOAST_DURATION) {
            self.toast = None;
        }
        // An install takes a while: keep saying so.
        if let Some(step) = self.ports.as_ref().filter(|ports| ports.install.is_some()).map(|ports| ports.step) {
            if self.toast.is_none() { self.notice(step); }
        }
        if now >= self.status_checked + STATUS_REFRESH {
            self.refresh_status(now);
        }
        if now >= self.indicators_checked + INDICATOR_REFRESH {
            self.refresh_indicators(now);
        }
        self.sync();
    }

    fn poll_bluetooth(&mut self) {
        let Some(screen) = &mut self.bluetooth else { return };
        let (changed, notices) = screen.poll();
        if changed { self.rows_for = None; }
        notices.into_iter().for_each(|notice| self.show(notice));
    }

    fn show(&mut self, notice: crate::bluetooth::model::Notice) {
        if notice.error { self.error(notice.text) } else { self.notice(&notice.text) }
        self.rows_for = None;
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
        eprintln!("ghazal: {} systems, {} games", library.systems().len(), library.game_count());
        if announce {
            self.notice(&format!("Found {} games", library.game_count()));
        }
        // System indices may have changed; keep only the collections' cursors.
        self.game_cursors.retain(|list, _| !matches!(list, GameList::System(_)));
        if matches!(self.view, View::Games(GameList::System(_))) {
            self.view = View::Systems;
        }
        self.catalog.replace_library(library);
        self.picker = None;
        self.system_cursor = self.system_cursor.min(self.system_entries().saturating_sub(1));
        if self.view == View::Information { self.refresh_information(); }
        self.rows_for = None;
    }

    fn receive_art(&mut self) {
        loop {
            let (id, image) = match self.art.as_ref().map(Loader::try_recv) {
                None | Some(Art::Pending) => return,
                Some(Art::Ready(id, image)) => (id, image),
                Some(Art::Stopped) => {
                    // Without this, the pending request would keep the menu waking.
                    eprintln!("ghazal: box art loader stopped; art is off until restart");
                    self.art = None;
                    self.art_pending = None;
                    self.ui.set_art_ready(false);
                    return;
                }
            };
            if Some(id) != self.art_pending { continue; }
            self.art_pending = None;
            match image {
                Some(pixels) => {
                    self.ui.set_art(slint::Image::from_rgba8(pixels));
                    self.ui.set_art_ready(true);
                }
                None => self.ui.set_art_ready(false),
            }
        }
    }

    fn receive_download(&mut self) {
        let Some(progress) = &self.download else { return };
        let mut latest = None;
        let stopped = loop {
            match progress.try_recv() {
                Ok(update) => latest = Some(update),
                Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
            }
        };
        match latest {
            None if stopped => {
                self.download = None;
                self.error("Thumbnail download stopped unexpectedly".into());
            }
            None => {}
            Some(Progress::Fetched { done, total }) => self.notice(&format!("Thumbnails {done}/{total}")),
            Some(Progress::Finished { found, total }) => {
                self.download = None;
                self.art_for = None;
                self.notice(&if total == 0 { "Every game already has a thumbnail".to_owned() } else { format!("Downloaded {found} of {total} thumbnails") });
            }
            Some(Progress::Failed(error)) => {
                self.download = None;
                self.error(format!("Thumbnail download stopped: {error}"));
            }
        }
    }

    fn receive_ports_install(&mut self) {
        let Some(ports) = &mut self.ports else { return };
        let Some(progress) = &ports.install else { return };
        let outcome = loop {
            match progress.try_recv() {
                Ok(ports::Progress::Step(step)) => ports.step = step,
                Ok(ports::Progress::Done) => break Some(Ok(())),
                Ok(ports::Progress::Failed(error)) => break Some(Err(error)),
                Err(std::sync::mpsc::TryRecvError::Empty) => break None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break Some(Err("the installer stopped unexpectedly".into())),
            }
        };
        let Some(outcome) = outcome else { return };
        ports.install = None;
        self.refresh_ports();
        match outcome {
            Ok(()) => self.notice("PortMaster installed: open it from Applications"),
            Err(error) => self.error(format!("Installing PortMaster: {error}")),
        }
    }

    fn refresh_ports(&mut self) {
        if let Some(ports) = &mut self.ports { ports.state = ports::portmaster_state(&ports.dir); }
        self.rows_for = None;
    }

    /// The Applications entries: PortMaster is offered where the profile has it.
    fn application_items(&self) -> Vec<MenuItem> {
        let mut items = APPLICATION_ITEMS.to_vec();
        match self.ports.as_ref().map(|ports| ports.state) {
            Some(ports::PortMaster::Installed) => items.push(MenuItem::PortMaster),
            Some(ports::PortMaster::Missing) => items.push(MenuItem::InstallPortMaster),
            None => {}
        }
        items
    }

    /// Asks the loader for the selected game's art when the selection changes.
    fn request_art(&mut self, cursor: usize) {
        let View::Games(list) = self.view else { return };
        if !self.settings.show_thumbnails || self.art_for == Some((list, cursor)) { return };
        self.art_for = Some((list, cursor));
        let paths = self.game_at(list, cursor).map(|(system, game)| thumbnails::candidates(&self.config, system, game)).unwrap_or_default();
        if let Some(loader) = &mut self.art {
            self.art_pending = Some(loader.request(paths));
        }
    }

    pub fn next_deadline(&self) -> Instant {
        let busy = self.scan.is_some()
            || self.art_pending.is_some()
            || self.download.is_some()
            || self.moonlight.as_ref().is_some_and(moonlight::Moonlight::busy)
            || self.ports.as_ref().is_some_and(|ports| ports.install.is_some())
            || matches!(self.join, Some(Join::Scanning(_)));
        let mut deadline = if busy { Instant::now() + if self.scan.is_some() { SCAN_POLL } else { BACKGROUND_POLL } } else { self.status_checked + STATUS_REFRESH };
        deadline = deadline.min(self.indicators_checked + INDICATOR_REFRESH);
        if let Some(until) = self.levels_visible_until { deadline = deadline.min(until); }
        if self.bluetooth.is_some() { deadline = deadline.min(Instant::now() + BLUETOOTH_POLL); }
        if let Some(toast) = &self.toast { deadline = deadline.min(toast.shown + TOAST_DURATION); }
        if let Some(wake) = self.wifi.as_ref().and_then(|wifi| wifi.next_deadline(Instant::now())) { deadline = deadline.min(wake); }
        let syncthing = match &self.syncthing {
            Some(controller) => controller.next_deadline(Instant::now()),
            None => Some(self.syncthing_status.next_deadline(Instant::now())),
        };
        if let Some(wake) = syncthing { deadline = deadline.min(wake); }
        deadline
    }

    /// Used by the snapshot renderer to wait for the initial worker result.
    #[cfg(feature = "desktop")]
    pub fn library_loading(&self) -> bool { self.scan.is_some() }

    pub fn refresh_levels(&mut self) {
        match crate::levels::read(&self.config.daemon_socket) {
            Ok(Some(levels)) => {
                if self.levels.is_some_and(|previous| previous != levels) {
                    self.levels_visible_until = Some(Instant::now() + TOAST_DURATION);
                }
                self.levels = Some(levels);
                if self.view == View::Configuration { self.rows_for = None; }
                self.ui.set_brightness_level(u8::from(levels.brightness).into());
                self.ui.set_volume_level(u8::from(levels.volume).into());
                self.sync();
            }
            Ok(None) => {},
            Err(error) => eprintln!("ghazal: reading live levels: {error:#}"),
        }
    }

    #[cfg(feature = "desktop")]
    pub(crate) fn snapshot_battery(&self, charging: bool) {
        self.ui.set_battery_percent(50);
        self.ui.set_battery_charging(charging);
    }

    /// Every status icon lit, with HDMI as GHAZAL_SNAPSHOT_HDMI set it.
    #[cfg(feature = "desktop")]
    pub(crate) fn snapshot_indicators(&self) {
        use indicators::{Bluetooth, Readings, Wifi};
        let readings = Readings { wifi: Wifi::Connected(Some(wifi::Signal::from_dbm(-62))), bluetooth: Bluetooth::Connected, syncthing: crate::syncthing::Summary::Syncing, hdmi: self.hdmi };
        self.ui.set_indicators(readings.view());
    }

    fn refresh_status(&mut self, now: Instant) {
        self.status_checked = now;
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
        let (h, m) = self.clock.wall_clock(unix);
        self.ui.set_clock(format!("{h:02}:{m:02}").into());
        match battery::read(None) {
            Some(b) => {
                self.ui.set_battery_percent(b.percent as i32);
                self.ui.set_battery_charging(b.charging);
            }
            None => self.ui.set_battery_percent(-1),
        }
        if self.view == View::Information { self.refresh_information(); }
        self.refresh_indicators(now);
    }

    fn refresh_indicators(&mut self, now: Instant) {
        self.indicators_checked = now;
        self.ui.set_indicators(indicators::Readings::read(self.hdmi, self.syncthing_status.summary()).view());
    }

    fn configuration_items(&self) -> Vec<MenuItem> {
        let mut items = vec![MenuItem::Startup, MenuItem::Thumbnails, MenuItem::RetroArchSettings, MenuItem::Achievements, MenuItem::AchievementsAccount];
        if wifi::available() { items.push(MenuItem::Wifi); }
        if crate::bluetooth::available() { items.push(MenuItem::Bluetooth); }
        if syncthing::available() { items.push(MenuItem::Syncthing); }
        if Store::system().available() { items.push(MenuItem::TimeZone); }
        if std::path::Path::new("/etc/rauc/system.conf").exists() { items.push(MenuItem::SystemUpdate); }
        if self.levels.is_some() { items.extend([MenuItem::Brightness, MenuItem::Volume]); }
        items.extend(self.lights.map(MenuItem::Lights));
        if self.hdmi.is_some() { items.push(MenuItem::Hdmi(self.settings.hdmi)); }
        items
    }

    pub fn hdmi_policy(&self) -> hdmi::Policy { self.settings.hdmi }

    /// What the screens are doing about HDMI; a failure reaches the user as a toast.
    pub fn set_hdmi(&mut self, status: hdmi::Status, error: Option<String>) {
        self.hdmi = Some(status);
        self.refresh_indicators(Instant::now());
        if let Some(error) = error { self.error(format!("HDMI: {error}")); }
        if self.view == View::Information { self.refresh_information(); }
        self.rows_for = None;
    }

    /// Replaces the whole menu with a message while it is shown only on the
    /// handheld's own screen (a game running on HDMI); `None` brings the menu back.
    pub fn set_notice(&mut self, notice: Option<(&str, &str)>) {
        let (title, detail) = notice.unwrap_or(("", ""));
        self.ui.set_notice(title.into());
        self.ui.set_notice_detail(detail.into());
    }

    fn step_hdmi(&mut self, forward: bool) {
        let mut settings = self.settings.clone();
        settings.hdmi = settings.hdmi.step(forward);
        match settings.save(&self.config.settings_file) {
            Ok(()) => self.settings = settings,
            Err(error) => self.error(format!("Saving HDMI setting: {error:#}")),
        }
        self.rows_for = None;
    }

    fn step_lights(&mut self, forward: bool) {
        match ctl::request(&self.config.daemon_socket, if forward { "lights next" } else { "lights prev" }) {
            Ok(reply) => self.lights = ctl::lights(&reply),
            Err(error) => self.error(format!("{error:#}")),
        }
    }

    fn adjust_level(&mut self, item: MenuItem, up: bool) {
        let (command, amount) = match item {
            MenuItem::Brightness => ("brightness", 10),
            MenuItem::Volume => ("volume", 5),
            _ => return,
        };
        let command = format!("{command} {}{amount}", if up { "+" } else { "-" });
        match ctl::request(&self.config.daemon_socket, &command) {
            Ok(_) => self.refresh_levels(),
            Err(error) => self.error(format!("{error:#}")),
        }
    }

    fn refresh_information(&mut self) {
        let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "ghazal".into());
        let uptime = std::fs::read_to_string("/proc/uptime").ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
            .map(|seconds| format!("{} min", (seconds / 60.0) as u64)).unwrap_or_else(|| "Unavailable".into());
        let address = crate::network::lan_address().map_or_else(|| "Disconnected".into(), |address| address.to_string());
        self.information_rows = [
            ("Software", format!("ghazal {}", env!("CARGO_PKG_VERSION"))),
            ("Device name", hostname.trim().to_owned()),
            ("Kernel", crate::sys::kernel_release()),
            ("LAN address", address),
            ("Data card", std::fs::read_to_string("/run/ghazal/data-source").map(|source| source.trim().to_owned()).unwrap_or_else(|_| "Unavailable".into())),
            ("Uptime", uptime),
            ("HDMI", self.hdmi.map_or_else(|| "Not on this device".to_owned(), hdmi::Status::describe)),
            ("Systems", self.catalog.library().systems().len().to_string()),
            ("Games", self.catalog.library().game_count().to_string()),
        ].into_iter().map(|(label, detail)| Row { label: label.into(), detail: detail.into(), glyph: "".into() }).collect();
        self.rows_for = None;
    }

    /// Scans in a worker so input and drawing continue. Only user-requested
    /// rescans announce their result.
    fn start_scan(&mut self, announce: bool) -> bool {
        let config = self.config.clone();
        let (sender, results) = std::sync::mpsc::channel();
        match std::thread::Builder::new().name("ghazal-library".into()).spawn(move || {
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

    fn page_items(&self, page: MenuPage) -> Vec<MenuItem> {
        match page {
            MenuPage::Main => self.main_items.clone(),
            MenuPage::Options => OPTION_ITEMS.to_vec(),
            MenuPage::Game => self.game_actions(),
        }
    }

    /// Playing is always offered. Netplay needs RetroArch, so a game that runs
    /// in a standalone emulator does not get it; choosing an emulator needs a
    /// system with more than one.
    fn game_actions(&self) -> Vec<MenuItem> {
        let Some((system, game)) = self.menu_game() else { return vec![MenuItem::PlayGame] };
        let netplay = matches!(self.emulators.resolve(system, game).launch, Launch::Core(_));
        [
            (MenuItem::PlayGame, true),
            (MenuItem::ChooseEmulator, system.emulators.len() > 1),
            (MenuItem::HostNetplay, netplay),
            (MenuItem::JoinGameNetplay, netplay),
        ].into_iter().filter_map(|(item, offered)| offered.then_some(item)).collect()
    }

    /// The game the open actions menu is about, unless the library changed under it.
    fn menu_game(&self) -> Option<(&System, &Game)> {
        let GameMenu { list, cursor, id } = self.game_menu.as_ref()?;
        self.game_at(*list, *cursor).filter(|(system, game)| GameId::new(system, game) == *id)
    }

    fn open_game_menu(&mut self, list: GameList, cursor: usize) {
        let Some((system, game)) = self.game_at(list, cursor) else { return };
        let id = GameId::new(system, game);
        self.game_menu = Some(GameMenu { list, cursor, id });
        self.open_menu(MenuPage::Game);
    }

    fn open_menu(&mut self, page: MenuPage) {
        self.menu = Some(MenuState { page, cursor: 0 });
    }

    fn activate(&mut self, item: MenuItem) -> Option<Effect> {
        // Option entries keep the Options page open; others close the menu.
        if !matches!(item, MenuItem::Options | MenuItem::Startup | MenuItem::Thumbnails | MenuItem::Back) {
            self.menu = None;
        }
        match item {
            MenuItem::SystemUpdate => {
                match self.updater.open() { Ok(()) => self.view = View::Updates, Err(error) => self.error(format!("{error:#}")) }
                None
            },
            MenuItem::Wifi => { self.open_wifi(); None }
            MenuItem::Syncthing => { self.open_syncthing(); None }
            MenuItem::TimeZone => { self.open_time_zone(); None }
            MenuItem::Bluetooth => {
                match bluetooth::Screen::open() {
                    Ok(screen) => { self.bluetooth = Some(screen); self.view = View::Bluetooth; self.rows_for = None; }
                    Err(error) => self.error(format!("Bluetooth: {error:#}")),
                }
                None
            }
            MenuItem::RetroArchSettings => match LaunchSpec::retroarch_settings(&self.config) {
                Ok(spec) => Some(Effect::Launch { spec, purpose: LaunchPurpose::RetroArchSettings }),
                Err(error) => { self.error(format!("RetroArch settings: {error:#}")); None }
            },
            MenuItem::Startup => {
                let mut settings = self.settings.clone();
                settings.startup = settings.startup.next();
                match settings.save(&self.config.settings_file) {
                    Ok(()) => { self.settings = settings; self.notice("Startup setting saved for next boot"); }
                    Err(error) => self.error(format!("Saving startup setting: {error:#}")),
                }
                None
            }
            MenuItem::Rescan => {
                if self.scan.is_none() && self.start_scan(true) {
                    self.notice("Scanning library…");
                }
                None
            }
            MenuItem::Options => { self.open_menu(MenuPage::Options); None }
            MenuItem::Back => { self.open_menu(MenuPage::Main); None }
            MenuItem::Thumbnails => {
                self.settings.show_thumbnails = !self.settings.show_thumbnails;
                if let Err(error) = self.settings.save(&self.config.settings_file) { self.error(format!("{error:#}")); }
                self.art_for = None;
                None
            }
            MenuItem::DownloadThumbnails => {
                if self.download.is_some() {
                    self.notice("Already downloading thumbnails");
                } else {
                    match thumbnails::download(self.config.clone()) {
                        Ok(progress) => { self.download = Some(progress); self.notice("Looking for missing thumbnails…"); }
                        Err(error) => self.error(format!("Downloading thumbnails: {error:#}")),
                    }
                }
                None
            }
            MenuItem::PortMaster => {
                let launch = match (&self.ports, &self.config.ports) {
                    (Some(menu), Some(config)) => ports::launch_spec(&self.config, config, &ports::portmaster_script(&menu.dir)),
                    _ => return None,
                };
                match launch {
                    Ok(spec) => Some(Effect::Launch { spec, purpose: LaunchPurpose::PortMaster }),
                    Err(error) => { self.error(format!("PortMaster: {error:#}")); None }
                }
            }
            MenuItem::InstallPortMaster => {
                if self.ports.as_ref().is_none_or(|ports| ports.install.is_some()) {
                    self.notice("Already installing PortMaster");
                    return None;
                }
                match ports::install_in_background(&self.config) {
                    Ok(progress) => if let Some(ports) = &mut self.ports { ports.install = Some(progress); ports.step = "Installing PortMaster…"; },
                    Err(error) => self.error(format!("Installing PortMaster: {error:#}")),
                }
                None
            }
            MenuItem::Sleep => self.power("suspend"),
            MenuItem::Restart => self.power("reboot"),
            MenuItem::PowerOff => self.power("poweroff"),
            MenuItem::Quit => Some(Effect::Quit),
            MenuItem::Brightness | MenuItem::Volume => { self.adjust_level(item, true); None }
            MenuItem::Lights(_) => { self.step_lights(true); None }
            MenuItem::Hdmi(_) => { self.step_hdmi(true); None }
            MenuItem::Moonlight => {
                match moonlight::Moonlight::open(&self.config) {
                    Some(session) => { self.moonlight = Some(session); self.view = View::Moonlight; self.relay_moonlight_notice(); }
                    None => self.notice("Moonlight is not available on this device"),
                }
                None
            }
            MenuItem::Achievements => { self.cycle_achievements(); self.rows_for = None; None }
            MenuItem::AchievementsAccount => match LaunchSpec::retroarch_achievements(&self.config) {
                Ok(spec) => Some(Effect::Launch { spec, purpose: LaunchPurpose::RetroArchSettings }),
                Err(error) => { self.error(format!("Achievements account: {error:#}")); None }
            },
            MenuItem::JoinNetplay => { self.start_join(Target::Any, None); None }
            MenuItem::PlayGame => self.play_menu_game(None),
            MenuItem::ChooseEmulator => { self.open_emulator_picker(); None }
            MenuItem::HostNetplay => self.play_menu_game(Some(self.session(Role::Host))),
            MenuItem::JoinGameNetplay => { self.join_menu_game(); None }
        }
    }

    fn session(&self, role: Role) -> Session {
        Session { role, nick: netplay::local_nickname() }
    }

    /// Starts the game the actions menu was opened on, alone or as a netplay session.
    fn play_menu_game(&mut self, session: Option<Session>) -> Option<Effect> {
        let Some((system, game)) = self.menu_game() else {
            self.error("The library changed; open the game again".into());
            return None;
        };
        let id = GameId::new(system, game);
        let emulator = self.emulators.resolve(system, game);
        let spec = match &session {
            Some(session) => LaunchSpec::netplay(&self.config, emulator, game, session),
            None => LaunchSpec::new(&self.config, emulator, game),
        };
        match spec {
            Ok(spec) => Some(Effect::Launch { spec, purpose: LaunchPurpose::Game(id) }),
            Err(error) => { self.error(format!("{error:#}")); None }
        }
    }

    /// Looks for hosts of the game the actions menu was opened on.
    fn join_menu_game(&mut self) {
        let Some((system, game)) = self.menu_game() else {
            self.error("The library changed; open the game again".into());
            return;
        };
        let target = Target::game(system, game);
        let back = self.game_menu.as_ref().map(|menu| menu.list);
        self.start_join(target, back);
    }

    /// Looks for hosts on a worker thread and shows the list as it arrives.
    fn start_join(&mut self, target: Target, back: Option<GameList>) {
        let window = Duration::from_millis(self.config.netplay.discovery_ms);
        match Scanning::start(target, window, self.config.netplay.hosts_file.clone(), self.config.netplay.port) {
            Ok(scanning) => {
                self.join = Some(Join::Scanning(scanning));
                self.view = View::Netplay(back);
                self.rows_for = None;
            }
            Err(error) => self.error(format!("{error:#}")),
        }
    }

    fn leave_netplay(&mut self, back: Option<GameList>) {
        self.join = None;
        self.view = back.map_or(View::Applications, View::Games);
        self.rows_for = None;
    }

    fn receive_hosts(&mut self) {
        match self.join.take() {
            Some(Join::Scanning(scanning)) => match scanning.poll() {
                Polled::Waiting(scanning) => self.join = Some(Join::Scanning(scanning)),
                Polled::Found(hosts) => { self.join = Some(Join::Choosing(hosts)); self.rows_for = None; }
                Polled::Failed(message) => {
                    self.error(message);
                    if let View::Netplay(back) = self.view { self.leave_netplay(back); }
                }
            },
            other => self.join = other,
        }
    }

    /// Joins the host under the cursor, starting the game it is playing.
    fn join_chosen(&mut self, back: Option<GameList>) -> Option<Effect> {
        let Some(Join::Choosing(hosts)) = &self.join else { return None };
        let Pick { id, role, host_core } = match hosts.choose(self.catalog.library()) {
            Ok(pick) => pick,
            Err(message) => { self.error(message); return None; }
        };
        let session = self.session(role);
        // Netplay needs the host's core, whatever emulator the game would otherwise use.
        let spec = id.resolve(self.catalog.library()).map(|(system, game)| {
            let emulator = netplay::joining_emulator(system, self.emulators.resolve(system, game), host_core.as_deref());
            LaunchSpec::netplay(&self.config, emulator, game, &session)
        });
        match spec {
            Some(Ok(spec)) => {
                self.leave_netplay(back);
                Some(Effect::Launch { spec, purpose: LaunchPurpose::Game(id) })
            }
            Some(Err(error)) => { self.error(format!("{error:#}")); None }
            None => { self.error("That game is not on this card".into()); None }
        }
    }

    fn refresh_achievements(&mut self) {
        self.achievements = self.config.retroarch.user_config.as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Achievements::read(&text))
            .unwrap_or_default();
    }

    /// Off, then on, then hardcore. Needs an account: RetroArch would otherwise
    /// complain at every game start.
    fn cycle_achievements(&mut self) {
        self.refresh_achievements();
        let mode = self.achievements.mode;
        if mode == Mode::Off && !self.achievements.has_account() {
            self.notice("Set up your account first: Achievements account");
            return;
        }
        let next = mode.next();
        // Through the launcher's accessor: it creates the config from the template, never from nothing.
        let written = LaunchSpec::user_config(&self.config)
            .and_then(|path| path.ok_or_else(|| anyhow::anyhow!("RetroArch has no user configuration")))
            .and_then(|path| {
                let text = std::fs::read_to_string(&path)?;
                crate::sys::write_atomic_private(&path, next.edits().apply(&text).as_bytes())
            });
        match written {
            Ok(()) => {
                self.achievements.mode = next;
                self.notice(match next {
                    Mode::Off => "Achievements off",
                    Mode::Softcore => "Achievements on; save states keep working",
                    Mode::Hardcore => "Hardcore on; save states and rewind are off",
                });
            }
            Err(error) => self.error(format!("Achievements: {error:#}")),
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

    fn relay_moonlight_notice(&mut self) {
        match self.moonlight.as_mut().and_then(moonlight::Moonlight::take_notice) {
            Some(Notice::Info(text)) => self.notice(&text),
            Some(Notice::Error(text)) => self.error(text),
            None => {}
        }
    }

    fn error(&mut self, text: String) {
        self.toast = Some(Toast { text, shown: Instant::now(), error: true });
    }

    fn system_entries(&self) -> usize {
        self.catalog.library().systems().len()
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
            GameList::Favorites => "Collection",
            GameList::Recent => "History",
        }
    }

    fn row(&self, view: View, index: usize) -> Option<Row> {
        Some(match view {
            View::Home => {
                let item = *self.home_items.get(index)?;
                let detail = match item {
                    HomeItem::Explore => self.catalog.library().game_count().to_string(),
                    HomeItem::Collection => self.game_count(GameList::Favorites).to_string(),
                    HomeItem::History => self.game_count(GameList::Recent).to_string(),
                    _ => String::new(),
                };
                Row { label: item.label().into(), detail: detail.into(), glyph: item.glyph().into() }
            }
            View::Systems => {
                let list = GameList::System(index);
                Row { label: self.list_title(list).into(), detail: self.game_count(list).to_string().into(), glyph: "".into() }
            }
            View::Applications => Row { label: self.application_items().get(index)?.label(&self.settings).into(), detail: "".into(), glyph: "".into() },
            View::Moonlight => {
                let row = self.moonlight.as_ref()?.row(index)?;
                Row { label: row.label.into(), detail: row.detail.into(), glyph: row.glyph.into() }
            }
            View::Configuration => {
                let item = *self.configuration_items().get(index)?;
                let detail = match (item, self.levels) {
                    (MenuItem::Brightness, Some(levels)) => format!("{}%", u8::from(levels.brightness)),
                    (MenuItem::Volume, Some(levels)) => format!("{}%", u8::from(levels.volume)),
                    (MenuItem::Achievements, _) => self.achievements.detail(),
                    (MenuItem::Hdmi(_), _) => self.hdmi.map_or_else(String::new, hdmi::Status::describe),
                    (MenuItem::TimeZone, _) => Store::system().current().map_or_else(|| "UTC".to_owned(), |zone| zone.name().replace('_', " ")),
                    _ => String::new(),
                };
                Row { label: item.label(&self.settings).into(), detail: detail.into(), glyph: "".into() }
            }
            View::Updates => { let (label, detail) = self.updater.row(index); Row { label: label.into(), detail: detail.into(), glyph: "".into() } },
            View::Wifi => {
                let row = self.wifi_view.as_ref()?.rows.get(index)?;
                Row { label: row.label.as_str().into(), detail: row.detail.as_str().into(), glyph: row.glyph.into() }
            }
            View::Bluetooth => { let line = self.bluetooth.as_ref()?.line(index)?; Row { label: line.label.into(), detail: line.detail.into(), glyph: line.glyph.into() } }
            View::TimeZone => { let line = self.time_zone.as_ref()?.line(index)?; Row { label: line.label.into(), detail: line.detail.into(), glyph: "".into() } }
            View::Syncthing => {
                let row = self.syncthing_view.as_ref()?.rows.get(index)?;
                Row { label: row.label.as_str().into(), detail: row.detail.as_str().into(), glyph: row.glyph.into() }
            }
            View::Information => self.information_rows.get(index)?.clone(),
            View::Netplay(_) => {
                let Some(Join::Choosing(hosts)) = &self.join else { return None };
                let (label, detail) = hosts.row(index)?;
                Row { label: label.into(), detail: detail.into(), glyph: "".into() }
            }
            View::Games(list) => {
                let (system, game) = self.game_at(list, index)?;
                let favorite = if self.catalog.is_favorite(&GameId::new(system, game)) { "★" } else { "" };
                // Collections mix systems, so name the game's system; a game pinned to an emulator names it.
                let system_name = (!matches!(list, GameList::System(_))).then_some(system.name.as_str());
                let pinned = self.emulators.pinned(system, game).map(|emulator| emulator.label.as_str());
                let detail = [(!favorite.is_empty()).then_some(favorite), system_name, pinned].into_iter().flatten().collect::<Vec<_>>().join(" ");
                Row { label: game.title.as_str().into(), detail: detail.into(), glyph: "".into() }
            }
        })
    }

    fn hints(&self) -> Hints {
        let compact = self.ui.window().size().width < 400;
        if self.picker.is_some() { return if compact { emulator_picker::COMPACT_HINTS } else { emulator_picker::HINTS }; }
        if self.menu.is_none() && compact && matches!(self.view, View::Games(_)) {
            return COMPACT_GAME_HINTS;
        }
        match (self.menu.map(|m| m.page), self.view) {
            (Some(MenuPage::Options), _) => OPTIONS_HINTS,
            (Some(MenuPage::Main | MenuPage::Game), _) => MENU_HINTS,
            (None, View::Netplay(_)) => NETPLAY_HINTS,
            (None, View::Systems) => SYSTEM_HINTS,
            (None, View::Home) => HOME_HINTS,
            (None, View::Applications) => SYSTEM_HINTS,
            (None, View::Moonlight) => self.moonlight.as_ref().map_or(SYSTEM_HINTS, moonlight::Moonlight::hints),
            (None, View::Configuration) => CONFIGURATION_HINTS,
            (None, View::Updates) => OPTIONS_HINTS,
            (None, View::Wifi) => self.wifi_view.as_ref().map_or(INFORMATION_HINTS, |view| if self.ui.window().size().width < 560 { view.narrow_hints } else { view.hints }),
            (None, View::Bluetooth) => self.bluetooth.as_ref().map_or(OPTIONS_HINTS, bluetooth::Screen::hints),
            (None, View::TimeZone) => self.time_zone.as_ref().map_or(OPTIONS_HINTS, timezone::Screen::hints),
            (None, View::Syncthing) => self.syncthing_view.as_ref().map_or(INFORMATION_HINTS, |view| view.hints),
            (None, View::Information) => INFORMATION_HINTS,
            (None, View::Games(GameList::System(_))) => GAME_HINTS,
            (None, View::Games(_)) => COLLECTION_HINTS,
        }
    }

    /// The list floating over the screen: the emulator picker, else the menu.
    fn overlay(&self) -> Option<Overlay> {
        if let Some(picker) = &self.picker {
            let (system, game) = picker.game().resolve(self.catalog.library())?;
            let view = picker.view(system, game, &self.emulators);
            return Some(Overlay { title: view.title, labels: view.rows, cursor: view.cursor, wide: true });
        }
        let MenuState { page, cursor } = self.menu?;
        let title = match page { MenuPage::Main => "Menu", MenuPage::Options => "Options", MenuPage::Game => "Game" }.to_owned();
        let labels = self.page_items(page).iter().map(|item| item.label(&self.settings).to_owned()).collect();
        Some(Overlay { title, labels, cursor, wide: false })
    }

    /// The keyboard some screen is asking for. A screen that takes typed text
    /// adds itself here.
    fn active_keyboard(&self) -> Option<&keyboard::Display> {
        self.wifi_view.as_ref().and_then(|view| view.keyboard.as_ref()).filter(|_| self.view == View::Wifi)
    }

    /// Shows the on-screen keyboard for whichever screen is asking for text.
    fn sync_keyboard(&mut self) {
        let state = self.ui.global::<KeyboardState>();
        let keyboard = self.active_keyboard();
        state.set_visible(keyboard.is_some());
        let Some(keyboard) = keyboard else { return };
        state.set_title(keyboard.title.as_str().into());
        let (note, is_error) = match &keyboard.note {
            keyboard::Note::Help(text) => (text, false),
            keyboard::Note::Problem(text) => (text, true),
        };
        state.set_note(note.as_str().into());
        state.set_note_is_error(is_error);
        state.set_text(keyboard.shown.as_str().into());
        state.set_selected(keyboard.selected as i32);
        if keyboard.keys != self.keyboard_keys {
            let caps: Vec<KeyCap> = keyboard.keys.iter().map(|key| KeyCap { label: key.label.as_str().into(), row: key.row as i32, start: key.start as i32, width: key.width as i32, action: key.action }).collect();
            state.set_keys(ModelRc::new(VecModel::from(caps)));
            self.keyboard_keys = keyboard.keys.clone();
        }
    }

    /// Pushes state into the UI. Slint only repaints what actually changed.
    fn sync(&mut self) {
        let (cursor, total) = match self.view {
            View::Home => (self.home_cursor, self.home_items.len()),
            View::Systems => (self.system_cursor, self.system_entries()),
            View::Games(list) => (self.cursor(list), self.game_count(list)),
            View::Applications => (self.application_cursor, self.application_items().len()),
            View::Moonlight => self.moonlight.as_ref().map_or((0, 0), |session| (session.cursor(), session.len())),
            View::Configuration => (self.configuration_cursor, self.configuration_items().len()),
            View::Updates => (self.updater.cursor, 4),
            View::Wifi => self.wifi_view.as_ref().map_or((0, 0), |view| (view.cursor, view.rows.len())),
            View::Bluetooth => self.bluetooth.as_ref().map_or((0, 0), |screen| (screen.cursor(), screen.len())),
            View::TimeZone => self.time_zone.as_ref().map_or((0, 0), |screen| (screen.cursor(), screen.len())),
            View::Syncthing => self.syncthing_view.as_ref().map_or((0, 0), |view| (view.cursor, view.rows.len())),
            View::Information => (self.information_cursor, self.information_rows.len()),
            View::Netplay(_) => match &self.join {
                Some(Join::Choosing(hosts)) => (hosts.cursor, hosts.len()),
                _ => (0, 0),
            },
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
        let moonlight_heading = self.moonlight.as_ref().map(moonlight::Moonlight::heading).unwrap_or_default();
        ui.set_heading(match self.view {
            View::Home => "Ghazal",
            View::Systems => "Explore Content",
            View::Games(list) => self.list_title(list),
            View::Applications => "Applications",
            View::Moonlight => moonlight_heading.as_str(),
            View::Configuration => "Configuration",
            View::Updates => "System update",
            View::Wifi => "Wi-Fi",
            View::Bluetooth => "Bluetooth",
            View::Syncthing => "Syncthing",
            View::TimeZone => "Time zone",
            View::Information => "Information",
            View::Netplay(_) => "Join netplay",
        }.into());
        ui.set_position(if total > 0 { format!("{} / {total}", cursor + 1) } else { String::new() }.into());
        ui.set_empty_message(match self.view {
            View::Netplay(_) => match self.join {
                Some(Join::Choosing(_)) => "No hosts found. Ask a friend to choose Host netplay on this Wi-Fi.".to_owned(),
                _ => "Looking for hosts…".to_owned(),
            },
            _ if self.scan.is_some() => "Loading games…".to_owned(),
            View::Games(GameList::Favorites) => "Press Y on a game to add a favorite".to_owned(),
            View::Games(GameList::Recent) => "Games you launch will appear here".to_owned(),
            _ => format!("No games found in {}", self.config.rom_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")),
        }.into());
        ui.set_selected(cursor as i32);
        let overlay = self.overlay();
        ui.set_menu_selected(overlay.as_ref().map_or(-1, |overlay| overlay.cursor as i32));
        if let Some(Overlay { title, labels, wide, .. }) = overlay {
            if labels != self.menu_labels {
                ui.set_menu_items(ModelRc::new(VecModel::from(labels.iter().map(|l| SharedString::from(l.as_str())).collect::<Vec<_>>())));
                self.menu_labels = labels;
            }
            ui.set_menu_title(title.into());
            ui.set_menu_wide(wide);
        }
        let show_art = self.settings.show_thumbnails && matches!(self.view, View::Games(_)) && total > 0;
        self.ui.set_art_visible(show_art);
        let device_id = self.syncthing_view.as_ref().filter(|_| self.view == View::Syncthing).and_then(|view| view.device_id.clone());
        let code = device_id.as_ref().and_then(|id| Some((id, self.syncthing_code(id)?)));
        self.ui.set_code_visible(code.is_some());
        if let Some((id, image)) = code {
            self.ui.set_code(image);
            self.ui.set_code_caption(id.lines().join("\n").into());
        }
        let ui = &self.ui;

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
        self.sync_keyboard();
        if show_art {
            self.request_art(cursor);
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
pub(crate) fn navigate(cursor: usize, len: usize, button: Button) -> usize {
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
    fn home_categories_and_power_capabilities() {
        let entries = HomeItem::entries(true);
        assert_eq!(entries.iter().map(|item| item.label()).collect::<Vec<_>>(), ["Explore Content", "Collection", "History", "Applications", "Information", "Configuration", "Reboot", "Shutdown"]);
        assert_eq!(HomeItem::entries(false).len(), 6);
        assert!(!HomeItem::entries(false).iter().any(|item| matches!(item, HomeItem::Reboot | HomeItem::Shutdown)));
    }

    #[test]
    fn collections_return_home_and_system_games_return_to_explore() {
        assert!(GameList::Favorites.parent() == View::Home);
        assert!(GameList::Recent.parent() == View::Home);
        assert!(GameList::System(0).parent() == View::Systems);
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
