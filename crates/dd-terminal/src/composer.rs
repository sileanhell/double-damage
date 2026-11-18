//! Input field modeled on the rho composer: a `❭ ` prompt, whitespace
//! soft-wrap, a grapheme-aware caret, and the shared edit-key table.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Prompt prefix painted before the first composer row.
pub(crate) const PROMPT_PREFIX: &str = "❭ ";

/// Width of [`PROMPT_PREFIX`]; continuation rows indent by the same amount.
pub(crate) fn prompt_width() -> usize {
    display_width(PROMPT_PREFIX)
}

/// Terminal cell width, ignoring control characters like rho's renderer.
pub(crate) fn display_width(text: &str) -> usize {
    text.split(char::is_control)
        .map(UnicodeWidthStr::width)
        .sum()
}

/// Vertical direction of a caret move or a history recall at a text edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vertical {
    Up,
    Down,
}

/// Cursor and text editing keys, a subset of rho's `ComposerEditKey`.
///
/// The owner checks its own keys (submit, quit, history, clipboard) first,
/// then maps the rest through [`EditKey::from_key`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditKey {
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    SelectLeft,
    SelectRight,
    SelectWordLeft,
    SelectWordRight,
    SelectUp,
    SelectDown,
    Newline,
    Undo,
    Redo,
    /// Clipboard keys; the owner answers them, the composer never sees them.
    Copy,
    Cut,
    Paste,
    Char(char),
}

impl EditKey {
    /// Backspace, Delete, Up, and Down ignore modifiers; a char inserts unless
    /// Ctrl or Alt is held. Left/Right move the caret, Ctrl adds word steps,
    /// Shift extends the selection. A newline comes from Shift+Enter where the
    /// terminal reports it — classic console records or, off Windows, the
    /// kitty/modifyOtherKeys protocols armed by `keyboard.rs` — and from
    /// Alt+Enter, which is what VS Code's Shift+Enter (`ESC CR`) decodes to
    /// through ConPTY; where Alt+Enter means fullscreen the terminal keeps
    /// the keystroke for itself anyway. Ctrl+C/X/V/Z (Ctrl+Shift+Z redoes) are clipboard/undo keys
    /// the owner answers. Alt falls through to the plain key, so e.g.
    /// Alt+Left is a char move, not a word move.
    pub(crate) fn from_key(key: KeyEvent) -> Option<Self> {
        let edit = match (key.modifiers, key.code) {
            (_, KeyCode::Backspace) => Self::Backspace,
            (_, KeyCode::Delete) => Self::Delete,
            (m, KeyCode::Left) if has_ctrl_shift(m) => Self::SelectWordLeft,
            (m, KeyCode::Right) if has_ctrl_shift(m) => Self::SelectWordRight,
            (m, KeyCode::Left) if m.contains(KeyModifiers::SHIFT) => Self::SelectLeft,
            (m, KeyCode::Right) if m.contains(KeyModifiers::SHIFT) => Self::SelectRight,
            (m, KeyCode::Up) if m.contains(KeyModifiers::SHIFT) => Self::SelectUp,
            (m, KeyCode::Down) if m.contains(KeyModifiers::SHIFT) => Self::SelectDown,
            (m, KeyCode::Left) if has_ctrl_no_alt(m) => Self::WordLeft,
            (m, KeyCode::Right) if has_ctrl_no_alt(m) => Self::WordRight,
            (_, KeyCode::Left) => Self::Left,
            (_, KeyCode::Right) => Self::Right,
            (_, KeyCode::Up) => Self::Up,
            (_, KeyCode::Down) => Self::Down,
            (m, KeyCode::Enter) if m == KeyModifiers::SHIFT || m == KeyModifiers::ALT => {
                Self::Newline
            }
            // Ctrl+letters report the layout's char: ЙЦУКЕН puts с/ч/м/я/н on
            // the same physical keys as c/x/v/z/y, so both spell each chord.
            (m, KeyCode::Char(ch)) if has_ctrl_no_alt(m) => {
                match ch.to_lowercase().next().unwrap_or(ch) {
                    'c' | 'с' => Self::Copy,
                    'x' | 'ч' => Self::Cut,
                    'v' | 'м' => Self::Paste,
                    'z' | 'я' if m.contains(KeyModifiers::SHIFT) => Self::Redo,
                    'z' | 'я' => Self::Undo,
                    _ => return None,
                }
            }
            (modifiers, KeyCode::Char(ch))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Self::Char(ch)
            }
            _ => return None,
        };
        Some(edit)
    }
}

