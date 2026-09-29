use std::time::{Duration, Instant};

const REPEAT_DELAY: Duration = Duration::from_millis(320);
const REPEAT_INTERVAL: Duration = Duration::from_millis(70);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    X,
    Y,
    L1,
    R1,
    L2,
    R2,
    Select,
    Start,
    Menu,
}

impl Button {
    fn repeats(self) -> bool {
        matches!(self, Self::Up | Self::Down | Self::Left | Self::Right | Self::L1 | Self::R1)
    }
}

/// Turns raw press/release edges into button actions, adding auto-repeat for held
/// navigation buttons. Gamepad drivers don't repeat on their own.
#[derive(Default)]
pub struct Repeater {
    held: Vec<(Button, Instant)>,
    pending: Vec<Button>,
}

impl Repeater {
    pub fn press(&mut self, button: Button, now: Instant) {
        self.pending.push(button);
        if button.repeats() {
            self.held.retain(|(b, _)| *b != button);
            self.held.push((button, now + REPEAT_DELAY));
        }
    }

    pub fn release(&mut self, button: Button) {
        self.held.retain(|(b, _)| *b != button);
    }

    pub fn tick(&mut self, now: Instant) {
        for (button, next) in &mut self.held {
            if now >= *next {
                self.pending.push(*button);
                *next = now + REPEAT_INTERVAL;
            }
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.held.iter().map(|(_, t)| *t).min()
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, Button> {
        self.pending.drain(..)
    }

    pub fn clear(&mut self) {
        self.held.clear();
        self.pending.clear();
    }
}
