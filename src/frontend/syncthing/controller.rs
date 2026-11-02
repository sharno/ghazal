//! Runs the Syncthing screen: a worker thread for the REST calls and the timing
//! of refreshes. Every decision is in `screen::step`; this is the edge where
//! its commands run. The worker starts with the screen and stops when the
//! controller is dropped.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::screen::{self, Command, Event, Notice, Screen, View};
use crate::frontend::input::Button;
use crate::syncthing::{Backend, Summary, Timestamp};

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
    next_refresh: Instant,
}

fn execute(backend: &dyn Backend, command: Command) -> Event {
    match command {
        Command::Observe => Event::Observed(backend.observe()),
        Command::CarryOut(order) => {
            let result = backend.carry_out(&order);
            Event::Done(order, result)
        }
    }
}

impl Controller {
    pub fn open(backend: Arc<dyn Backend>, now: Instant) -> std::io::Result<Self> {
        let (commands, work) = mpsc::channel::<Command>();
        let (done, events) = mpsc::channel();
        std::thread::Builder::new().name("ghazal-syncthing".into()).spawn(move || {
            // Ends when the controller is dropped and the channel closes.
            for command in work {
                if done.send(execute(&*backend, command)).is_err() { break; }
            }
        })?;
        let mut controller = Self { screen: Screen::Loading, commands, events, in_flight: 0, next_refresh: now };
        controller.send(Command::Observe);
        Ok(controller)
    }

    fn send(&mut self, command: Command) {
        // A dead worker is noticed by `poll`.
        if self.commands.send(command).is_ok() { self.in_flight += 1; }
    }

    pub fn handle(&mut self, button: Button, now: Instant) -> Reaction {
        self.apply(Event::Button(button), now)
    }