fn has_ctrl_shift(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL) && modifiers.contains(KeyModifiers::SHIFT)
}

/// Ctrl without Alt, so AltGr chords (Ctrl+Alt) do not look like Ctrl.
fn has_ctrl_no_alt(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
}

/// What [`Composer::apply_edit`] did, so the owner can run its side effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditOutcome {
    /// Text changed.
    Edited,
    /// Text is unchanged; the caret may have moved.
    TextUnchanged,
    /// Up on the first row or Down on the last row. The caret has not moved:
    /// the owner may recall history, else call [`Composer::move_vertically`].
    VerticalEdge(Vertical),
}

/// Undo history depth: one checkpoint per text mutation.
const UNDO_LIMIT: usize = 100;

/// Composer text with a char-indexed caret and an optional selection, like
/// rho's `ComposerBuffer` without collapsed paste markers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Composer {
    text: String,
    /// Caret as a char index into `text`; the selection's moving end.
    cursor: usize,
    /// The selection's fixed end as a char index; `None` when nothing is
    /// selected. Any unshifted caret move or text load clears it.
    selection_anchor: Option<usize>,
    /// Wrapped text width at the last paint; vertical moves follow it.
    /// `None` until the first paint, when rows break only at newlines.
    wrap_width: Option<usize>,
    /// `(text, cursor)` snapshots taken before each mutation, newest last.
    undo: std::collections::VecDeque<(String, usize)>,
    /// States dropped by Ctrl+Z; Ctrl+Shift+Z walks them back. A fresh
    /// mutation clears the stack — it starts a new history branch.
    redo: std::collections::VecDeque<(String, usize)>,
}

impl Composer {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Retain the wrap width actually painted so Up/Down follow it. Only the
    /// paint path calls this, so speculative layouts never move the caret.
    pub(crate) fn retain_wrap_width(&mut self, wrap_width: usize) {
        self.wrap_width = Some(wrap_width.max(1));
    }

    fn wrap_width(&self) -> usize {
        self.wrap_width.unwrap_or(usize::MAX)
    }

    /// Load a whole draft (history entry or saved live text), caret at its end.
    pub(crate) fn set_text(&mut self, text: String) {
        self.checkpoint();
        self.cursor = text.chars().count();
        self.selection_anchor = None;
        self.text = text;
    }

    /// Replace the whole text as one undo step with the caret at char
    /// `cursor` (clamped); used by command completion.
    pub(crate) fn set_text_and_cursor(&mut self, text: String, cursor: usize) {
        self.checkpoint();
        self.cursor = cursor.min(text.chars().count());
        self.selection_anchor = None;
        self.text = text;
    }

