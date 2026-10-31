//! An on-screen keyboard driven by a gamepad, for anything the menu needs typed
//! (Wi-Fi names and passwords; pairing codes later). This is the model only:
//! `ui/keyboard.slint` draws it from `Keyboard::keys`.
//!
//! D-pad moves, A types the highlighted key, B deletes, X cycles
//! lower case, upper case and symbols, Y shows or hides masked text, Start
//! confirms and Menu cancels. The bottom row also has Cancel and OK keys.

use super::input::Button;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Lower,
    Upper,
    Symbols,
}

impl Layer {
    fn next(self) -> Self {
        match self { Self::Lower => Self::Upper, Self::Upper => Self::Symbols, Self::Symbols => Self::Lower }
    }

    /// The key that switches away from this layer is named after the next one.
    fn switch_label(self) -> &'static str {
        match self { Self::Lower => "ABC", Self::Upper => "#+=", Self::Symbols => "abc" }
    }

    fn rows(self) -> [&'static str; 4] {
        match self {
            Self::Lower => ["1234567890", "qwertyuiop", "asdfghjkl@", "zxcvbnm.-_"],
            Self::Upper => ["1234567890", "QWERTYUIOP", "ASDFGHJKL@", "ZXCVBNM.-_"],
            // Every printable ASCII symbol, with the space on the bottom row.
            Self::Symbols => ["!@#$%^&*()", "-_=+[]{}\\|", ";:'\",.<>/?", "`~"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Char(char),
    Layer,
    Space,
    Backspace,
    Cancel,
    Done,
}

impl Key {
    /// Width in half-keys: every row is 20 wide.
    fn width(self) -> usize {
        match self {
            Self::Char(_) => 2,
            Self::Layer | Self::Done => 4,
            Self::Space => 6,
            Self::Backspace | Self::Cancel => 3,
        }
    }

    fn label(self, layer: Layer) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::Layer => layer.switch_label().into(),
            Self::Space => "space".into(),
            Self::Backspace => "⌫".into(),
            Self::Cancel => "Cancel".into(),
            Self::Done => "OK".into(),
        }
    }
}

const ACTION_ROW: [Key; 5] = [Key::Layer, Key::Space, Key::Backspace, Key::Cancel, Key::Done];
const ROWS: usize = 5;

fn keys(layer: Layer, row: usize) -> Vec<Key> {
    match layer.rows().get(row) {
        Some(characters) => characters.chars().map(Key::Char).collect(),
        None => ACTION_ROW.to_vec(),
    }
}

/// A key's place on screen, for drawing. Columns and widths count half-keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyView {
    pub label: String,
    pub row: usize,
    pub start: usize,
    pub width: usize,
    /// Not a character: drawn apart from the letters.
    pub action: bool,
}

/// The line under the title: what to type, or why the last attempt was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    Help(String),
    Problem(String),
}

/// Everything the UI needs to draw a keyboard and its text field.
#[derive(Clone, Debug)]
pub struct Display {
    pub title: String,
    pub note: Note,
    pub shown: String,
    pub keys: Vec<KeyView>,
    pub selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Position {
    row: usize,
    index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Submit(String),
    Cancel,
}

/// How typed text is shown. Passwords start masked and Y reveals them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echo {
    Plain,
    Masked,
    Revealed,
}

impl Echo {
    fn toggled(self) -> Self {
        match self { Self::Plain => Self::Plain, Self::Masked => Self::Revealed, Self::Revealed => Self::Masked }
    }
}

#[derive(Clone, Debug)]
pub struct Keyboard {
    layer: Layer,
    position: Position,
    text: String,
    /// Characters, not bytes: the keyboard only types ASCII.
    limit: usize,
    echo: Echo,
}

impl Keyboard {
    /// The cursor starts on the `q`, where typing most often starts.
    pub fn new(limit: usize, echo: Echo) -> Self {
        Self { layer: Layer::Lower, position: Position { row: 1, index: 0 }, text: String::new(), limit, echo }
    }