    fn apply(&mut self, event: Event, now: Instant) -> Reaction {
        let step = screen::step(std::mem::replace(&mut self.screen, Screen::Closed), event);
        self.screen = step.screen;
        if let Some(command) = step.command { self.send(command); }
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
                // The worker died: nothing on this screen can work any more.
                Err(TryRecvError::Disconnected) => {
                    if !self.screen.closed() {
                        self.in_flight = 0;
                        self.screen = Screen::Closed;
                        reaction = Reaction { changed: true, notice: Some(Notice::Error("Syncthing helper stopped".into())) };
                    }
                    break;
                }
            }
        }
        if self.in_flight == 0 && self.screen.refresh_interval().is_some() && now >= self.next_refresh {
            self.send(Command::Observe);
        }
        reaction
    }

    /// When `poll` next has something to do.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        if self.in_flight > 0 { return Some(now + WORKER_POLL); }
        self.screen.refresh_interval().map(|_| self.next_refresh)
    }

    /// `now` dates "last seen" in the rows.
    pub fn view(&self, now: Timestamp) -> View { screen::view(&self.screen, now) }
    pub fn closed(&self) -> bool { self.screen.closed() }
    /// What the status icon shows, from the screen's own last look.
    pub fn summary(&self) -> Option<Summary> { self.screen.summary() }
    /// Whether a command is running, for the snapshot renderer to wait on.
    #[cfg(any(test, feature = "desktop"))]
    pub fn working(&self) -> bool { self.in_flight > 0 }
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeBackend;
    use super::*;
    use crate::syncthing::{Exposure, Failure, Order, Switch};

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
                self.settle();
            }
        }

        fn rows(&self) -> Vec<(String, String)> {
            self.controller.view(crate::syncthing::clock()).rows.into_iter().map(|row| (row.label, row.detail)).collect()
        }

        fn row(&self, label: &str) -> String {
            self.rows().into_iter().find(|(found, _)| found == label).map(|(_, detail)| detail).unwrap_or_else(|| panic!("no row {label}"))
        }

        /// Moves down to the row labelled `label`.
        fn goto(&mut self, label: &str) {
            for _ in 0..30 {
                let view = self.controller.view(crate::syncthing::clock());
                if view.rows[view.cursor].label == label { return; }
                self.press(&[Button::Down]);
            }
            panic!("no row {label}");
        }

        fn last_notice(&self) -> Option<&str> { self.notices.last().map(Notice::text) }
    }

    #[test]
    fn opening_loads_the_list_in_the_background() {
        let harness = Harness::new(FakeBackend::demo());
        assert_eq!(harness.rows()[0], ("Syncthing".to_owned(), "On, syncing".to_owned()));
        assert_eq!(harness.fake.orders(), []);
        assert_eq!(harness.controller.summary(), Some(Summary::Syncing));
    }

    #[test]
    fn accepting_a_device_pairs_it_and_the_list_follows() {
        let mut harness = Harness::new(FakeBackend::demo());
        harness.goto("New device: Laptop");
        harness.press(&[Button::A]);
        assert!(harness.last_notice().is_some_and(|notice| notice.starts_with("Added Laptop")));
        assert_eq!(harness.row("Laptop"), "Connected");
        assert!(matches!(harness.fake.orders().as_slice(), [Order::AcceptDevice(_)]));
    }

    #[test]
    fn merging_a_folder_asks_and_then_syncs_it() {
        let mut harness = Harness::new(FakeBackend::demo());
        harness.goto("ROMS from Desktop PC");
        harness.press(&[Button::A]);
        assert!(harness.fake.orders().is_empty());
        harness.press(&[Button::Down, Button::A]);
        assert_eq!(harness.last_notice(), Some("Syncing ROMS in /mnt/games/ROMS"));
        assert_eq!(harness.row("ROMS"), "Up to date");
    }

    #[test]
    fn turning_syncthing_off_leaves_only_its_switch() {
        let mut harness = Harness::new(FakeBackend::demo());
        harness.press(&[Button::A]);
        assert_eq!(harness.rows(), [("Syncthing".to_owned(), "Off".to_owned())]);
        assert_eq!(harness.controller.summary(), Some(Summary::Off));
        harness.press(&[Button::A]);
        assert_eq!(harness.fake.orders(), [Order::Switch(Switch::Off), Order::Switch(Switch::On)]);
        assert_eq!(harness.rows()[0].1, "On, syncing");
    }

    #[test]
    fn the_web_interface_comes_with_a_login() {
        let mut harness = Harness::new(FakeBackend::demo());
        harness.goto("Web interface on the network");
        harness.press(&[Button::A]);
        assert_eq!(harness.row("Web interface on the network"), "http://192.168.1.133:8384");
        assert!(harness.row("Web interface login").starts_with("ghazal / "));
        assert_eq!(harness.fake.orders(), [Order::Expose(Exposure::Network)]);
    }

    #[test]
    fn a_failed_order_is_reported_and_the_list_stays() {
        let fake = FakeBackend::demo();
        fake.fail_next(Failure::TimedOut);
        let mut harness = Harness::new(fake);
        harness.goto("Saves");
        harness.press(&[Button::A]);
        assert_eq!(harness.last_notice(), Some("Syncthing did not answer in time"));
        assert_eq!(harness.row("Saves"), "Syncing 42%");
    }

    #[test]
    fn refreshes_follow_the_clock_and_stop_with_the_screen() {
        let mut harness = Harness::new(FakeBackend::demo());
        let observed = harness.fake.observations();
        harness.clock += Duration::from_secs(3);
        harness.settle();
        assert_eq!(harness.fake.observations(), observed + 1);
        harness.press(&[Button::B]);
        assert!(harness.controller.closed());
        assert_eq!(harness.controller.next_deadline(harness.clock), None);
        let weak = Arc::downgrade(&harness.fake);
        drop(harness.controller);
        let give_up = Instant::now() + Duration::from_secs(2);
        // The worker held a clone of the backend; it lets go when it exits.
        while weak.strong_count() > 1 && Instant::now() < give_up { std::thread::sleep(Duration::from_millis(5)); }
        assert_eq!(weak.strong_count(), 1);
    }
}
