//! Runs the Wi-Fi screen: a worker thread for the slow iwctl calls and the
//! timing of refreshes. Every decision is in `screen::step`; this is the edge
//! where its effects happen. Nothing runs until the screen is opened, and it
//! all stops when the controller is dropped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::backend::Backend;
use super::screen::{self, Command, Effect, Event, Notice, Screen, View};
use super::types::Failure;
use crate::frontend::input::Button;

/// How often the app looks at the worker while a command is running.
const WORKER_POLL: Duration = Duration::from_millis(50);

/// What a button press or a poll changed, for the app to show.
#[derive(Default)]
pub struct Reaction {
    pub changed: bool,
    pub notice: Option<Notice>,
}

pub struct Controller {
    screen: Screen,
    commands: Sender<Command>,
    events: Receiver<Event>,
    /// Commands sent whose answer has not arrived.
    in_flight: usize,
    /// Set when the user cancels; the connect in progress, or about to start, sees it.
    abort: Arc<AtomicBool>,
    next_refresh: Instant,
}

fn execute(backend: &dyn Backend, abort: &AtomicBool, command: Command) -> Event {
    match command {
        Command::Refresh => Event::Refreshed(backend.snapshot()),
        Command::Scan => Event::ScanStarted(backend.scan()),
        Command::Connect(request) => Event::Connected(backend.connect(&request, abort)),
        Command::Disconnect => Event::Disconnected(backend.disconnect()),
        Command::Forget(ssid) => Event::Forgotten(backend.forget(&ssid)),
    }
}

impl Controller {
    pub fn open(backend: Arc<dyn Backend>, now: Instant) -> std::io::Result<Self> {
        let (commands, work) = mpsc::channel::<Command>();
        let (done, events) = mpsc::channel();
        let abort = Arc::new(AtomicBool::new(false));
        let worker_abort = Arc::clone(&abort);
        std::thread::Builder::new().name("ghazal-wifi".into()).spawn(move || {
            // Ends when the controller is dropped and the channel closes.
            for command in work {
                if done.send(execute(&*backend, &worker_abort, command)).is_err() { break; }
            }
        })?;
        let mut controller = Self { screen: Screen::Loading, commands, events, in_flight: 0, abort, next_refresh: now };
        controller.send(Command::Refresh);
        Ok(controller)
    }

    fn send(&mut self, command: Command) {
        if matches!(command, Command::Connect(_)) { self.abort.store(false, Ordering::SeqCst); }
        // A dead worker is noticed by `poll`.
        if self.commands.send(command).is_ok() { self.in_flight += 1; }
    }

    pub fn handle(&mut self, button: Button, now: Instant) -> Reaction {
        self.apply(Event::Button(button), now)
    }

    fn apply(&mut self, event: Event, now: Instant) -> Reaction {
        let step = screen::step(std::mem::replace(&mut self.screen, Screen::Closed), event);
        self.screen = step.screen;
        match step.effect {
            Some(Effect::Run(command)) => self.send(command),
            Some(Effect::Abort) => {
                self.abort.store(true, Ordering::SeqCst);
                // Leaves iwd idle whichever way the attempt ended.
                self.send(Command::Disconnect);
            }
            None => {}
        }
        if let Some(interval) = self.screen.refresh_interval() { self.next_refresh = now + interval; }
        Reaction { changed: true, notice: step.notice }
    }

