//! The text-entry side of script input: a "typing span" a script starts
//! with `typing_begin()`, during which the app turns keystrokes into an
//! edited line (or lines) of text instead of passing them on as controls.
//! The script reads the text and cursor each frame (`typing_state()`) and
//! draws them itself.
//!
//! Editing follows the usual text-box keys: typing and pasting insert at
//! the cursor, Backspace/Delete remove (a word at a time with Ctrl), the
//! arrows, Home and End move (Ctrl+Left/Right by word), Enter finishes, and
//! Shift+Enter starts a new line when the span allows several. Losing focus
//! (Escape, clicking elsewhere) cancels the span, so a script can never trap
//! the keyboard.

use eframe::egui;

/// One editing event from a frame's input, in the order it happened.
#[derive(Clone, Debug, PartialEq)]
pub enum TextEvent {
    /// Typed characters (after Shift and the keyboard layout; IME commits too).
    Text(String),
    Paste(String),
    /// A key press, including key repeat, with the modifiers held.
    Key(egui::Key, egui::Modifiers),
}

#[derive(Clone, Debug, Default)]
pub struct TypingSpan {
    text: Vec<char>,
    /// Characters before the cursor.
    cursor: usize,
    pub active: bool,
    /// Finished with Enter this frame or earlier (until the next begin).
    pub done: bool,
    /// Cancelled by losing focus (until the next begin).
    pub cancelled: bool,
    max_length: Option<usize>,
    multiline: bool,
    /// The last key pressed this frame, by name ("backspace", "left", ...).
    pub last_key: Option<String>,
}

impl TypingSpan {
    pub fn begin(&mut self, text: &str, max_length: Option<usize>, multiline: bool) {
        let mut chars: Vec<char> = text.chars().filter(|&c| multiline || c != '\n').collect();
        if let Some(max) = max_length {
            chars.truncate(max);
        }
        self.cursor = chars.len();
        self.text = chars;
        self.active = true;
        self.done = false;
        self.cancelled = false;
        self.max_length = max_length;
        self.multiline = multiline;
        self.last_key = None;
    }