    /// Press at `cursor` (clamped): the caret lands there and anchors a
    /// potential selection, so a following [`Self::drag_to`] extends it like
    /// Shift+arrows do. A bare press leaves an empty, invisible selection.
    pub(crate) fn press_at(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.char_len());
        self.selection_anchor = Some(self.cursor);
    }

    /// Drag the caret to `cursor` (clamped), extending the press's anchor —
    /// the mouse path to Shift+arrows selecting.
    pub(crate) fn drag_to(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.char_len());
    }

    /// Drop the selection highlight without touching the text; used when the
    /// pointer presses outside the field.
    pub(crate) fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    /// Take the text and leave the field empty.
    pub(crate) fn take(&mut self) -> String {
        self.checkpoint();
        self.cursor = 0;
        self.selection_anchor = None;
        std::mem::take(&mut self.text)
    }

    /// Selected range as char indices, ordered low to high; `None` when the
    /// selection is empty or absent.
    pub(crate) fn selection_range(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// Copy of the selected text.
    pub(crate) fn selected_text(&self) -> Option<String> {
        self.selection_range()
            .map(|range| self.byte_slice(range).to_string())
    }

    /// Take the selected text out of the field; the caret lands at its start.
    pub(crate) fn take_selected(&mut self) -> Option<String> {
        let range = self.selection_range()?;
        let text = self.byte_slice(range.clone()).to_string();
        self.selection_anchor = None;
        self.replace_range(range, "");
        Some(text)
    }

    /// Remove the selection; false when there is nothing selected. Call before
    /// inserting so typed or pasted text replaces the selection.
    ///
    /// The anchor always drops, even when the selection is empty: a bare
    /// click leaves an anchor at the caret, and keeping it would make the
    /// text inserted next look selected.
    pub(crate) fn delete_selection(&mut self) -> bool {
        let range = self.selection_range();
        self.selection_anchor = None;
        let Some(range) = range else {
            return false;
        };
        self.replace_range(range, "");
        true
    }

    /// Apply an edit key. Vertical moves follow the painted wrap width.
    /// Clipboard keys (`Copy`/`Cut`/`Paste`) are the owner's job and arrive
    /// here only as no-ops.
    pub(crate) fn apply_edit(&mut self, edit: EditKey) -> EditOutcome {
        let edited = match edit {
            EditKey::Backspace => self.delete_selection() || self.backspace(),
            EditKey::Delete => self.delete_selection() || self.delete(),
            EditKey::Left => {
                self.collapse_or_move(Vertical::Up);
                false
            }
            EditKey::Right => {
                self.collapse_or_move(Vertical::Down);
                false
            }
            EditKey::WordLeft => {
                self.selection_anchor = None;
                self.move_to_previous_word();
                false
            }
            EditKey::WordRight => {
                self.selection_anchor = None;
                self.move_to_next_word();
                false
            }
            EditKey::Up => {
                self.selection_anchor = None;
                return self.vertical_edit(Vertical::Up);
            }
            EditKey::Down => {
                self.selection_anchor = None;
                return self.vertical_edit(Vertical::Down);
            }
            EditKey::SelectLeft => {
                self.begin_selecting();
                self.move_left();
                false
            }
            EditKey::SelectRight => {
                self.begin_selecting();
                self.move_right();
                false
            }
            EditKey::SelectWordLeft => {
                self.begin_selecting();
                self.move_to_previous_word();
                false
            }
            EditKey::SelectWordRight => {
                self.begin_selecting();
                self.move_to_next_word();
                false
            }
            EditKey::SelectUp => {
                self.begin_selecting();
                self.select_vertically(Vertical::Up);
                false
            }
            EditKey::SelectDown => {
                self.begin_selecting();
                self.select_vertically(Vertical::Down);
                false
            }
            EditKey::Newline => {
                self.delete_selection();
                self.insert_text("\n");
                true
            }
            EditKey::Char(ch) => {
                self.delete_selection();
                self.insert_char(ch);
                true
            }
            EditKey::Undo => self.undo(),
            EditKey::Redo => self.redo(),
            EditKey::Copy | EditKey::Cut | EditKey::Paste => false,
        };
        if edited {
            EditOutcome::Edited
        } else {
            EditOutcome::TextUnchanged
        }
    }

    /// Start or keep a selection: the first select-move anchors at the caret,
    /// later ones extend toward the moving caret.
    fn begin_selecting(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
    }

    /// A plain Left/Right collapses the selection to its edge instead of
    /// stepping over it (editor convention); without one it is a char move.
    /// `direction` reuses [`Vertical`]: Up = left/start, Down = right/end.
    fn collapse_or_move(&mut self, direction: Vertical) {
        if let Some(range) = self.selection_range() {
            self.cursor = match direction {
                Vertical::Up => range.start,
                Vertical::Down => range.end,
            };
        } else {
            match direction {
                Vertical::Up => self.move_left(),
                Vertical::Down => self.move_right(),
            }
        }
        self.selection_anchor = None;
    }

    /// Select-move one wrapped row; at the text edges the caret clamps to the
    /// start/end instead of reaching the owner's history edge.
    fn select_vertically(&mut self, direction: Vertical) {
        let visual_lines = editable_input_visual_lines(&self.text, self.wrap_width());
        let (row, _) = visual_caret_position(&visual_lines, &self.text, self.cursor);
        match direction {
            Vertical::Up if row == 0 => self.cursor = 0,
            Vertical::Down if row + 1 >= visual_lines.len() => self.cursor = self.char_len(),
            _ => self.move_vertically(direction),
        }
    }

    fn vertical_edit(&mut self, direction: Vertical) -> EditOutcome {
        let visual_lines = editable_input_visual_lines(&self.text, self.wrap_width());
        let (row, _) = visual_caret_position(&visual_lines, &self.text, self.cursor);
        let at_edge = match direction {
            Vertical::Up => row == 0,
            Vertical::Down => row + 1 >= visual_lines.len(),
        };
        if at_edge {
            return EditOutcome::VerticalEdge(direction);
        }
        self.move_vertically(direction);
        EditOutcome::TextUnchanged
    }

    /// Move one wrapped row, keeping the display column. Past the last row the
    /// caret lands at the end of the text.
    pub(crate) fn move_vertically(&mut self, direction: Vertical) {
        let visual_lines = editable_input_visual_lines(&self.text, self.wrap_width());
        let (row, column) = visual_caret_position(&visual_lines, &self.text, self.cursor);
        let target_row = match direction {
            Vertical::Up => row.saturating_sub(1),
            Vertical::Down => row + 1,
        };
        self.cursor = cursor_index_on_visual_line(&self.text, &visual_lines, target_row, column);
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
    }

    fn move_to_previous_word(&mut self) {
        self.cursor = previous_word_boundary(&self.text, self.cursor);
    }

    fn move_to_next_word(&mut self) {
        self.cursor = next_word_boundary(&self.text, self.cursor);
    }

    /// Push the current `(text, cursor)` onto the undo stack, dropping the
    /// oldest checkpoint past [`UNDO_LIMIT`]. Called before every mutation;
    /// a mutation also clears the redo stack, since it opens a new branch.
    fn checkpoint(&mut self) {
        self.redo.clear();
        if self.undo.len() == UNDO_LIMIT {
            self.undo.pop_front();
        }
        self.undo.push_back((self.text.clone(), self.cursor));
    }

    /// Restore the last undo checkpoint, moving the current state to redo;
    /// false when the undo stack is empty.
    fn undo(&mut self) -> bool {
        let Some((text, cursor)) = self.undo.pop_back() else {
            return false;
        };
        if self.redo.len() == UNDO_LIMIT {
            self.redo.pop_front();
        }
        self.redo
            .push_back((std::mem::replace(&mut self.text, text), self.cursor));
        self.cursor = cursor.min(self.char_len());
        self.selection_anchor = None;
        true
    }

    /// Reapply the last undone state, moving the current state back to undo;
    /// false when the redo stack is empty.
    fn redo(&mut self) -> bool {
        let Some((text, cursor)) = self.redo.pop_back() else {
            return false;
        };
        if self.undo.len() == UNDO_LIMIT {
            self.undo.pop_front();
        }
        self.undo
            .push_back((std::mem::replace(&mut self.text, text), self.cursor));
        self.cursor = cursor.min(self.char_len());
        self.selection_anchor = None;
        true
    }

    fn insert_char(&mut self, ch: char) {
        self.checkpoint();
        let byte_index = self.byte_index(self.cursor);
        self.text.insert(byte_index, ch);
        self.cursor += 1;
    }

    /// Insert `text` at the caret.
    pub(crate) fn insert_text(&mut self, text: &str) {
        self.checkpoint();
        let byte_index = self.byte_index(self.cursor);
        self.text.insert_str(byte_index, text);
        self.cursor += text.chars().count();
    }

    /// Remove the char before the caret; false at the start.
    fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.replace_range(self.cursor - 1..self.cursor, "");
        true
    }

    /// Remove the char under the caret; false at the end.
    fn delete(&mut self) -> bool {
        if self.cursor >= self.char_len() {
            return false;
        }
        self.replace_range(self.cursor..self.cursor + 1, "");
        true
    }

    /// Replace chars in `range` with `inserted`; the caret lands after it.
    fn replace_range(&mut self, range: std::ops::Range<usize>, inserted: &str) {
        self.checkpoint();
        let char_len = self.char_len();
        let start = range.start.min(char_len);
        let end = range.end.max(start).min(char_len);
        let start_byte = self.byte_index(start);
        let end_byte = self.byte_index(end);
        self.text.replace_range(start_byte..end_byte, inserted);
        self.cursor = start + inserted.chars().count();
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(index, _)| index)
    }

    fn byte_slice(&self, char_range: std::ops::Range<usize>) -> &str {
        &self.text[self.byte_index(char_range.start)..self.byte_index(char_range.end)]
    }
}

