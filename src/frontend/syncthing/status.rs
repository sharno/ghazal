//! Syncthing's word for the status bar. Asking takes a few REST calls, so it
//! runs on a short-lived thread at most every ten seconds while the menu is up;
//! the icon shows the last answer meanwhile.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::syncthing::Summary;

const EVERY: Duration = Duration::from_secs(10);
/// How soon the app looks for an answer that is on its way.
const ANSWER_POLL: Duration = Duration::from_millis(250);

pub struct Status {
    summary: Summary,
    answer: Option<Receiver<Summary>>,
    asked: Instant,
}

impl Status {
    /// Hidden until the first answer, so a Syncthing the player turned off never
    /// flashes. The first question waits a full interval: at boot Syncthing only
    /// starts after the menu's first frame, and the client's code would be paged
    /// in while the menu's still is.
    pub fn new(now: Instant) -> Self { Self { summary: Summary::Off, answer: None, asked: now } }

    pub fn summary(&self) -> Summary { self.summary }

    /// Takes an answer that arrived and asks again when one is due. Returns
    /// whether the summary changed.
    pub fn poll(&mut self, now: Instant, ask: impl FnOnce() -> Summary + Send + 'static) -> bool {
        let before = self.summary;
        if let Some(answer) = &self.answer {
            match answer.try_recv() {
                Ok(summary) => { self.summary = summary; self.answer = None; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.answer = None,
            }
        }
        if self.answer.is_none() && now >= self.asked + EVERY {
            self.asked = now;
            let (sender, answer) = mpsc::channel();
            // A thread that cannot start leaves the channel closed: the next poll asks again.
            let _ = std::thread::Builder::new().name("ghazal-syncthing-status".into()).spawn(move || { let _ = sender.send(ask()); });
            self.answer = Some(answer);
        }
        self.summary != before
    }

    /// What the Syncthing screen just saw; it counts as an answer.
    pub fn record(&mut self, summary: Summary, now: Instant) -> bool {
        let changed = self.summary != summary;
        self.summary = summary;
        self.asked = now;
        changed
    }

    /// When `poll` next has something to do.
    pub fn next_deadline(&self, now: Instant) -> Instant {
        match self.answer {
            Some(_) => now + ANSWER_POLL,
            None => self.asked + EVERY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(status: &mut Status, now: Instant) -> bool {
        let give_up = Instant::now() + Duration::from_secs(5);
        let mut changed = false;
        while status.answer.is_some() {
            changed |= status.poll(now, || unreachable!("asked twice"));
            assert!(Instant::now() < give_up);
            std::thread::sleep(Duration::from_millis(5));
        }
        changed
    }

    #[test]
    fn answers_arrive_in_the_background_and_asking_is_spaced_out() {
        let opened = Instant::now();
        let mut status = Status::new(opened);
        // Nothing is asked while the menu starts.
        assert!(!status.poll(opened + Duration::from_secs(9), || unreachable!("too soon")));
        assert_eq!(status.summary(), Summary::Off);
        let start = opened + EVERY;
        assert!(!status.poll(start, || Summary::Syncing));
        assert!(settle(&mut status, start));
        assert_eq!(status.summary(), Summary::Syncing);
        // Not again before ten seconds.
        assert!(!status.poll(start + Duration::from_secs(9), || unreachable!("too soon")));
        assert_eq!(status.next_deadline(start), start + EVERY);
        status.poll(start + EVERY, || Summary::UpToDate);
        settle(&mut status, start + EVERY);
        assert_eq!(status.summary(), Summary::UpToDate);
    }

    #[test]
    fn the_screens_own_look_counts_as_an_answer() {
        let now = Instant::now();
        let mut status = Status::new(now);
        assert!(status.record(Summary::Error, now));
        assert!(!status.poll(now + Duration::from_secs(1), || unreachable!("the screen just looked")));
        assert_eq!(status.summary(), Summary::Error);
    }
}