    /// Takes the worker's answers and starts the next refresh when it is due.
    pub fn poll(&mut self, now: Instant) -> Reaction {
        let mut reaction = Reaction::default();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    self.in_flight = self.in_flight.saturating_sub(1);
                    let applied = self.apply(event, now);
                    reaction = Reaction { changed: true, notice: applied.notice.or(reaction.notice) };
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.screen.closed() {
                        self.in_flight = 0;
                        self.screen = Screen::Unavailable(Failure::Other("Wi-Fi helper stopped".into()));
                        reaction.changed = true;
                    }
                    break;
                }
            }
        }
        if self.in_flight == 0 && self.screen.refresh_interval().is_some() && now >= self.next_refresh {
            self.send(Command::Refresh);
        }
        reaction
    }

    /// When `poll` next has something to do.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        if self.in_flight > 0 { return Some(now + WORKER_POLL); }
        self.screen.refresh_interval().map(|_| self.next_refresh)
    }

    pub fn view(&self) -> View { screen::view(&self.screen) }
    pub fn closed(&self) -> bool { self.screen.closed() }
    /// Whether a command is running, for the snapshot renderer to wait on.
    #[cfg(any(test, feature = "desktop"))]
    pub fn working(&self) -> bool { self.in_flight > 0 }
    /// Start and Menu type on the keyboard instead of opening the menu.
    pub fn typing(&self) -> bool { self.screen.typing() }
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeBackend;
    use super::super::types::{Security, Visibility};
    use crate::frontend::keyboard::Note;
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    struct Harness {
        controller: Controller,
        fake: Arc<FakeBackend>,
        clock: Instant,
        notices: Vec<Notice>,
    }

    impl Harness {
        fn new(fake: FakeBackend) -> Self {
            let fake = Arc::new(fake);
            let clock = Instant::now();
            let controller = Controller::open(fake.clone(), clock).unwrap();
            let mut harness = Self { controller, fake, clock, notices: Vec::new() };
            harness.settle();
            harness
        }

        /// Polls until the worker is idle, as the app's loop would.
        fn settle(&mut self) {
            let give_up = Instant::now() + Duration::from_secs(10);
            loop {
                let reaction = self.controller.poll(self.clock);
                self.notices.extend(reaction.notice);
                if !self.controller.working() { return; }
                assert!(Instant::now() < give_up, "worker never finished");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn press(&mut self, buttons: &[Button]) {
            for button in buttons {
                let reaction = self.controller.handle(*button, self.clock);
                self.notices.extend(reaction.notice);
            }
            self.settle();
        }

        /// Lets time pass so the next refresh is due.
        fn wait(&mut self, by: Duration) {
            self.clock += by;
            self.settle();
        }

        fn labels(&self) -> Vec<String> { self.controller.view().rows.into_iter().map(|row| row.label).collect() }
        fn last_notice(&self) -> Option<&str> { self.notices.last().map(Notice::text) }

        /// Moves the keyboard cursor to the key labelled `letter` (down to its
        /// row, then along it) and types it.
        fn type_letter(&mut self, letter: char) {
            for _ in 0..80 {
                let keyboard = self.controller.view().keyboard.expect("keyboard open");
                let target = keyboard.keys.iter().position(|key| key.label == letter.to_string()).expect("a key with that label");
                let (here, there) = (&keyboard.keys[keyboard.selected], &keyboard.keys[target]);
                if keyboard.selected == target {
                    self.press(&[Button::A]);
                    return;
                }
                self.press(&[if here.row != there.row { Button::Down } else { Button::Right }]);
            }
            panic!("could not reach {letter}");
        }
    }

    fn radio() -> FakeBackend {
        FakeBackend::new(Duration::from_millis(20))
            .with_network("Home", Security::Psk, -44, Some("correct horse"), Visibility::Listed)
            .with_network("Cafe", Security::Psk, -60, Some("espresso123"), Visibility::Listed)
            .with_network("Guest", Security::Open, -70, None, Visibility::Listed)
            .saved("Home", Security::Psk)
            .connected_to("Home")
    }

    #[test]
    fn opening_loads_the_list_in_the_background() {
        let harness = Harness::new(radio());
        assert_eq!(harness.labels(), ["Connected to Home", "Scan for networks", "Home", "Cafe", "Guest", "Join a hidden network"]);
        assert_eq!(harness.controller.view().cursor, 2);
        assert_eq!(harness.fake.calls(), ["snapshot"]);
    }

    #[test]
    fn typing_a_password_connects_and_the_status_follows() {
        let mut harness = Harness::new(radio());
        // Down to Cafe, A opens the keyboard.
        harness.press(&[Button::Down, Button::A]);
        assert!(harness.controller.typing());
        // Eight q's are the wrong password: it is refused and the keyboard comes back.
        harness.press(&[Button::A; 8]);
        harness.press(&[Button::Start]);
        assert!(harness.controller.typing());
        assert_eq!(harness.controller.view().keyboard.as_ref().unwrap().note, Note::Problem("Connection refused: check the password".into()));
        // Delete it and type the right one.
        harness.press(&[Button::B; 8]);
        for letter in "espresso123".chars() { harness.type_letter(letter); }
        harness.press(&[Button::Start]);
        assert!(!harness.controller.typing());
        assert_eq!(harness.last_notice(), Some("Connected to Cafe"));
        assert!(harness.labels()[0].starts_with("Connected to Cafe"));
        assert!(harness.fake.calls().contains(&"connect Cafe with passphrase".to_owned()));
    }

    #[test]
    fn a_cancelled_attempt_ends_in_the_list_with_iwd_left_idle() {
        let mut harness = Harness::new(FakeBackend::new(Duration::from_secs(5)).with_network("Guest", Security::Open, -50, None, Visibility::Listed));
        // Guest is the only network, so the cursor starts on it: A connects (an
        // open network), and B cancels long before the 5 seconds pass.
        let started = Instant::now();
        harness.controller.handle(Button::A, harness.clock);
        harness.controller.handle(Button::B, harness.clock);
        harness.settle();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(harness.last_notice(), Some("Cancelled"));
        assert!(harness.fake.calls().contains(&"disconnect".to_owned()));
        assert!(!harness.controller.closed());
    }

    #[test]
    fn forgetting_a_network_removes_it_from_the_list() {
        let mut harness = Harness::new(radio());
        // Home is selected and saved: Y asks, Down selects Yes, A confirms.
        harness.press(&[Button::Y, Button::Down, Button::A]);
        assert_eq!(harness.last_notice(), Some("Forgot Home"));
        assert!(harness.labels()[0].starts_with("Not connected"));
        assert!(harness.fake.calls().contains(&"forget Home".to_owned()));
    }

    #[test]
    fn scanning_polls_until_iwd_is_done() {
        let mut harness = Harness::new(radio());
        harness.press(&[Button::X]);
        assert!(harness.controller.view().rows[1].detail.contains("Scanning"));
        // Each second a refresh runs; the fake finishes scanning after two.
        for _ in 0..4 { harness.wait(SECOND * 2); }
        assert_eq!(harness.controller.view().rows[1].detail, "");
        let snapshots = harness.fake.calls().iter().filter(|call| *call == "snapshot").count();
        assert!(snapshots >= 3, "{snapshots}");
    }

    #[test]
    fn nothing_refreshes_while_the_keyboard_is_up() {
        let mut harness = Harness::new(radio());
        harness.press(&[Button::Down, Button::A]);
        let before = harness.fake.calls().len();
        harness.wait(SECOND * 30);
        assert_eq!(harness.fake.calls().len(), before);
        assert_eq!(harness.controller.next_deadline(harness.clock), None);
    }

    #[test]
    fn a_missing_daemon_shows_an_error_and_a_retry_recovers() {
        let fake = radio();
        fake.fail_next(Failure::Unavailable);
        let mut harness = Harness::new(fake);
        assert_eq!(harness.labels(), ["Wi-Fi service is not running", "Try again"]);
        harness.press(&[Button::A]);
        assert_eq!(harness.labels()[0], "Connected to Home");
    }

    #[test]
    fn b_closes_and_a_dropped_controller_stops_its_worker() {
        let mut harness = Harness::new(radio());
        harness.press(&[Button::B]);
        assert!(harness.controller.closed());
        assert_eq!(harness.controller.next_deadline(harness.clock), None);
        let weak = Arc::downgrade(&harness.fake);
        drop(harness.controller);
        let give_up = Instant::now() + Duration::from_secs(2);
        // The worker held a clone of the backend; it lets go when it exits.
        // The harness still owns one.
        while weak.strong_count() > 1 && Instant::now() < give_up { std::thread::sleep(Duration::from_millis(5)); }
        assert_eq!(weak.strong_count(), 1);
    }
}
