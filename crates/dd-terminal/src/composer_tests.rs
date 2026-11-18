use super::*;

#[test]
fn prompt_prefix_is_two_cells() {
    // Geometry depends on `❭` being a narrow (single-cell) dingbat.
    assert_eq!(prompt_width(), 2);
}

#[test]
fn wrap_covers_every_char() {
    let input = "hello world foo";
    let ranges = wrap_line_at_whitespace_ranges(input, 6);
    let covered: String = ranges.iter().map(|r| &input[r.clone()]).collect();
    assert_eq!(covered, input);
    assert_eq!(ranges, vec![0..6, 6..12, 12..15]);
}

#[test]
fn visual_lines_wrap_and_break_on_newlines() {
    let lines = editable_input_visual_lines("ab cd\nef", 3);
    assert_eq!(lines, vec!["ab ", "cd", "ef"]);
}

#[test]
fn caret_stays_at_hard_newline_end() {
    let input = "ab\ncd";
    let lines = editable_input_visual_lines(input, 10);
    assert_eq!(visual_caret_position(&lines, input, 2), (0, 2));
    assert_eq!(visual_caret_position(&lines, input, 3), (1, 0));
    assert_eq!(visual_caret_position(&lines, input, 5), (1, 2));
}

#[test]
fn caret_falls_to_next_row_on_soft_wrap() {
    let input = "abcd";
    let lines = editable_input_visual_lines(input, 2);
    // A full last row earns a spare row for its end caret.
    assert_eq!(lines, vec!["ab", "cd", ""]);
    assert_eq!(visual_caret_position(&lines, input, 2), (1, 0));
    assert_eq!(visual_caret_position(&lines, input, 4), (2, 0));
}

#[test]
fn vertical_move_keeps_column() {
    let mut c = Composer::default();
    c.set_text("ab cd ef".to_string());
    c.retain_wrap_width(4);
    // rows: "ab ", "cd ", "ef"; cursor 8 sits at row 2, column 2.
    assert_eq!(c.cursor(), 8);
    assert_eq!(c.apply_edit(EditKey::Up), EditOutcome::TextUnchanged);
    // Column 2 on "cd " lands after 'd'.
    assert_eq!(c.cursor(), 5);
}

#[test]
fn up_on_first_row_is_a_vertical_edge() {
    let mut c = Composer::default();
    c.set_text("hello".to_string());
    c.retain_wrap_width(10);
    assert_eq!(
        c.apply_edit(EditKey::Up),
        EditOutcome::VerticalEdge(Vertical::Up)
    );
}

#[test]
fn only_shift_enter_maps_to_newline() {
    let key = |code, modifiers| KeyEvent::new(code, modifiers);
    assert_eq!(
        EditKey::from_key(key(KeyCode::Enter, KeyModifiers::SHIFT)),
        Some(EditKey::Newline)
    );
    // A bare Enter is the owner's submit, not an edit key.
    assert_eq!(
        EditKey::from_key(key(KeyCode::Enter, KeyModifiers::NONE)),
        None
    );
    // Alt+Enter is the ConPTY encoding of VS Code's Shift+Enter (`ESC CR`).
    assert_eq!(
        EditKey::from_key(key(KeyCode::Enter, KeyModifiers::ALT)),
        Some(EditKey::Newline)
    );
    // Other modified Enters and Ctrl+J are intentionally unmapped.
    assert_eq!(
        EditKey::from_key(key(KeyCode::Enter, KeyModifiers::CONTROL)),
        None
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        None
    );
    // Alt falls through to the plain key.
    assert_eq!(
        EditKey::from_key(key(KeyCode::Right, KeyModifiers::ALT)),
        Some(EditKey::Right)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Backspace, KeyModifiers::ALT)),
        Some(EditKey::Backspace)
    );
}