    /// Stop the span (the script's choice: neither done nor cancelled).
    pub fn end(&mut self) {
        self.active = false;
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The cursor's line (from 1) and column (characters before it on that
    /// line, from 0).
    pub fn line_and_column(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.iter().filter(|&&c| c == '\n').count() + 1;
        let column = before.iter().rev().take_while(|&&c| c != '\n').count();
        (line, column)
    }

    /// Apply one frame's editing. `focused`: whether the visualizer still
    /// has keyboard focus; losing it cancels the span.
    pub fn update(&mut self, events: &[TextEvent], focused: bool) {
        self.last_key = None;
        if !self.active {
            return;
        }
        if !focused {
            self.active = false;
            self.cancelled = true;
            return;
        }
        for event in events {
            if !self.active {
                break; // Enter finished it; later keys aren't part of it
            }
            match event {
                TextEvent::Text(text) => self.insert(text),
                TextEvent::Paste(text) => {
                    let text = text.replace("\r\n", "\n");
                    let text = if self.multiline { text } else { text.replace('\n', " ") };
                    self.insert(&text);
                }
                TextEvent::Key(key, modifiers) => self.key(*key, *modifiers),
            }
        }
    }

    fn insert(&mut self, text: &str) {
        for c in text.chars().filter(|&c| !c.is_control() || (c == '\n' && self.multiline)) {
            if self.max_length.is_some_and(|max| self.text.len() >= max) {
                break;
            }
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        use egui::Key;
        self.last_key = Some(key.name().to_ascii_lowercase());
        let word = modifiers.command;
        match key {
            Key::Enter if modifiers.shift && self.multiline => self.insert("\n"),
            Key::Enter => {
                self.done = true;
                self.active = false;
            }
            Key::Backspace => {
                let start = if word { self.word_left() } else { self.cursor.saturating_sub(1) };
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            Key::Delete => {
                let end = if word { self.word_right() } else { (self.cursor + 1).min(self.text.len()) };
                self.text.drain(self.cursor..end);
            }
            Key::ArrowLeft => self.cursor = if word { self.word_left() } else { self.cursor.saturating_sub(1) },
            Key::ArrowRight => {
                self.cursor = if word { self.word_right() } else { (self.cursor + 1).min(self.text.len()) };
            }
            Key::Home => self.cursor = self.line_start(self.cursor),
            Key::End => self.cursor = self.line_end(self.cursor),
            Key::ArrowUp if self.multiline => self.move_lines(-1),
            Key::ArrowDown if self.multiline => self.move_lines(1),
            _ => {}
        }
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..].iter().position(|&c| c == '\n').map_or(self.text.len(), |i| at + i)
    }

    /// Move the cursor `delta` lines up or down, keeping its column where
    /// the line is long enough.
    fn move_lines(&mut self, delta: i32) {
        let column = self.cursor - self.line_start(self.cursor);
        let target_start = if delta < 0 {
            let start = self.line_start(self.cursor);
            if start == 0 {
                return;
            }
            self.line_start(start - 1)
        } else {
            let end = self.line_end(self.cursor);
            if end == self.text.len() {
                return;
            }
            end + 1
        };
        let target_end = self.line_end(target_start);
        self.cursor = (target_start + column).min(target_end);
    }

    /// Where Ctrl+Left lands: the start of the word before the cursor.
    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && !is_word(self.text[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(self.text[i - 1]) {
            i -= 1;
        }
        i
    }

    /// Where Ctrl+Right lands: the end of the word after the cursor.
    fn word_right(&self) -> usize {
        let mut i = self.cursor;
        while i < self.text.len() && !is_word(self.text[i]) {
            i += 1;
        }
        while i < self.text.len() && is_word(self.text[i]) {
            i += 1;
        }
        i
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use egui::{Key, Modifiers};

    use super::*;

    fn key(k: Key) -> TextEvent {
        TextEvent::Key(k, Modifiers::NONE)
    }

    fn ctrl(k: Key) -> TextEvent {
        TextEvent::Key(k, Modifiers::COMMAND)
    }

    fn typed(s: &str) -> TextEvent {
        TextEvent::Text(s.to_string())
    }

    #[test]
    fn types_moves_and_deletes_like_a_text_box() {
        let mut span = TypingSpan::default();
        span.begin("", None, false);
        span.update(&[typed("hello world"), key(Key::ArrowLeft), key(Key::ArrowLeft), key(Key::Backspace)], true);
        assert_eq!((span.text().as_str(), span.cursor()), ("hello wold", 8));
        span.update(&[ctrl(Key::ArrowLeft), typed("big "), key(Key::End), ctrl(Key::Backspace)], true);
        assert_eq!(span.text(), "hello big ");
        span.update(&[key(Key::Home), key(Key::Delete), ctrl(Key::Delete)], true);
        assert_eq!((span.text().as_str(), span.cursor()), (" big ", 0));
        assert_eq!(span.last_key.as_deref(), Some("delete"));
        assert!(span.active && !span.done);
    }

    #[test]
    fn enter_finishes_and_later_keys_are_ignored() {
        let mut span = TypingSpan::default();
        span.begin("ls", None, false);
        span.update(&[typed(" -a"), key(Key::Enter), typed("ignored")], true);
        assert!(!span.active && span.done && !span.cancelled);
        assert_eq!(span.text(), "ls -a");
        // Done stays readable until the next begin.
        span.update(&[typed("x")], true);
        assert_eq!(span.text(), "ls -a");
        assert!(span.done);
    }

    #[test]
    fn multiline_lines_and_columns() {
        let mut span = TypingSpan::default();
        span.begin("", None, true);
        span.update(&[typed("abc"), TextEvent::Key(Key::Enter, Modifiers::SHIFT), typed("de")], true);
        assert_eq!(span.text(), "abc\nde");
        assert_eq!(span.line_and_column(), (2, 2));
        span.update(&[key(Key::ArrowUp)], true);
        assert_eq!((span.cursor(), span.line_and_column()), (2, (1, 2)));
        span.update(&[key(Key::End), key(Key::ArrowDown)], true);
        assert_eq!(span.line_and_column(), (2, 2)); // column clamped to the shorter line
        // Single-line spans turn pasted newlines into spaces.
        let mut single = TypingSpan::default();
        single.begin("", None, false);
        single.update(&[TextEvent::Paste("a\r\nb".into()), TextEvent::Key(Key::Enter, Modifiers::SHIFT)], true);
        assert_eq!(single.text(), "a b");
        assert!(single.done, "Shift+Enter finishes a single-line span");
    }

    #[test]
    fn max_length_and_losing_focus() {
        let mut span = TypingSpan::default();
        span.begin("abcdef", Some(4), false);
        assert_eq!(span.text(), "abcd");
        span.update(&[typed("xyz")], true);
        assert_eq!(span.text(), "abcd");
        span.update(&[], false);
        assert!(!span.active && span.cancelled && !span.done);
    }
}