    #[cfg(test)]
    pub fn text(&self) -> &str { &self.text }

    /// Every key in drawing order (row by row); `selected` indexes this list.
    pub fn keys(&self) -> Vec<KeyView> {
        (0..ROWS).flat_map(|row| {
            let mut start = 0;
            keys(self.layer, row).into_iter().map(move |key| {
                let view = KeyView { label: key.label(self.layer), row, start, width: key.width(), action: !matches!(key, Key::Char(_)) };
                start += key.width();
                view
            }).collect::<Vec<_>>()
        }).collect()
    }

    pub fn selected(&self) -> usize {
        (0..self.position.row).map(|row| keys(self.layer, row).len()).sum::<usize>() + self.position.index
    }

    /// What to draw: `columns` is how many characters of text fit in the field.
    pub fn display(&self, title: String, note: Note, columns: usize) -> Display {
        Display { title, note, shown: self.shown(columns), keys: self.keys(), selected: self.selected() }
    }

    /// The text as drawn, ending in a caret. Long text shows its tail.
    pub fn shown(&self, columns: usize) -> String {
        let characters: Vec<char> = match self.echo {
            Echo::Plain | Echo::Revealed => self.text.chars().collect(),
            Echo::Masked => self.text.chars().map(|_| '•').collect(),
        };
        let room = columns.saturating_sub(1).max(1);
        let tail: String = if characters.len() > room { std::iter::once('…').chain(characters[characters.len() - room + 1..].iter().copied()).collect() } else { characters.iter().collect() };
        tail + "▏"
    }

    pub fn press(&mut self, button: Button) -> Option<Outcome> {
        match button {
            Button::Up => self.move_vertically(-1),
            Button::Down => self.move_vertically(1),
            Button::Left => self.move_horizontally(-1),
            Button::Right => self.move_horizontally(1),
            Button::A => return self.activate(self.current()),
            Button::B => { self.text.pop(); }
            Button::X => self.switch_layer(),
            Button::Y => self.echo = self.echo.toggled(),
            Button::Start => return Some(Outcome::Submit(self.text.clone())),
            Button::Menu => return Some(Outcome::Cancel),
            Button::L1 | Button::R1 | Button::L2 | Button::R2 | Button::Select => {}
        }
        None
    }

    fn current(&self) -> Key {
        keys(self.layer, self.position.row)[self.position.index]
    }

    fn activate(&mut self, key: Key) -> Option<Outcome> {
        match key {
            Key::Char(c) => self.type_char(c),
            Key::Space => self.type_char(' '),
            Key::Backspace => { self.text.pop(); }
            Key::Layer => self.switch_layer(),
            Key::Cancel => return Some(Outcome::Cancel),
            Key::Done => return Some(Outcome::Submit(self.text.clone())),
        }
        None
    }

    fn type_char(&mut self, c: char) {
        if self.text.chars().count() < self.limit {
            self.text.push(c);
            // Shift applies to one character, as on a phone; symbols stay until X.
            if self.layer == Layer::Upper { self.layer = Layer::Lower; }
        }
    }

    fn switch_layer(&mut self) {
        self.layer = self.layer.next();
        // The symbol layer's last row is short.
        self.position.index = self.position.index.min(keys(self.layer, self.position.row).len() - 1);
    }

    fn move_horizontally(&mut self, delta: isize) {
        let length = keys(self.layer, self.position.row).len() as isize;
        self.position.index = (self.position.index as isize + delta).rem_euclid(length) as usize;
    }