fn previous_word_boundary(input: &str, cursor: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut index = cursor.min(chars.len());
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

fn next_word_boundary(input: &str, cursor: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut index = cursor.min(chars.len());
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    index
}

/// Wrapped composer rows; every source character stays on some row so caret
/// math can lockstep with `input`. Empty input yields one empty row. A last
/// row exactly filled gets an extra empty row for its end caret.
pub(crate) fn editable_input_visual_lines(input: &str, width: usize) -> Vec<String> {
    editable_input_visual_line_ranges(input, width)
        .into_iter()
        .map(|range| input[range].to_string())
        .collect()
}

/// Byte ranges of `input` backing each row of [`editable_input_visual_lines`],
/// same order and count. Rendering uses them to map selection char offsets
/// onto painted rows.
pub(crate) fn editable_input_visual_line_ranges(
    input: &str,
    width: usize,
) -> Vec<std::ops::Range<usize>> {
    let width = width.max(1);
    let mut ranges = Vec::new();
    let mut base = 0;
    for raw_line in input.split('\n') {
        for range in wrap_line_at_whitespace_ranges(raw_line, width) {
            ranges.push(base + range.start..base + range.end);
        }
        base += raw_line.len() + 1;
    }
    if ranges.is_empty() {
        ranges.push(0..0);
    }
    if !input.is_empty()
        && ranges
            .last()
            .is_some_and(|range| display_width(&input[range.clone()]) >= width)
    {
        ranges.push(input.len()..input.len());
    }
    ranges
}

/// Row and display column of char index `cursor` across `visual_lines`.
///
/// The lines partition `input` with hard newlines between rows they split, so
/// the caret lands on the row that paints the character under the cursor: a
/// cursor on a row end stays there when the break is a hard newline (or end of
/// input) and falls to the next row when the line soft-wrapped.
pub(crate) fn visual_caret_position(
    visual_lines: &[String],
    input: &str,
    cursor: usize,
) -> (usize, usize) {
    let mut chars = input.chars().peekable();
    let mut line_start = 0usize;
    for (row, line) in visual_lines.iter().enumerate() {
        let len = line.chars().count();
        let line_end = line_start + len;
        // Consume this row's source chars so `chars.peek()` is the row break.
        for _ in 0..len {
            chars.next();
        }
        let next_is_newline = chars.peek() == Some(&'\n');
        let last_row = row + 1 == visual_lines.len();
        if cursor < line_end || (cursor == line_end && (last_row || next_is_newline)) {
            let column_byte = line
                .char_indices()
                .nth(cursor - line_start)
                .map_or(line.len(), |(byte, _)| byte);
            return (row, display_width(&line[..column_byte]));
        }
        if next_is_newline {
            chars.next();
            line_start = line_end + 1;
        } else {
            line_start = line_end;
        }
    }
    (visual_lines.len().saturating_sub(1), 0)
}

/// Char index of `target_column` on `target_row`, at grapheme boundaries.
pub(crate) fn cursor_index_on_visual_line(
    input: &str,
    visual_lines: &[String],
    target_row: usize,
    target_column: usize,
) -> usize {
    // Walk `input` once alongside the visual lines so the row break (a hard
    // newline) between them is accounted for.
    let mut chars = input.chars().peekable();
    let mut line_start = 0;
    for line in visual_lines.iter().take(target_row) {
        let consumed = line.chars().count();
        for _ in 0..consumed {
            chars.next();
        }
        line_start += consumed;
        if chars.peek() == Some(&'\n') {
            chars.next();
            line_start += 1;
        }
    }

    let mut cursor = line_start;
    let mut column = 0;
    if let Some(line) = visual_lines.get(target_row) {
        for grapheme in line.graphemes(true) {
            let next_column = column + display_width(grapheme);
            if next_column > target_column {
                break;
            }
            column = next_column;
            cursor += grapheme.chars().count();
        }
    }
    cursor
}

/// Covering soft-wrap ranges: every source byte belongs to exactly one range.
/// Break-boundary whitespace stays at row ends so it remains addressable.
pub(crate) fn wrap_line_at_whitespace_ranges(
    line: &str,
    width: usize,
) -> Vec<std::ops::Range<usize>> {
    let width = width.max(1);
    if line.is_empty() {
        return std::iter::once(0..0).collect();
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let mut count = 0usize;
        let mut last_fitting_split = None;
        let mut whitespace_break = None;
        let mut saw_non_whitespace = false;
        let mut overflow = false;
        let mut prefer_width_split = false;

        for (relative_index, grapheme) in line[start..].grapheme_indices(true) {
            let grapheme_width = display_width(grapheme);
            let is_whitespace = grapheme.chars().all(char::is_whitespace);
            if count > 0 && count + grapheme_width > width {
                overflow = true;
                prefer_width_split = is_whitespace;
                break;
            }
            count += grapheme_width;
            let next = start + relative_index + grapheme.len();
            last_fitting_split = Some(next);
            if is_whitespace {
                if saw_non_whitespace {
                    whitespace_break = Some(next);
                }
            } else {
                saw_non_whitespace = true;
            }
        }

        if !overflow {
            ranges.push(start..line.len());
            break;
        }

        let split = if prefer_width_split {
            last_fitting_split.expect("overflow requires a fitting split")
        } else {
            whitespace_break
                .filter(|split| *split > start)
                .unwrap_or_else(|| last_fitting_split.expect("overflow requires a fitting split"))
        };
        ranges.push(start..split);
        start = split;
    }

    ranges
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
