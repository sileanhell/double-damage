//! Paste that arrives as keystrokes instead of `Event::Paste`.
//!
//! On Windows crossterm reads console key records, and ConPTY (VS Code,
//! Windows Terminal) drops the bracketed-paste markers the host wraps a paste
//! in: the pasted text reaches us as one key press per character, every line
//! break a plain Enter. Handled one by one, each Enter would submit its line
//! and every character would cost a full redraw.
//!
//! A human types one key at a time, but a paste lands in the input queue many
//! keys at once — and VS Code feeds a long paste to ConPTY in chunks with
//! short pauses between them. So the owner treats a batch of already-queued
//! events holding two or more text presses as the start of a paste burst,
//! keeps collecting chunks while they arrive within [`PASTE_CHUNK_GAP`], and
//! inserts the folded text once.

use std::time::Duration;

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// Longest pause between two chunks of one paste burst. Only waited for once
/// a burst has started, so ordinary typing never pays it.
pub(crate) const PASTE_CHUNK_GAP: Duration = Duration::from_millis(50);

/// A lone text key read this soon after the previous one is the tail of a
/// paste that ConPTY split into separate reads: even key auto-repeat
/// (about 30 ms) is slower, so a typed Enter still submits.
pub(crate) const PASTE_FOLLOW_GAP: Duration = Duration::from_millis(20);

/// The text prefix of `events` folded for a paste: the char each text press
/// types, Enter as `\n` and Tab as `\t`, key releases skipped.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Folded {
    pub(crate) text: String,
    /// Events consumed: everything before the first non-text event.
    pub(crate) consumed: usize,
    /// Key presses among them; two or more in one batch start a burst.
    pub(crate) presses: usize,
}

/// Fold the longest prefix of `events` that a paste can produce.
pub(crate) fn fold_text_prefix(events: &[Event]) -> Folded {
    let mut folded = Folded::default();
    for event in events {
        match paste_key(event) {
            Some(PasteKey::Release) => {}
            Some(PasteKey::Text(ch)) => {
                folded.text.push(ch);
                folded.presses += 1;
            }
            None => break,
        }
        folded.consumed += 1;
    }
    folded
}

enum PasteKey {
    Text(char),
    Release,
}

/// What `event` contributes to a keystroke paste; `None` for anything a paste
/// cannot produce (Ctrl or Alt chords, arrows, mouse), which ends the burst.
fn paste_key(event: &Event) -> Option<PasteKey> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind == KeyEventKind::Release {
        return Some(PasteKey::Release);
    }
    let ch = match key.code {
        // ConPTY synthesizes each pasted char with the modifiers the current
        // layout needs to type it: Shift, or Ctrl+Alt for AltGr chars.
        KeyCode::Char(ch) if is_text_modifiers(key.modifiers) => ch,
        KeyCode::Enter if key.modifiers.is_empty() => '\n',
        KeyCode::Tab if key.modifiers.is_empty() => '\t',
        _ => return None,
    };
    Some(PasteKey::Text(ch))
}

fn is_text_modifiers(modifiers: KeyModifiers) -> bool {
    let without_shift = modifiers - KeyModifiers::SHIFT;
    without_shift.is_empty() || without_shift == KeyModifiers::CONTROL | KeyModifiers::ALT
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn release(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))
    }

    /// Key records as ConPTY delivers them: a press and a release per char.
    fn typed(text: &str) -> Vec<Event> {
        text.chars()
            .flat_map(|ch| {
                let code = match ch {
                    '\r' => KeyCode::Enter,
                    '\t' => KeyCode::Tab,
                    ch => KeyCode::Char(ch),
                };
                let modifiers = if ch.is_uppercase() {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                };
                [press(code, modifiers), release(code)]
            })
            .collect()
    }

    #[test]
    fn multiline_text_folds_whole() {
        let events = typed("Первая\rвторая\r\tTail");
        let folded = fold_text_prefix(&events);
        assert_eq!(folded.text, "Первая\nвторая\n\tTail");
        assert_eq!(folded.consumed, events.len());
        assert_eq!(folded.presses, 19);
    }

    #[test]
    fn a_lone_key_is_one_press() {
        assert_eq!(fold_text_prefix(&typed("\r")).presses, 1);
        assert_eq!(fold_text_prefix(&typed("a")).presses, 1);
    }

    #[test]
    fn altgr_chars_are_text() {
        let events = [press(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        )];
        assert_eq!(fold_text_prefix(&events).text, "@");
    }

    #[test]
    fn a_chord_ends_the_prefix() {
        let mut events = typed("ab");
        events.push(press(KeyCode::Char('z'), KeyModifiers::CONTROL));
        events.extend(typed("c"));
        let folded = fold_text_prefix(&events);
        assert_eq!(folded.text, "ab");
        assert_eq!(folded.consumed, 4);

        // Shift+Enter arrives as Alt+Enter through ConPTY: never paste text.
        let events = [press(KeyCode::Enter, KeyModifiers::ALT)];
        assert_eq!(fold_text_prefix(&events), Folded::default());
    }
}