    /// Keeps the column the cursor is over, even when rows have differently
    /// sized keys, by matching the middle of the current key.
    fn move_vertically(&mut self, delta: isize) {
        let middle = keys(self.layer, self.position.row).iter().take(self.position.index + 1).map(|key| key.width()).sum::<usize>() - self.current().width() / 2;
        let row = (self.position.row as isize + delta).rem_euclid(ROWS as isize) as usize;
        let target = keys(self.layer, row);
        let mut end = 0;
        let index = target.iter().position(|key| { end += key.width(); middle < end }).unwrap_or(target.len() - 1);
        self.position = Position { row, index };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Keyboard {
        fn layer(&self) -> Layer { self.layer }
    }

    fn type_keys(keyboard: &mut Keyboard, buttons: &[Button]) -> Vec<Outcome> {
        buttons.iter().filter_map(|button| keyboard.press(*button)).collect()
    }

    fn label(keyboard: &Keyboard) -> String { keyboard.keys()[keyboard.selected()].label.clone() }

    #[test]
    fn the_cursor_starts_on_q_and_a_types_it() {
        let mut keyboard = Keyboard::new(63, Echo::Masked);
        assert_eq!(label(&keyboard), "q");
        keyboard.press(Button::A);
        keyboard.press(Button::Right);
        keyboard.press(Button::A);
        assert_eq!(keyboard.text(), "qw");
    }

    #[test]
    fn b_deletes_and_does_nothing_on_empty_text() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        keyboard.press(Button::B);
        keyboard.press(Button::A);
        keyboard.press(Button::A);
        keyboard.press(Button::B);
        assert_eq!(keyboard.text(), "q");
    }

    #[test]
    fn text_stops_at_the_limit() {
        let mut keyboard = Keyboard::new(3, Echo::Plain);
        for _ in 0..5 { keyboard.press(Button::A); }
        assert_eq!(keyboard.text(), "qqq");
    }