#[test]
fn arrow_keys_map_to_moves_selects_and_word_jumps() {
    let key = |code, modifiers| KeyEvent::new(code, modifiers);
    assert_eq!(
        EditKey::from_key(key(KeyCode::Left, KeyModifiers::NONE)),
        Some(EditKey::Left)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Left, KeyModifiers::CONTROL)),
        Some(EditKey::WordLeft)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Right, KeyModifiers::SHIFT)),
        Some(EditKey::SelectRight)
    );
    assert_eq!(
        EditKey::from_key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        Some(EditKey::SelectWordRight)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Up, KeyModifiers::SHIFT)),
        Some(EditKey::SelectUp)
    );
    // Ctrl+Alt (AltGr) skips the word jump and falls through to a char move.
    assert_eq!(
        EditKey::from_key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )),
        Some(EditKey::Left)
    );
    // Clipboard chords.
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(EditKey::Copy)
    );
    assert_eq!(
        EditKey::from_key(key(
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        Some(EditKey::Paste)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        Some(EditKey::Cut)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL)),
        Some(EditKey::Undo)
    );
    assert_eq!(
        EditKey::from_key(key(
            KeyCode::Char('Z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        Some(EditKey::Redo)
    );
    // Ctrl+Y is not a redo chord.
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('y'), KeyModifiers::CONTROL)),
        None
    );
    // Russian layout spells the same chords Cyrillic: я on Z, с on C.
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('я'), KeyModifiers::CONTROL)),
        Some(EditKey::Undo)
    );
    assert_eq!(
        EditKey::from_key(key(
            KeyCode::Char('Я'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        Some(EditKey::Redo)
    );
    assert_eq!(
        EditKey::from_key(key(KeyCode::Char('с'), KeyModifiers::CONTROL)),
        Some(EditKey::Copy)
    );
}

#[test]
fn undo_restores_mutations() {
    let mut c = Composer::default();
    c.retain_wrap_width(80);
    c.apply_edit(EditKey::Char('a'));
    c.apply_edit(EditKey::Char('b'));
    assert_eq!(c.text(), "ab");
    assert_eq!(c.apply_edit(EditKey::Undo), EditOutcome::Edited);
    assert_eq!(c.text(), "a");
    assert_eq!(c.cursor(), 1);
    assert_eq!(c.apply_edit(EditKey::Undo), EditOutcome::Edited);
    assert_eq!(c.text(), "");
    // The empty stack reports no change.
    assert_eq!(c.apply_edit(EditKey::Undo), EditOutcome::TextUnchanged);
    // set_text/take are undoable too (history recall, submit, cut).
    c.set_text("draft".to_string());
    assert_eq!(c.apply_edit(EditKey::Undo), EditOutcome::Edited);
    assert_eq!(c.text(), "");
}

#[test]
fn redo_replays_undos_until_a_new_mutation() {
    let mut c = Composer::default();
    c.retain_wrap_width(80);
    c.apply_edit(EditKey::Char('a'));
    c.apply_edit(EditKey::Char('b'));
    c.apply_edit(EditKey::Undo);
    assert_eq!(c.apply_edit(EditKey::Redo), EditOutcome::Edited);
    assert_eq!(c.text(), "ab");
    assert_eq!(c.cursor(), 2);
    // Redo on an empty stack reports no change.
    assert_eq!(c.apply_edit(EditKey::Redo), EditOutcome::TextUnchanged);
    // A fresh mutation opens a new branch and clears redo.
    c.apply_edit(EditKey::Undo);
    c.apply_edit(EditKey::Char('x'));
    assert_eq!(c.apply_edit(EditKey::Redo), EditOutcome::TextUnchanged);
    assert_eq!(c.text(), "ax");
}

#[test]
fn mouse_press_and_drag_selects_text() {
    let mut c = Composer::default();
    c.set_text("hello world".to_string());
    // A bare press anchors an empty selection: nothing is highlighted.
    c.press_at(1);
    assert_eq!(c.selection_range(), None);
    // Dragging extends it from the press point, like Shift+arrows.
    c.drag_to(6);
    assert_eq!(c.selected_text(), Some("ello ".to_string()));
    c.clear_selection();
    // A drag with no anchor selects nothing.
    c.drag_to(3);
    assert_eq!(c.selection_range(), None);
    // And the selection survives Ctrl+X's take_selected the same way.
    c.press_at(0);
    c.drag_to(5);
    assert_eq!(c.take_selected(), Some("hello".to_string()));
    assert_eq!(c.text(), " world");
}

#[test]
fn shift_arrows_select_and_edits_replace() {
    let mut c = Composer::default();
    c.set_text("foo bar".to_string());
    c.retain_wrap_width(80);
    c.apply_edit(EditKey::SelectLeft);
    c.apply_edit(EditKey::SelectLeft);
    c.apply_edit(EditKey::SelectLeft);
    assert_eq!(c.selection_range(), Some(4..7));
    assert_eq!(c.selected_text().as_deref(), Some("bar"));
    // Typing replaces the selection.
    assert_eq!(c.apply_edit(EditKey::Char('x')), EditOutcome::Edited);
    assert_eq!(c.text(), "foo x");
    assert_eq!(c.selection_range(), None);
    // A plain Left with a selection collapses to its start.
    c.set_text("hello".to_string());
    c.apply_edit(EditKey::SelectLeft);
    c.apply_edit(EditKey::SelectLeft);
    c.apply_edit(EditKey::Left);
    assert_eq!(c.cursor(), 3);
    assert_eq!(c.selection_range(), None);
}

#[test]
fn word_jumps_and_selection() {
    let mut c = Composer::default();
    c.set_text("foo bar  baz".to_string());
    c.retain_wrap_width(80);
    c.apply_edit(EditKey::WordLeft);
    assert_eq!(c.cursor(), 9);
    c.apply_edit(EditKey::WordLeft);
    assert_eq!(c.cursor(), 4);
    c.apply_edit(EditKey::SelectWordRight);
    assert_eq!(c.selection_range(), Some(4..7));
    assert_eq!(c.selected_text().as_deref(), Some("bar"));
    // Backspace removes the selection rather than the char before it.
    assert_eq!(c.apply_edit(EditKey::Backspace), EditOutcome::Edited);
    assert_eq!(c.text(), "foo   baz");
    assert_eq!(c.cursor(), 4);
}

#[test]
fn text_inserted_after_a_bare_click_is_not_selected() {
    // Paste after a click: the owner's delete_selection + insert_text.
    let mut c = Composer::default();
    c.set_text("ab".to_string());
    c.press_at(1);
    assert!(!c.delete_selection());
    c.insert_text("XY\nZ");
    assert_eq!(c.text(), "aXY\nZb");
    assert_eq!(c.selection_range(), None);

    // Typing after a click.
    c.press_at(0);
    c.apply_edit(EditKey::Char('q'));
    assert_eq!(c.text(), "qaXY\nZb");
    assert_eq!(c.selection_range(), None);

    // Backspace after a click deletes one char, not a stale range.
    c.press_at(3);
    c.apply_edit(EditKey::Backspace);
    assert_eq!(c.text(), "qaY\nZb");
    assert_eq!(c.selection_range(), None);
}
