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

    /// Menu and Start also start hotkey chords (Menu + Vol, Menu + Start), so
    /// they act when released, and only if nothing else was pressed meanwhile.
    fn acts_on_release(self) -> bool {
        matches!(self, Self::Menu | Self::Start)
    }
}

/// A key edge from any input device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Button(Button),
    /// A key the menu doesn't use (volume, power, L3...). It still makes a
    /// held Menu or Start part of a chord.
    Other(u16),
}

/// Turns raw press/release edges into button actions: auto-repeat for held
/// navigation buttons (gamepad drivers don't repeat), and release-time actions
/// for chord-starting buttons.
#[derive(Default)]
pub struct Repeater {
    repeating: Vec<(Button, Instant)>,
    down: Vec<Key>,
    /// A release-acting button pressed alone; it fires on release unless
    /// another key joins it first.
    armed: Option<Button>,
    pending: Vec<Button>,
}

impl Repeater {
    pub fn press(&mut self, key: Key, now: Instant) {
        if self.armed.is_some_and(|armed| key != Key::Button(armed)) {
            self.armed = None;
        }
        let alone = self.down.iter().all(|down| *down == key);
        self.down.retain(|down| *down != key);
        self.down.push(key);
        let Key::Button(button) = key else { return };
        if button.acts_on_release() {
            if alone { self.armed = Some(button); }
            return;
        }
        self.pending.push(button);
        if button.repeats() {
            self.repeating.retain(|(b, _)| *b != button);
            self.repeating.push((button, now + REPEAT_DELAY));
        }
    }

    pub fn release(&mut self, key: Key) {
        self.down.retain(|down| *down != key);
        let Key::Button(button) = key else { return };
        self.repeating.retain(|(b, _)| *b != button);
        if self.armed == Some(button) {
            self.armed = None;
            self.pending.push(button);
        }
    }

    pub fn tick(&mut self, now: Instant) {
        for (button, next) in &mut self.repeating {
            if now >= *next {
                self.pending.push(*button);
                *next = now + REPEAT_INTERVAL;
            }
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.repeating.iter().map(|(_, t)| *t).min()
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, Button> {
        self.pending.drain(..)
    }

    pub fn clear(&mut self) {
        self.repeating.clear();
        self.down.clear();
        self.armed = None;
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(edges: &[(Key, bool)]) -> Vec<Button> {
        let mut repeater = Repeater::default();
        let now = Instant::now();
        for &(key, pressed) in edges {
            if pressed { repeater.press(key, now) } else { repeater.release(key) }
        }
        repeater.drain().collect()
    }

    const MENU: Key = Key::Button(Button::Menu);
    const START: Key = Key::Button(Button::Start);
    const VOL_UP: Key = Key::Other(115);

    #[test]
    fn menu_alone_acts_on_release() {
        assert_eq!(run(&[(MENU, true)]), []);
        assert_eq!(run(&[(MENU, true), (MENU, false)]), [Button::Menu]);
    }

    #[test]
    fn chords_never_open_the_menu() {
        assert_eq!(run(&[(MENU, true), (VOL_UP, true), (VOL_UP, false), (MENU, false)]), []);
        assert_eq!(run(&[(MENU, true), (START, true), (START, false), (MENU, false)]), []);
        assert_eq!(run(&[(VOL_UP, true), (MENU, true), (MENU, false)]), []);
    }

    #[test]
    fn other_buttons_still_act_on_press() {
        let a = Key::Button(Button::A);
        assert_eq!(run(&[(a, true)]), [Button::A]);
        assert_eq!(run(&[(MENU, true), (a, true), (a, false), (MENU, false)]), [Button::A]);
    }
}