    #[test]
    fn x_cycles_the_layers_and_shift_applies_to_one_character() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        keyboard.press(Button::X);
        assert_eq!((keyboard.layer(), label(&keyboard)), (Layer::Upper, "Q".into()));
        keyboard.press(Button::A);
        assert_eq!(keyboard.layer(), Layer::Lower);
        keyboard.press(Button::A);
        assert_eq!(keyboard.text(), "Qq");
        keyboard.press(Button::X);
        keyboard.press(Button::X);
        assert_eq!(keyboard.layer(), Layer::Symbols);
        keyboard.press(Button::A);
        keyboard.press(Button::A);
        assert_eq!(keyboard.layer(), Layer::Symbols);
        keyboard.press(Button::X);
        assert_eq!(keyboard.layer(), Layer::Lower);
    }

    #[test]
    fn every_printable_ascii_character_can_be_typed() {
        let reachable: std::collections::BTreeSet<char> = [Layer::Lower, Layer::Upper, Layer::Symbols].iter().flat_map(|layer| layer.rows().concat().chars().collect::<Vec<_>>()).chain([' ']).collect();
        let missing: String = (' '..='~').filter(|c| !reachable.contains(c)).collect();
        assert_eq!(missing, "");
    }

    #[test]
    fn rows_are_all_the_same_width() {
        for layer in [Layer::Lower, Layer::Upper] {
            for row in 0..ROWS {
                assert_eq!(keys(layer, row).iter().map(|key| key.width()).sum::<usize>(), 20, "{layer:?} row {row}");
            }
        }
        assert_eq!(keys(Layer::Symbols, 4).iter().map(|key| key.width()).sum::<usize>(), 20);
    }

    #[test]
    fn horizontal_movement_wraps_within_a_row() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        keyboard.press(Button::Left);
        assert_eq!(label(&keyboard), "p");
        keyboard.press(Button::Right);
        assert_eq!(label(&keyboard), "q");
    }

    #[test]
    fn vertical_movement_keeps_the_column_and_wraps() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        type_keys(&mut keyboard, &[Button::Up]);
        assert_eq!(label(&keyboard), "1");
        type_keys(&mut keyboard, &[Button::Up]);
        // Wrapped to the action row; the first key is the layer switch.
        assert_eq!(label(&keyboard), "ABC");
        // The wide layer key's middle is on the border of z and x; x wins.
        type_keys(&mut keyboard, &[Button::Up]);
        assert_eq!(label(&keyboard), "x");
        // From the middle of the letters down to the wide space key.
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        type_keys(&mut keyboard, &[Button::Down, Button::Down, Button::Right, Button::Right, Button::Right, Button::Down]);
        assert_eq!(label(&keyboard), "space");
    }

    #[test]
    fn the_short_symbol_row_clamps_the_cursor() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        // Row 2 has ten symbols; the key below the seventh is past the end of row 3.
        type_keys(&mut keyboard, &[Button::X, Button::X, Button::Down, Button::Right, Button::Right, Button::Right, Button::Right, Button::Right, Button::Right, Button::Down]);
        assert_eq!(label(&keyboard), "~");
        keyboard.press(Button::A);
        assert_eq!(keyboard.text(), "~");
    }

    #[test]
    fn switching_layers_clamps_a_cursor_past_the_short_row() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        type_keys(&mut keyboard, &[Button::Down, Button::Down, Button::Right, Button::Right, Button::Right, Button::Right]);
        assert_eq!(label(&keyboard), "b");
        type_keys(&mut keyboard, &[Button::X, Button::X]);
        assert_eq!(label(&keyboard), "~");
    }

    #[test]
    fn the_action_keys_work() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        type_keys(&mut keyboard, &[Button::Up, Button::Up]);
        // On the layer key: A switches layers.
        keyboard.press(Button::A);
        assert_eq!(keyboard.layer(), Layer::Upper);
        // Space, then backspace.
        type_keys(&mut keyboard, &[Button::Right, Button::A]);
        assert_eq!(keyboard.text(), " ");
        type_keys(&mut keyboard, &[Button::Right, Button::A]);
        assert_eq!(keyboard.text(), "");
        type_keys(&mut keyboard, &[Button::Left, Button::A]);
        assert_eq!(keyboard.text(), " ");
        // Cancel and OK.
        type_keys(&mut keyboard, &[Button::Right, Button::Right]);
        assert_eq!(keyboard.press(Button::A), Some(Outcome::Cancel));
        keyboard.press(Button::Right);
        assert_eq!(keyboard.press(Button::A), Some(Outcome::Submit(" ".into())));
    }

    #[test]
    fn start_submits_and_menu_cancels() {
        let mut keyboard = Keyboard::new(63, Echo::Masked);
        keyboard.press(Button::A);
        assert_eq!(keyboard.press(Button::Start), Some(Outcome::Submit("q".into())));
        assert_eq!(keyboard.press(Button::Menu), Some(Outcome::Cancel));
        assert_eq!(keyboard.press(Button::L1), None);
    }

    #[test]
    fn masked_text_is_hidden_until_y_reveals_it() {
        let mut keyboard = Keyboard::new(63, Echo::Masked);
        type_keys(&mut keyboard, &[Button::A, Button::A]);
        assert_eq!(keyboard.shown(40), "••▏");
        keyboard.press(Button::Y);
        assert_eq!(keyboard.shown(40), "qq▏");
        // Plain text has nothing to reveal.
        let mut plain = Keyboard::new(63, Echo::Plain);
        plain.press(Button::A);
        plain.press(Button::Y);
        assert_eq!(plain.shown(40), "q▏");
    }

    #[test]
    fn long_text_shows_its_tail_within_the_width() {
        let mut keyboard = Keyboard::new(63, Echo::Plain);
        for _ in 0..30 { keyboard.press(Button::A); }
        let shown = keyboard.shown(10);
        assert_eq!(shown.chars().count(), 10);
        assert!(shown.starts_with('…') && shown.ends_with("q▏"));
    }

    #[test]
    fn the_key_list_places_every_key_once() {
        let keyboard = Keyboard::new(63, Echo::Plain);
        let keys = keyboard.keys();
        assert_eq!(keys.len(), 10 * 4 + 5);
        assert_eq!(keys.iter().filter(|key| key.action).count(), 5);
        assert!(keys.iter().all(|key| key.start + key.width <= 20));
        assert_eq!(keys[keyboard.selected()].label, "q");
    }
}
