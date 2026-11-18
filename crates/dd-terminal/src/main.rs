use std::collections::VecDeque;
use std::io;
use std::ops::Range;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::{
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
            KeyModifiers, MouseButton, MouseEventKind,
        },
        execute,
    },
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

mod commands;
mod composer;
mod hub;
mod keyboard;
mod picker;
mod scrollbar;
mod sessions;
mod settings;
mod typed_paste;

use commands::{
    CommandPalette, EFFORT_COMMANDS, EXIT_COMMAND, MODELS_COMMAND, NEW_COMMAND, PaletteKey,
    SESSIONS_COMMAND, SETTINGS_COMMANDS, USAGE_COMMANDS,
};
use composer::{
    Composer, EditKey, EditOutcome, PROMPT_PREFIX, Vertical, cursor_index_on_visual_line,
    display_width, editable_input_visual_line_ranges, editable_input_visual_lines, prompt_width,
    visual_caret_position, wrap_line_at_whitespace_ranges,
};
use dd_providers::{Effort, Message, ModelInfo, ProviderKind};
use hub::{Hub, Load, ProviderEntry, Reply};
use keyboard::KeyboardProtocols;
use picker::{Picker, PickerItem, PickerOutcome, PickerPurpose};
use scrollbar::{Scrollbar, ScrollbarDrag};
use sessions::{ChatMessage, Role, Session};
use settings::{
    COMMAND_HINTS, HISTORY_RECALL, MESSAGE_BACKGROUND, MOUSE_CAPTURE, MenuOutcome, SCROLLBARS,
    SelectedModel, Settings, SettingsMenu, account_lines, effort_label,
};
use typed_paste::{PASTE_CHUNK_GAP, PASTE_FOLLOW_GAP, fold_text_prefix};

/// Inverted cells for both the Shift+arrows text selection and the
/// mouse-drag screen selection.
const SELECTION_STYLE: Style = Style::new().bg(Color::White).fg(Color::Black);
/// How long the copy notice popup stays up, like rho's `COPY_NOTICE_DURATION`.
const COPY_NOTICE_DURATION: Duration = Duration::from_secs(2);
const ERROR_NOTICE_DURATION: Duration = Duration::from_secs(4);
/// Longest the loop sleeps before checking background results.
const POLL_SLICE: Duration = Duration::from_millis(50);
/// Spinner frame time while a reply streams in.
const SPINNER_FRAME: Duration = Duration::from_millis(100);
/// Two presses on the same palette row this close together complete it,
/// like a double click in rho's palette.
const PALETTE_DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The symbol in every cell of the last frame, row by row.
type ScreenCells = Vec<Vec<String>>;

/// A mouse drag on screen: the cell where the button went down and the cell
/// under the pointer now, in terminal column/row coordinates.
#[derive(Clone, Copy)]
struct ScreenSelection {
    anchor: (u16, u16),
    focus: (u16, u16),
}

impl ScreenSelection {
    fn at(column: u16, row: u16) -> Self {
        Self {
            anchor: (column, row),
            focus: (column, row),
        }
    }

    /// The drag's first and last cells (both inclusive) in reading order,
    /// whichever way the pointer moved. `None` while the pointer has not
    /// left the cell it went down on, so a bare click selects nothing —
    /// selection starts with the drag.
    fn ordered(&self) -> Option<((u16, u16), (u16, u16))> {
        let (anchor, focus) = ((self.anchor.1, self.anchor.0), (self.focus.1, self.focus.0));
        (anchor != focus).then(|| {
            let ((start_row, start_col), (end_row, end_col)) = if anchor < focus {
                (anchor, focus)
            } else {
                (focus, anchor)
            };
            ((start_col, start_row), (end_col, end_row))
        })
    }

    /// Selected columns of screen `row`, flowing like a Shift+arrows text
    /// selection: the first row from the anchor on, middle rows whole, the
    /// last row up to the pointer. `text` is the row's selectable span —
    /// the message text columns, without the block padding and scrollbar.
    fn columns(&self, row: u16, text: Range<u16>) -> Option<Range<u16>> {
        let ((start_col, start_row), (end_col, end_row)) = self.ordered()?;
        if row < start_row || row > end_row {
            return None;
        }
        let from = if row == start_row { start_col } else { 0 }.max(text.start);
        let to = if row == end_row {
            end_col.saturating_add(1)
        } else {
            u16::MAX
        }
        .min(text.end);
        (from < to).then_some(from..to)
    }
}

/// What the last frame painted where a pointer can land: the transcript
/// (the only mouse-selectable chrome-free region) and the composer plus its
/// first visible row, wrap width and scrollbar, so clicks and drags map back
/// to text positions.
#[derive(Clone, Copy)]
struct FrameView {
    transcript: Rect,
    composer: Rect,
    /// Command palette rows above the composer and the match index of the
    /// first one; zero height while the palette is hidden.
    palette: Rect,
    palette_start: usize,
    view_start: usize,
    /// Text columns per composer row: the width minus the `❭ ` gutter and,
    /// while it shows, the scrollbar column.
    content_width: usize,
    composer_scrollbar: Option<Scrollbar>,
    /// First painted transcript row and the transcript's scrollbar.
    transcript_start: usize,
    transcript_scrollbar: Option<Scrollbar>,
}

/// The two scrollable regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollTarget {
    Transcript,
    Composer,
}

impl FrameView {
    /// The scrollable region painted on screen row `row`, if any.
    fn region_at(&self, row: u16) -> Option<ScrollTarget> {
        let inside = |area: Rect| row >= area.y && row < area.y + area.height;
        if inside(self.composer) {
            Some(ScrollTarget::Composer)
        } else if inside(self.transcript) {
            Some(ScrollTarget::Transcript)
        } else {
            None
        }
    }

    fn scrollbar(&self, target: ScrollTarget) -> Option<Scrollbar> {
        match target {
            ScrollTarget::Transcript => self.transcript_scrollbar,
            ScrollTarget::Composer => self.composer_scrollbar,
        }
    }

    /// The scrollbar under the pointer, with the region it scrolls.
    fn scrollbar_at(&self, column: u16, row: u16) -> Option<(ScrollTarget, Scrollbar)> {
        [ScrollTarget::Transcript, ScrollTarget::Composer]
            .into_iter()
            .find_map(|target| {
                self.scrollbar(target)
                    .filter(|bar| bar.contains(column, row))
                    .map(|bar| (target, bar))
            })
    }

    /// Transcript columns that hold message text: inside the gray block's
    /// padding and left of the scrollbar gutter.
    fn transcript_text_columns(&self) -> Range<u16> {
        let gutter = if self.transcript_scrollbar.is_some() {
            SCROLLBAR_GUTTER
        } else {
            0
        };
        let right = (self.transcript.width as usize).saturating_sub(gutter + MESSAGE_PADDING);
        let left = self.transcript.x + MESSAGE_PADDING as u16;
        left..(self.transcript.x + right as u16).max(left)
    }

    /// First painted row of `target`.
    fn start(&self, target: ScrollTarget) -> usize {
        match target {
            ScrollTarget::Transcript => self.transcript_start,
            ScrollTarget::Composer => self.view_start,
        }
    }
}

/// Columns a shown scrollbar takes from the text: the bar itself plus one
/// blank column that keeps text and message blocks off it.
const SCROLLBAR_GUTTER: usize = 2;

/// Rows one wheel notch scrolls: the transcript moves like rho's history,
/// the composer a row at a time like a text field.
const TRANSCRIPT_WHEEL_ROWS: usize = 3;
const COMPOSER_WHEEL_ROWS: usize = 1;

/// Scroll positions of both regions and the scrollbar drag in progress.
#[derive(Default)]
struct ScrollState {
    composer: ComposerScroll,
    /// Manually scrolled transcript top row; `None` follows the newest
    /// message at the bottom. `draw` turns a top at the last screenful back
    /// into following.
    transcript_top: Option<usize>,
    drag: Option<(ScrollTarget, ScrollbarDrag)>,
}

impl ScrollState {
    /// Scroll `target` so `top` is its first row; `draw` clamps it.
    fn scroll_to(&mut self, target: ScrollTarget, top: usize) {
        match target {
            ScrollTarget::Transcript => self.transcript_top = Some(top),
            ScrollTarget::Composer => {
                self.composer = ComposerScroll {
                    start: top,
                    follow_caret: false,
                }
            }
        }
    }

    fn dragging(&self, target: ScrollTarget) -> bool {
        self.drag.is_some_and(|(dragged, _)| dragged == target)
    }
}

/// Fewest and most rows the composer may grow to before it scrolls.
const COMPOSER_MIN_MAX_ROWS: usize = 2;
const COMPOSER_MAX_MAX_ROWS: usize = 8;
/// Screen rows per allowed composer row: the cap grows with the terminal,
/// reaching [`COMPOSER_MAX_MAX_ROWS`] from 32 rows up.
const SCREEN_ROWS_PER_COMPOSER_ROW: usize = 4;

/// How many rows the composer may take on a `screen_height` row terminal
/// before its text scrolls: between 2 and 8 depending on the screen.
fn composer_max_rows(screen_height: usize) -> usize {
    (screen_height / SCREEN_ROWS_PER_COMPOSER_ROW)
        .clamp(COMPOSER_MIN_MAX_ROWS, COMPOSER_MAX_MAX_ROWS)
}

/// The composer's scroll position. Keys and pastes make the view follow the
/// caret; the wheel and the scrollbar scroll it freely until the next key.
#[derive(Clone, Copy)]
struct ComposerScroll {
    start: usize,
    follow_caret: bool,
}

impl Default for ComposerScroll {
    fn default() -> Self {
        Self {
            start: 0,
            follow_caret: true,
        }
    }
}

/// Tone of the transient copy notice, like rho's `CopyNoticeTone`.
#[derive(Clone, Copy)]
enum NoticeTone {
    Success,
    Error,
}

/// A transient copy-report popup, ported from rho's `CopyNotice`: the
/// message, its tone, and the instant it stops being shown.
struct CopyNotice {
    message: String,
    tone: NoticeTone,
    visible_until: Instant,
}

impl CopyNotice {
    fn copied(character_count: usize, now: Instant) -> Self {
        Self {
            message: format!("Скопировано символов: {character_count}"),
            tone: NoticeTone::Success,
            visible_until: now + COPY_NOTICE_DURATION,
        }
    }

    fn failed_settings(detail: String) -> Self {
        Self::error(format!("не удалось сохранить настройки: {detail}"))
    }

    /// A longer-lived error report (chat and session problems).
    fn error(message: String) -> Self {
        Self {
            message,
            tone: NoticeTone::Error,
            visible_until: Instant::now() + ERROR_NOTICE_DURATION,
        }
    }

    fn failed(detail: String, now: Instant) -> Self {
        Self {
            message: format!("не удалось скопировать: {detail}"),
            tone: NoticeTone::Error,
            visible_until: now + COPY_NOTICE_DURATION,
        }
    }

    fn is_visible(&self, now: Instant) -> bool {
        now < self.visible_until
    }
}

/// Renders the copy notice as a bordered box in `area`'s top-right corner,
/// like rho's `render_copy_notice`.
fn render_copy_notice(f: &mut Frame, area: Rect, notice: &CopyNotice, now: Instant) {
    if !notice.is_visible(now) || area.width == 0 || area.height == 0 {
        return;
    }
    let popup_width =
        (UnicodeWidthStr::width(notice.message.as_str()) + 4).min(area.width as usize) as u16;
    let popup_height = area.height.min(3);
    let popup = Rect::new(
        area.x
            .saturating_add(area.width.saturating_sub(popup_width)),
        area.y,
        popup_width,
        popup_height,
    );
    let color = match notice.tone {
        NoticeTone::Success => Color::Green,
        NoticeTone::Error => Color::Red,
    };
    f.render_widget(Clear, popup);
    if popup.width >= 3 && popup.height >= 3 {
        f.render_widget(
            Paragraph::new(notice.message.as_str())
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::new().fg(color)),
                ),
            popup,
        );
    } else {
        f.render_widget(
            Paragraph::new(notice.message.as_str())
                .alignment(Alignment::Right)
                .style(Style::new().fg(color)),
            popup,
        );
    }
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let keyboard = KeyboardProtocols::acquire();
    let mut settings = Settings::load();
    // Capturing the mouse replaces the terminal's own selection with ours so
    // we can paint it white and copy it from Ctrl+C.
    set_mouse_capture(settings.flag(MOUSE_CAPTURE));
    let result = Hub::new().and_then(|mut hub| run(&mut terminal, &mut settings, &mut hub));
    let _ = execute!(io::stdout(), DisableMouseCapture);
    keyboard.release();
    ratatui::restore();
    result
}

/// Turn the terminal's mouse reporting on or off, per «Захват мыши».
fn set_mouse_capture(enabled: bool) {
    let _ = if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    };
}

fn run(terminal: &mut DefaultTerminal, settings: &mut Settings, hub: &mut Hub) -> io::Result<()> {
    let mut composer = Composer::default();
    // The conversation on screen; a fresh one each launch, `/sessions`
    // switches to a saved one.
    let mut session = Session::new();
    // `/models`, `/effort` and `/sessions` lists; while open it owns keys.
    let mut picker: Option<Picker> = None;
    // Display name of the model the reply in flight comes from.
    let mut reply_model = String::new();
    let mut history_cursor: Option<usize> = None;
    let mut draft: Option<String> = None;
    let mut clipboard = Clipboard::new().ok();
    let mut screen_selection: Option<ScreenSelection> = None;
    // Cells painted on the last frame; what a mouse selection refers to when
    // Ctrl+C copies it.
    let mut screen_lines: ScreenCells = Vec::new();
    // Layout of the last frame's pointer-reachable regions; clicks and drags
    // map back to text positions through it.
    let mut frame_view: Option<FrameView> = None;
    // A press inside the composer drags a text selection (like Shift+arrows)
    // rather than a screen selection.
    let mut composer_drag = false;
    // Transient copy report; expires on its own like rho's `CopyNotice`.
    let mut notice: Option<CopyNotice> = None;
    // Events read together with an earlier one, still waiting their turn.
    let mut pending: VecDeque<Event> = VecDeque::new();
    // When the last text key or paste was read: a lone key right behind it
    // is the tail of a paste ConPTY split, not something typed.
    let mut last_text_at: Option<Instant> = None;
    // Transcript and composer scroll positions, and any scrollbar drag.
    let mut scroll = ScrollState::default();
    // The `/` command suggestions above the composer.
    let mut palette = CommandPalette::default();
    // Last press on a palette row, so a second one completes it.
    let mut palette_click: Option<(usize, Instant)> = None;
    // The `/config` menu; while open it owns the screen and every key.
    let mut settings_menu: Option<SettingsMenu> = None;

    loop {
        // Background results: finished replies join the session, fresh
        // catalogs refill an open `/models`.
        let hub_changed = hub.poll();
        for result in hub.take_finished() {
            session.push(match result {
                Ok(reply) if reply.text.trim().is_empty() => {
                    ChatMessage::reply(Role::Error, "модель вернула пустой ответ", &reply_model)
                        .with_thinking(reply.thinking)
                        .with_thought(reply.thought)
                }
                Ok(reply) => ChatMessage::reply(Role::Assistant, reply.text, &reply_model)
                    .with_thinking(reply.thinking)
                    .with_thought(reply.thought),
                Err(error) => ChatMessage::reply(Role::Error, error, &reply_model),
            });
            save_session(&session, &mut notice);
        }
        // A stored effort the selected model doesn't offer folds to its
        // own default as soon as the catalog is known (a Devin family may
        // lack «medium», a single config may allow only «выкл»).
        if let Some(info) = settings.model.as_ref().and_then(|model| {
            hub.entry(model.provider)
                .models()
                .iter()
                .find(|m| m.id == model.id)
        }) && !info.accepts(settings.effort)
        {
            settings.effort = info.fit(settings.effort);
            save_settings(settings, &mut notice);
        }
        if hub_changed && let Some(open) = picker.as_mut() {
            match open.purpose {
                PickerPurpose::Models => open.set_items(model_items(hub, settings)),
                PickerPurpose::Effort => open.set_items(effort_items(hub, settings)),
                PickerPurpose::Usage => open.set_items(usage_items(&hub.providers)),
                PickerPurpose::Sessions => {}
            }
        }
        // `/usage` pulls every provider's account, not just the shown one.
        if picker
            .as_ref()
            .is_some_and(|open| open.purpose == PickerPurpose::Usage)
        {
            for kind in ProviderKind::ALL {
                hub.ensure_account(kind);
            }
        }
        if let Some(kind) = settings_menu
            .as_ref()
            .and_then(SettingsMenu::shown_provider)
        {
            hub.ensure_account(kind);
            hub.ensure_models(kind);
        }
        if notice
            .as_ref()
            .is_some_and(|current| !current.is_visible(Instant::now()))
        {
            notice = None;
        }

        palette.set_hidden(!settings.flag(COMMAND_HINTS));
        let rate = settings
            .model
            .as_ref()
            .and_then(|model| hub.generation_rate(model.provider, &model.id));
        terminal.draw(|f| {
            if let Some(menu) = settings_menu.as_mut() {
                let area = f.area();
                f.render_widget(Clear, area);
                menu.render(area, f.buffer_mut(), settings, &hub.providers);
                return;
            }
            let view = TranscriptView {
                messages: &session.messages,
                pending: hub.streaming(),
                rate,
            };
            draw(
                f,
                &mut composer,
                &view,
                screen_selection,
                &mut screen_lines,
                &mut frame_view,
                &mut scroll,
                &mut palette,
                picker.as_mut(),
                notice.as_ref(),
                settings,
            )
        })?;

        // Sleep until input, a background result, the notice expiring or
        // the next spinner frame.
        if pending.is_empty()
            && !wait_for_input(
                hub,
                notice.as_ref().map(|current| current.visible_until),
                hub.busy(),
            )?
        {
            continue;
        }

        let next = match pending.pop_front() {
            Some(event) => event,
            None => read_event_batch(&mut pending, &mut last_text_at)?,
        };
        if let Some(menu) = settings_menu.as_mut() {
            let mouse_capture = settings.flag(MOUSE_CAPTURE);
            let outcome = match next {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    menu.handle_key(key, settings, &hub.providers)
                }
                Event::Mouse(mouse) => menu.handle_mouse(mouse, settings, &hub.providers),
                Event::Paste(text) => {
                    menu.handle_paste(&text);
                    MenuOutcome::Stay
                }
                _ => MenuOutcome::Stay,
            };
            match outcome {
                MenuOutcome::Close => settings_menu = None,
                MenuOutcome::Changed => {
                    save_settings(settings, &mut notice);
                    if settings.flag(MOUSE_CAPTURE) != mouse_capture {
                        set_mouse_capture(settings.flag(MOUSE_CAPTURE));
                    }
                }
                MenuOutcome::Login(kind) => hub.login(kind),
                MenuOutcome::CancelLogin(kind) => hub.cancel_login(kind),
                MenuOutcome::Logout(kind) => match hub.logout(kind) {
                    Ok(()) => {
                        // The chosen model went away with the account.
                        if settings.model.as_ref().is_some_and(|m| m.provider == kind) {
                            settings.model = None;
                            save_settings(settings, &mut notice);
                        }
                    }
                    Err(error) => {
                        notice = Some(CopyNotice::error(format!("не удалось выйти: {error}")));
                    }
                },
                MenuOutcome::Stay => {}
            }
            continue;
        }
        if let Some(open) = picker.as_mut() {
            let outcome = match next {
                Event::Key(key) if key.kind == KeyEventKind::Press => open.handle_key(key),
                Event::Mouse(mouse) => open.handle_mouse(mouse),
                _ => PickerOutcome::Stay,
            };
            let purpose = open.purpose;
            match outcome {
                PickerOutcome::Stay => {}
                PickerOutcome::Close => picker = None,
                PickerOutcome::Pick(value) => {
                    picker = None;
                    match purpose {
                        PickerPurpose::Models => {
                            if let Some(model) = selected_model(hub, &value) {
                                settings.model = Some(model);
                                save_settings(settings, &mut notice);
                            }
                        }
                        // Read-only list: a pick already closed it.
                        PickerPurpose::Usage => {}
                        PickerPurpose::Effort => {
                            settings.effort = match value.as_str() {
                                "off" => None,
                                _ => value.parse().ok(),
                            };
                            save_settings(settings, &mut notice);
                        }
                        PickerPurpose::Sessions if value == session.id => {}
                        PickerPurpose::Sessions => match Session::load(&value) {
                            Ok(next_session) => {
                                stop_reply(hub, &mut session, &reply_model);
                                save_session(&session, &mut notice);
                                session = next_session;
                                screen_selection = None;
                                scroll.transcript_top = None;
                                history_cursor = None;
                                draft = None;
                            }
                            Err(error) => {
                                notice = Some(CopyNotice::error(format!(
                                    "не удалось открыть сессию: {error}"
                                )));
                            }
                        },
                    }
                }
                PickerOutcome::Delete(id) => {
                    if id == session.id {
                        hub.cancel_chat();
                        session = Session::new();
                        scroll.transcript_top = None;
                    }
                    if let Err(error) = sessions::delete(&id) {
                        notice = Some(CopyNotice::error(format!(
                            "не удалось удалить сессию: {error}"
                        )));
                    }
                    open.set_items(session_items(&session));
                }
            }
            continue;
        }
        match next {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                scroll.composer.follow_caret = true;
                // The open command palette owns Up/Down/Tab/Esc; Enter
                // completes the highlighted command, then submits below.
                if let Some(palette_key) = commands::palette_key(key)
                    && let Some(matches) =
                        palette.visible_matches(composer.text(), composer.cursor())
                {
                    match palette_key {
                        PaletteKey::Up | PaletteKey::Down => {
                            palette.step(matches.len(), palette_key == PaletteKey::Up);
                            continue;
                        }
                        PaletteKey::Esc => {
                            if composer.text() == "/" {
                                composer.set_text(String::new());
                            }
                            palette.dismiss();
                            continue;
                        }
                        PaletteKey::Tab | PaletteKey::Enter => {
                            if let Some(command) = palette.selected(&matches) {
                                complete_palette_command(&mut composer, &mut palette, command);
                                history_cursor = None;
                                draft = None;
                            }
                            if palette_key == PaletteKey::Tab {
                                continue;
                            }
                        }
                    }
                }
                match key.code {
                    // Only a bare Enter submits; Shift+Enter falls through to
                    // `EditKey::from_key`, which inserts a newline where the
                    // terminal reports it.
                    KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                        let submitted = composer.take();
                        let submitted = submitted.trim();
                        palette.input_changed("", 0);
                        if submitted == EXIT_COMMAND {
                            return Ok(());
                        }
                        if submitted == NEW_COMMAND {
                            // The old session is already on disk (saved
                            // after every message); start a clean one.
                            stop_reply(hub, &mut session, &reply_model);
                            save_session(&session, &mut notice);
                            session = Session::new();
                            screen_selection = None;
                            scroll.transcript_top = None;
                            history_cursor = None;
                            draft = None;
                            continue;
                        }
                        let opened = if SETTINGS_COMMANDS.contains(&submitted) {
                            // The providers tab goes through `ensure_account`,
                            // which serves the 60-second account cache.
                            settings_menu = Some(SettingsMenu::new());
                            true
                        } else if USAGE_COMMANDS.contains(&submitted) {
                            // Opens on the cached data at once; the loop's
                            // `ensure_account` calls refresh whatever is stale
                            // in the background and swap it in when it lands.
                            picker = command_picker(submitted, hub, settings, &session);
                            true
                        } else if let Some(open) =
                            command_picker(submitted, hub, settings, &session)
                        {
                            picker = Some(open);
                            true
                        } else {
                            false
                        };
                        if opened {
                            screen_selection = None;
                            composer_drag = false;
                            scroll.drag = None;
                        } else if !submitted.is_empty() {
                            if hub.streaming().is_some() {
                                composer.set_text(submitted.to_string());
                                notice = Some(CopyNotice::error(
                                    "дождитесь ответа или остановите его клавишей Esc".into(),
                                ));
                            } else if let Some(model) = settings.model.clone() {
                                session.push(ChatMessage::user(submitted));
                                save_session(&session, &mut notice);
                                reply_model = model.name.clone();
                                hub.send_chat(
                                    model.provider,
                                    model.id,
                                    settings.effort,
                                    &settings.system_prompt,
                                    chat_history(&session),
                                );
                                // A new message brings the transcript back
                                // to the bottom, where it shows.
                                scroll.transcript_top = None;
                            } else {
                                composer.set_text(submitted.to_string());
                                notice = Some(CopyNotice::error(
                                    "модель не выбрана — выберите её командой /models".into(),
                                ));
                            }
                        }
                        history_cursor = None;
                        draft = None;
                    }
                    // Esc stops the reply streaming in.
                    KeyCode::Esc if hub.streaming().is_some() => {
                        stop_reply(hub, &mut session, &reply_model);
                        save_session(&session, &mut notice);
                    }
                    _ => {
                        let Some(edit) = EditKey::from_key(key) else {
                            continue;
                        };
                        match edit {
                            EditKey::Copy => {
                                if let Some(text) = take_copied_selection(
                                    &mut screen_selection,
                                    &screen_lines,
                                    frame_view,
                                    &mut composer,
                                ) {
                                    notice = Some(copy_to_clipboard(&mut clipboard, text));
                                }
                            }
                            EditKey::Cut => {
                                // Only the composer's own selection — the
                                // Shift+arrows one or a drag inside the field
                                // — can be cut. Anything else selected (the
                                // transcript) is read-only, so Ctrl+X copies
                                // it exactly like Ctrl+C.
                                if let Some(text) =
                                    composer.take_selected().filter(|text| !text.is_empty())
                                {
                                    notice = Some(copy_to_clipboard(&mut clipboard, text));
                                    palette.input_changed(composer.text(), composer.cursor());
                                    history_cursor = None;
                                    draft = None;
                                } else if let Some(text) = take_copied_selection(
                                    &mut screen_selection,
                                    &screen_lines,
                                    frame_view,
                                    &mut composer,
                                ) {
                                    notice = Some(copy_to_clipboard(&mut clipboard, text));
                                }
                            }
                            EditKey::Paste => {
                                if let Some(text) =
                                    clipboard.as_mut().and_then(|cb| cb.get_text().ok())
                                {
                                    composer.delete_selection();
                                    composer.insert_text(&pasted_text(&text));
                                    palette.input_changed(composer.text(), composer.cursor());
                                }
                                history_cursor = None;
                                draft = None;
                            }
                            _ => match composer.apply_edit(edit) {
                                EditOutcome::Edited => {
                                    palette.input_changed(composer.text(), composer.cursor());
                                    history_cursor = None;
                                    draft = None;
                                }
                                EditOutcome::TextUnchanged => {
                                    history_cursor = None;
                                    draft = None;
                                }
                                EditOutcome::VerticalEdge(direction) => {
                                    let history = session.user_texts();
                                    if settings.flag(HISTORY_RECALL)
                                        && recall_history(
                                            direction,
                                            &history,
                                            &mut history_cursor,
                                            &mut draft,
                                            &mut composer,
                                        )
                                    {
                                        // Recalled text is finished content,
                                        // not a live search: keep the palette
                                        // closed until the next edit, like rho.
                                        palette.dismiss();
                                    } else {
                                        composer.move_vertically(direction);
                                    }
                                }
                            },
                        }
                    }
                }
            }
            // Bracketed paste where the terminal delivers it, or a keystroke
            // paste folded back by `read_event_batch` (Windows ConPTY).
            Event::Paste(text) => {
                scroll.composer.follow_caret = true;
                composer.delete_selection();
                composer.insert_text(&pasted_text(&text));
                palette.input_changed(composer.text(), composer.cursor());
                history_cursor = None;
                draft = None;
            }
            Event::Mouse(mouse) => match mouse.kind {
                // A press on a scrollbar grabs the thumb (or jumps the track
                // there) without touching the caret or the composer selection.
                MouseEventKind::Down(MouseButton::Left)
                    if let Some((target, scrollbar)) =
                        frame_view.and_then(|view| view.scrollbar_at(mouse.column, mouse.row)) =>
                {
                    let drag = scrollbar.begin_drag(mouse.row);
                    scroll.drag = Some((target, drag));
                    composer_drag = false;
                    // A screen selection is pinned to cells; scrolled text
                    // would slide out from under it.
                    screen_selection = None;
                    scroll.scroll_to(target, scrollbar.top_line_for_pointer(mouse.row, drag));
                }
                MouseEventKind::Drag(MouseButton::Left)
                    if let Some((target, drag)) = scroll.drag
                        && let Some(scrollbar) =
                            frame_view.and_then(|view| view.scrollbar(target)) =>
                {
                    scroll.scroll_to(target, scrollbar.top_line_for_pointer(mouse.row, drag));
                }
                // A press on a palette row highlights it; a second press on
                // the same row completes it, like rho's palette pointer.
                MouseEventKind::Down(MouseButton::Left)
                    if let Some(view) = frame_view
                        && mouse.row >= view.palette.y
                        && mouse.row < view.palette.bottom() =>
                {
                    screen_selection = None;
                    scroll.drag = None;
                    composer_drag = false;
                    let index = view.palette_start + (mouse.row - view.palette.y) as usize;
                    let now = Instant::now();
                    let double = palette_click.is_some_and(|(clicked, at)| {
                        clicked == index && now.duration_since(at) < PALETTE_DOUBLE_CLICK
                    });
                    palette.select(index);
                    if double {
                        palette_click = None;
                        if let Some(command) = palette
                            .visible_matches(composer.text(), composer.cursor())
                            .and_then(|matches| palette.selected(&matches))
                        {
                            complete_palette_command(&mut composer, &mut palette, command);
                            history_cursor = None;
                            draft = None;
                        }
                    } else {
                        palette_click = Some((index, now));
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    scroll.drag = None;
                }
                // The wheel scrolls the region under the pointer; `draw`
                // clamps the start to the last screenful.
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    if let Some(view) = frame_view
                        && let Some(target) = view.region_at(mouse.row) =>
                {
                    let rows = match target {
                        ScrollTarget::Transcript => {
                            screen_selection = None;
                            TRANSCRIPT_WHEEL_ROWS
                        }
                        ScrollTarget::Composer => COMPOSER_WHEEL_ROWS,
                    };
                    let start = view.start(target);
                    scroll.scroll_to(
                        target,
                        if mouse.kind == MouseEventKind::ScrollUp {
                            start.saturating_sub(rows)
                        } else {
                            start + rows
                        },
                    );
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    screen_selection = None;
                    scroll.drag = None;
                    if let Some(view) = frame_view
                        && mouse.row >= view.composer.y
                        && mouse.row < view.composer.y + view.composer.height
                    {
                        // Press inside the field: place the caret and anchor a
                        // selection a drag can extend, like Shift+arrows.
                        composer_drag = true;
                        composer.press_at(composer_char_at(
                            &composer,
                            view,
                            mouse.column,
                            mouse.row,
                        ));
                    } else {
                        composer_drag = false;
                        composer.clear_selection();
                        // Only the transcript is mouse-selectable; the rules,
                        // and the `❭ ` prefix are chrome.
                        if let Some(view) = frame_view
                            && mouse.row >= view.transcript.y
                            && mouse.row < view.transcript.y + view.transcript.height
                        {
                            screen_selection = Some(ScreenSelection::at(mouse.column, mouse.row));
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if composer_drag {
                        if let Some(view) = frame_view {
                            composer.drag_to(composer_char_at(
                                &composer,
                                view,
                                mouse.column,
                                mouse.row,
                            ));
                        }
                    } else if let Some(selection) = screen_selection.as_mut() {
                        selection.focus = (mouse.column, mouse.row);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Read the next event plus everything already queued behind it.
///
/// A batch that opens with two or more text presses is a keystroke paste
/// (ConPTY drops the bracketed-paste markers, so pasted text arrives as key
/// presses, each line break a plain Enter). VS Code feeds a long paste in
/// chunks, so the burst keeps collecting chunks that follow within
/// [`PASTE_CHUNK_GAP`] and comes back as one `Event::Paste` — the trailing
/// Enter of a paste included, which therefore never submits. Otherwise the
/// first event is returned and the rest wait in `pending`.
///
/// ConPTY sometimes splits a paste so a batch holds a single press — often
/// the line break's Enter. A text press read within [`PASTE_FOLLOW_GAP`] of
/// the previous one, faster than anyone types, continues that paste too.
fn read_event_batch(
    pending: &mut VecDeque<Event>,
    last_text_at: &mut Option<Instant>,
) -> io::Result<Event> {
    let mut batch = read_queued(event::read()?)?;
    let now = Instant::now();
    let folded = fold_text_prefix(&batch);
    let follows_text = last_text_at.is_some_and(|at| now.duration_since(at) < PASTE_FOLLOW_GAP);
    if folded.consumed == 0 {
        // A chord, an arrow or the mouse: whatever came before is over.
        *last_text_at = None;
    } else if folded.presses > 0 {
        *last_text_at = Some(now);
    }
    let starts_burst = folded.presses >= 2 || (folded.presses == 1 && follows_text);
    if !starts_burst {
        let mut events = batch.into_iter();
        let first = events.next().expect("batch starts with the read event");
        pending.extend(events);
        return Ok(first);
    }

    let mut text = folded.text;
    let mut consumed = folded.consumed;
    // The burst ends at the first event a paste cannot produce (left for
    // normal handling) or when no chunk follows within the gap.
    while consumed == batch.len() && event::poll(PASTE_CHUNK_GAP)? {
        batch = read_queued(event::read()?)?;
        let folded = fold_text_prefix(&batch);
        text.push_str(&folded.text);
        consumed = folded.consumed;
    }
    *last_text_at = Some(Instant::now());
    pending.extend(batch.into_iter().skip(consumed));
    Ok(Event::Paste(text))
}

/// `first` plus every event already queued behind it.
fn read_queued(first: Event) -> io::Result<Vec<Event>> {
    let mut batch = vec![first];
    while event::poll(Duration::ZERO)? {
        batch.push(event::read()?);
    }
    Ok(batch)
}

/// Replace the composer's command token with `command`, keeping any
/// arguments, and reopen the palette for the completed text.
fn complete_palette_command(
    composer: &mut Composer,
    palette: &mut CommandPalette,
    command: &commands::CommandSpec,
) {
    let (text, cursor) =
        commands::complete_command(composer.text(), composer.cursor(), command.name);
    composer.set_text_and_cursor(text, cursor);
    palette.input_changed(composer.text(), composer.cursor());
}

/// Normalize pasted line endings for the single `\n` the buffer stores.
fn pasted_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The text Ctrl+C copies, closing the selection it came from. A mouse
/// selection wins over the composer's own selection; with neither there is
/// nothing to copy. The copied selection closes either way.
fn take_copied_selection(
    screen_selection: &mut Option<ScreenSelection>,
    screen_lines: &[Vec<String>],
    frame_view: Option<FrameView>,
    composer: &mut Composer,
) -> Option<String> {
    screen_selection
        .take()
        .and_then(|selection| selected_screen_text(screen_lines, frame_view?, selection))
        .or_else(|| {
            let text = composer.selected_text();
            composer.clear_selection();
            text
        })
        .filter(|text| !text.is_empty())
}

/// Push `text` onto the system clipboard and describe the result for the
/// popup, like rho's `CopyNotice::from_copy_result`.
fn copy_to_clipboard(clipboard: &mut Option<Clipboard>, text: String) -> CopyNotice {
    let now = Instant::now();
    let chars = text.chars().count();
    match clipboard.as_mut().map(|cb| cb.set_text(text)) {
        Some(Ok(())) => CopyNotice::copied(chars, now),
        Some(Err(error)) => CopyNotice::failed(error.to_string(), now),
        None => CopyNotice::failed("буфер обмена недоступен".to_string(), now),
    }
}

/// Char index under screen cell `(column, row)` inside the composer. Rows
/// outside it clamp to the text's start or end, so a drag that leaves the
/// field still extends to its edge like a text editor's.
fn composer_char_at(composer: &Composer, view: FrameView, column: u16, row: u16) -> usize {
    let visual_lines = editable_input_visual_lines(composer.text(), view.content_width);
    let visual_row = if row < view.composer.y {
        0
    } else if row >= view.composer.y + view.composer.height {
        usize::MAX
    } else {
        view.view_start + (row - view.composer.y) as usize
    };
    // Columns left of the text — the `❭ ` gutter — map to the line start, so
    // the prefix can never be part of a selection.
    let display_column = (column as usize).saturating_sub(prompt_width());
    cursor_index_on_visual_line(composer.text(), &visual_lines, visual_row, display_column)
}

/// The text inside a mouse selection of the painted transcript, flowing
/// like a Shift+arrows selection: from the anchor to the end of its row,
/// whole rows between, the last row up to the pointer. Rows join with `\n`
/// and trailing blanks are trimmed.
fn selected_screen_text(
    lines: &[Vec<String>],
    view: FrameView,
    selection: ScreenSelection,
) -> Option<String> {
    let ((_, start_row), (_, end_row)) = selection.ordered()?;
    // Chrome rows — the rules — never contribute text.
    let first = start_row.max(view.transcript.y);
    let last = end_row.min(view.transcript.bottom().saturating_sub(1));
    let text_columns = view.transcript_text_columns();
    let mut rows = Vec::new();
    for row in first..=last {
        let (Some(line), Some(columns)) = (
            lines.get(row as usize),
            selection.columns(row, text_columns.clone()),
        ) else {
            rows.push(String::new());
            continue;
        };
        // Cells, not chars: a wide char's trailing cell holds an empty
        // symbol, so columns stay aligned with what was painted.
        let text: String = line
            .iter()
            .skip(columns.start as usize)
            .take(columns.len())
            .map(String::as_str)
            .collect();
        rows.push(text.trim_end().to_string());
    }
    let text = rows.join("\n").trim_matches('\n').to_string();
    (!text.trim().is_empty()).then_some(text)
}

/// Recall a submitted text on a vertical caret edge, like rho's history
/// stepping: Up from the live draft saves it and shows the newest entry;
/// Down past the newest entry restores the draft. Returns false when history
/// does not apply, so the caret can move between wrapped rows instead.
fn recall_history(
    direction: Vertical,
    history: &[String],
    cursor: &mut Option<usize>,
    draft: &mut Option<String>,
    composer: &mut Composer,
) -> bool {
    if history.is_empty() {
        return false;
    }
    let len = history.len();
    let index = match (direction, *cursor) {
        (Vertical::Up, None) => {
            *draft = Some(composer.text().to_string());
            len - 1
        }
        (Vertical::Up, Some(current)) => current.saturating_sub(1),
        (Vertical::Down, None) => return false,
        (Vertical::Down, Some(current)) if current + 1 < len => current + 1,
        (Vertical::Down, Some(_)) => {
            composer.set_text(draft.take().unwrap_or_default());
            *cursor = None;
            return true;
        }
    };
    composer.set_text(history[index].clone());
    *cursor = Some(index);
    true
}

/// The messages a chat request carries: user and assistant turns; failed
/// requests stay on screen only.
fn chat_history(session: &Session) -> Vec<Message> {
    session
        .messages
        .iter()
        .filter_map(|message| match message.role {
            Role::User => Some(Message::user(message.text.clone())),
            Role::Assistant => Some(Message::assistant(message.text.clone())),
            Role::Error => None,
        })
        .collect()
}

/// Stop the reply in flight, keeping what had streamed in.
fn stop_reply(hub: &mut Hub, session: &mut Session, model: &str) {
    let Some(partial) = hub.cancel_chat() else {
        return;
    };
    if !partial.text.trim().is_empty() || !partial.thinking.trim().is_empty() {
        session.push(
            ChatMessage::reply(Role::Assistant, partial.text, model)
                .with_thinking(partial.thinking)
                .with_thought(partial.thought),
        );
    }
    session.push(ChatMessage::reply(Role::Error, "ответ остановлен", model));
}

fn save_session(session: &Session, notice: &mut Option<CopyNotice>) {
    if let Err(error) = session.save() {
        *notice = Some(CopyNotice::error(format!(
            "не удалось сохранить сессию: {error}"
        )));
    }
}

fn save_settings(settings: &Settings, notice: &mut Option<CopyNotice>) {
    if let Err(error) = settings.save() {
        *notice = Some(CopyNotice::failed_settings(error.to_string()));
    }
}

/// Block until terminal input is ready (true) or the screen should redraw
/// (false): a background result arrived, `notice_until` passed, or a
/// spinner frame is due while `animate`.
fn wait_for_input(hub: &mut Hub, notice_until: Option<Instant>, animate: bool) -> io::Result<bool> {
    let frame = animate.then(|| Instant::now() + SPINNER_FRAME);
    loop {
        if hub.has_events() {
            return Ok(false);
        }
        let now = Instant::now();
        let slice = match [notice_until, frame].into_iter().flatten().min() {
            Some(deadline) if deadline <= now => return Ok(false),
            Some(deadline) => (deadline - now).min(POLL_SLICE),
            None => POLL_SLICE,
        };
        if event::poll(slice)? {
            return Ok(true);
        }
    }
}

/// The list a `/models`, `/effort` (and aliases) or `/sessions` command
/// opens, or `None` for any other text.
fn command_picker(
    command: &str,
    hub: &Hub,
    settings: &Settings,
    session: &Session,
) -> Option<Picker> {
    let (purpose, title, items) = if command == MODELS_COMMAND {
        (PickerPurpose::Models, "Модель", model_items(hub, settings))
    } else if EFFORT_COMMANDS.contains(&command) {
        (
            PickerPurpose::Effort,
            "Уровень рассуждений",
            effort_items(hub, settings),
        )
    } else if command == SESSIONS_COMMAND {
        (PickerPurpose::Sessions, "Сессии", session_items(session))
    } else if USAGE_COMMANDS.contains(&command) {
        (PickerPurpose::Usage, "Лимиты", usage_items(&hub.providers))
    } else {
        return None;
    };
    let mut picker = Picker::new(purpose, title);
    picker.set_items(items);
    Some(picker)
}

/// `/models`: the shown models of every signed-in provider, grouped.
fn model_items(hub: &Hub, settings: &Settings) -> Vec<PickerItem> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut items = Vec::new();
    for entry in hub.providers.iter().filter(|entry| entry.authenticated) {
        items.push(PickerItem::heading(entry.kind.display_name()));
        match &entry.models {
            Load::Idle | Load::Loading => items.push(PickerItem::note("загрузка моделей…", dim)),
            Load::Failed(error) => items.push(PickerItem::note(
                format!("не удалось загрузить: {error}"),
                Style::new().fg(Color::Red),
            )),
            Load::Ready(models) => {
                let before = items.len();
                for model in models
                    .iter()
                    .filter(|model| !settings.model_hidden(entry.kind, &model.id))
                {
                    let current = settings
                        .model
                        .as_ref()
                        .is_some_and(|m| m.provider == entry.kind && m.id == model.id);
                    items.push(
                        PickerItem::option(
                            format!("{}:{}", entry.kind.as_str(), model.id),
                            model.display_name.clone(),
                        )
                        .detail(model.id.clone())
                        .current(current),
                    );
                }
                if items.len() == before {
                    items.push(PickerItem::note(
                        "все модели скрыты — включите их в /config → Провайдеры",
                        dim,
                    ));
                }
            }
        }
    }
    if items.is_empty() {
        items.push(PickerItem::note(
            "Нет авторизованных провайдеров — войдите в /config → Провайдеры",
            dim,
        ));
    }
    items
}

/// The model a `/models` pick (`provider:id`) names.
fn selected_model(hub: &Hub, value: &str) -> Option<SelectedModel> {
    let (provider, id) = value.split_once(':')?;
    let provider: ProviderKind = provider.parse().ok()?;
    let name = hub
        .entry(provider)
        .models()
        .iter()
        .find(|model| model.id == id)
        .map_or_else(|| id.to_string(), |model| model.display_name.clone());
    Some(SelectedModel {
        provider,
        id: id.to_string(),
        name,
    })
}

/// `/effort`: only the levels the selected model actually offers — its
/// own list (a Devin family like `low/high/xhigh`) or the standard one —
/// plus «выкл» when the model allows turning reasoning off.
fn effort_items(hub: &Hub, settings: &Settings) -> Vec<PickerItem> {
    let dim = Style::new().fg(Color::DarkGray);
    let Some(model) = &settings.model else {
        return vec![PickerItem::note(
            "модель не выбрана — выберите её командой /models",
            dim,
        )];
    };
    match &hub.entry(model.provider).models {
        Load::Idle | Load::Loading => vec![PickerItem::note("загрузка моделей…", dim)],
        Load::Failed(error) => vec![PickerItem::note(
            format!("не удалось загрузить модели: {error}"),
            Style::new().fg(Color::Red),
        )],
        Load::Ready(models) => {
            // A model missing from the fresh catalog gets the standard list.
            let fallback = ModelInfo::default();
            let info = models
                .iter()
                .find(|m| m.id == model.id)
                .unwrap_or(&fallback);
            effort_options(info, &model.name, settings)
        }
    }
}

/// The option rows of `/effort` for one known model.
fn effort_options(info: &ModelInfo, name: &str, settings: &Settings) -> Vec<PickerItem> {
    let mut items = vec![PickerItem::heading(name.to_string())];
    let option = |effort: Option<Effort>, detail: String| {
        let mut label = effort_label(effort).to_string();
        if let Some(first) = label.get(..label.chars().next().map_or(0, char::len_utf8)) {
            label = first.to_uppercase() + &label[first.len()..];
        }
        PickerItem::option(effort.map_or("off", |e| e.as_str()), label)
            .detail(detail)
            .current(settings.effort == effort)
    };
    if info.effort_off {
        items.push(option(None, "без рассуждений".to_string()));
    }
    let default = info.default_level();
    for &level in info.effort_levels() {
        let detail = if default == Some(level) {
            format!("{} · по умолчанию", effort_detail(level))
        } else {
            effort_detail(level).to_string()
        };
        items.push(option(Some(level), detail));
    }
    if items.len() == 1 {
        items.push(PickerItem::note(
            "  у этой модели нет уровней рассуждений",
            Style::new().fg(Color::DarkGray),
        ));
    }
    items
}

/// One-line description of a level in the `/effort` list.
fn effort_detail(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal => "быстрее и дешевле всего",
        Effort::Low => "быстрее и дешевле",
        Effort::Medium => "баланс скорости и качества",
        Effort::High => "дольше думает над сложным",
        Effort::XHigh => "ещё дольше думает над сложным",
        Effort::Max => "самый большой бюджет рассуждений",
    }
}

/// `/usage` (alias `/limits`): every provider's account limits — the
/// account rows of the Providers tab flattened into one scrollable list,
/// grouped under a heading per provider.
fn usage_items(providers: &[ProviderEntry]) -> Vec<PickerItem> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut items = Vec::new();
    let mut signed_in = false;
    for entry in providers {
        items.push(PickerItem::heading(entry.kind.display_name()));
        if !entry.authenticated {
            items.push(PickerItem::note("вход не выполнен", dim));
            continue;
        }
        signed_in = true;
        match &entry.account {
            Load::Idle | Load::Loading => items.push(PickerItem::note("загрузка…", dim)),
            Load::Failed(error) => items.push(PickerItem::note(
                format!("не удалось загрузить: {error}"),
                Style::new().fg(Color::Red),
            )),
            Load::Ready(info) => {
                items.extend(account_lines(info).into_iter().map(PickerItem::line));
            }
        }
    }
    if !signed_in {
        items.clear();
        items.push(PickerItem::note(
            "Нет авторизованных провайдеров — войдите в /config → Провайдеры",
            dim,
        ));
    }
    items
}

/// `/sessions`: the saved history only, ordered by the last message,
/// newest first. `/new` starts a fresh session.
fn session_items(current: &Session) -> Vec<PickerItem> {
    let mut items = Vec::new();
    for summary in sessions::list() {
        items.push(
            PickerItem::option(summary.id.clone(), summary.title)
                .detail(format!(
                    "{} · сообщений: {}",
                    sessions::ago(summary.updated_ms),
                    summary.messages
                ))
                .current(summary.id == current.id),
        );
    }
    if items.is_empty() {
        items.push(PickerItem::note(
            "Сохранённых сессий пока нет — они появятся после первого сообщения",
            Style::new().fg(Color::DarkGray),
        ));
    }
    items
}

/// What the transcript paints: the session plus the reply in flight.
struct TranscriptView<'a> {
    messages: &'a [ChatMessage],
    /// Text streamed so far, `None` when nothing is in flight.
    pending: Option<&'a Reply>,
    /// The selected model's average generation rate (`N tok/s`).
    rate: Option<u64>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one frame reads and writes every piece of loop state it paints"
)]
fn draw(
    f: &mut Frame,
    composer: &mut Composer,
    view: &TranscriptView,
    screen_selection: Option<ScreenSelection>,
    screen_lines: &mut ScreenCells,
    frame_view: &mut Option<FrameView>,
    scroll: &mut ScrollState,
    palette: &mut CommandPalette,
    mut picker: Option<&mut Picker>,
    notice: Option<&CopyNotice>,
    settings: &Settings,
) {
    let composer_scroll = &mut scroll.composer;
    let scrollbars = settings.flag(SCROLLBARS);
    let message_style = if settings.flag(MESSAGE_BACKGROUND) {
        MESSAGE_STYLE
    } else {
        Style::new()
    };
    let area = f.area();
    let width = area.width as usize;
    let dim = Style::new().fg(Color::DarkGray);

    // Command suggestions sit between the top rule and the composer, like
    // rho's `commands` band. They never squeeze out the transcript's last
    // row, the rules or the composer's first row.
    palette.clamp(composer.text(), composer.cursor());
    let (mut palette_lines, palette_start) = palette
        .visible_matches(composer.text(), composer.cursor())
        .filter(|_| picker.is_none())
        .map_or((Vec::new(), 0), |matches| {
            palette.lines(&matches, width, settings.accent())
        });
    palette_lines.truncate((area.height as usize).saturating_sub(5));
    let palette_height = palette_lines.len();
    // `/usage` stretches to fill the screen above the composer — the
    // input field, its rules and the status line stay on screen; other
    // open lists sit above the top rule and take only the rows they need.
    let fullscreen_picker = picker
        .as_ref()
        .is_some_and(|open| open.purpose == PickerPurpose::Usage);
    let picker_height = if fullscreen_picker {
        0
    } else {
        picker
            .as_ref()
            .map_or(0, |open| open.height().min(area.height.saturating_sub(5)))
    };

    // Composer geometry: the prompt prefix leaves the rest of the row for
    // text. The field grows with its text up to `composer_max_rows` (and
    // never past the rows the rules leave), then scrolls; the
    // scrollbar then takes the last column behind a blank one, so the text
    // rewraps two columns narrower.
    let max_rows = settings
        .composer_rows()
        .unwrap_or_else(|| composer_max_rows(area.height as usize))
        .min((area.height as usize).saturating_sub(3 + palette_height + picker_height as usize))
        .max(1);
    let full_width = width.saturating_sub(prompt_width()).max(1);
    let overflows = |content_width| {
        let rows = editable_input_visual_lines(composer.text(), content_width).len();
        scrollbars && scrollbar::should_show(rows, max_rows)
    };
    let content_width = if overflows(full_width) {
        full_width.saturating_sub(SCROLLBAR_GUTTER).max(1)
    } else {
        full_width
    };
    composer.retain_wrap_width(content_width);
    let visual_lines = editable_input_visual_lines(composer.text(), content_width);
    let (caret_row, caret_column) =
        visual_caret_position(&visual_lines, composer.text(), composer.cursor());
    let composer_height = visual_lines.len().min(max_rows);
    // `/usage` gets every row the transcript would have taken.
    let picker_area_height = if fullscreen_picker {
        area.height
            .saturating_sub(3 + palette_height as u16 + composer_height as u16)
    } else {
        picker_height
    };
    let view_start = if composer_scroll.follow_caret {
        visible_composer_start(
            caret_row,
            visual_lines.len(),
            composer_height,
            composer_scroll.start,
        )
    } else {
        composer_scroll
            .start
            .min(visual_lines.len().saturating_sub(composer_height))
    };
    composer_scroll.start = view_start;

    let [
        transcript_area,
        picker_area,
        top_rule,
        palette_area,
        composer_area,
        bottom_rule,
        status_area,
    ] = Layout::vertical([
        Constraint::Min(if fullscreen_picker { 0 } else { 1 }),
        Constraint::Length(picker_area_height),
        Constraint::Length(1),
        Constraint::Length(palette_height as u16),
        Constraint::Length(composer_height as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let composer_scrollbar =
        Scrollbar::new(composer_area, visual_lines.len(), view_start).filter(|_| scrollbars);

    // Transcript: submitted messages on their gray blocks. Like the composer
    // it gets a scrollbar in its last column once it overflows, and the
    // messages rewrap two columns narrower: the bar plus a blank gap.
    let transcript_height = transcript_area.height as usize;
    let full_transcript_width = (transcript_area.width as usize).max(1);
    let mut transcript_lines = transcript_lines_for(view, full_transcript_width, message_style);
    if scrollbars && scrollbar::should_show(transcript_lines.len(), transcript_height) {
        transcript_lines = transcript_lines_for(
            view,
            full_transcript_width
                .saturating_sub(SCROLLBAR_GUTTER)
                .max(1),
            message_style,
        );
    }
    let max_transcript_start = transcript_lines.len().saturating_sub(transcript_height);
    let transcript_start = match scroll.transcript_top {
        Some(top) if top < max_transcript_start => top,
        // Scrolled back down to the last screenful: follow new messages.
        _ => {
            scroll.transcript_top = None;
            max_transcript_start
        }
    };
    let transcript_scrollbar =
        Scrollbar::new(transcript_area, transcript_lines.len(), transcript_start)
            .filter(|_| scrollbars);
    let transcript: Vec<Line> = transcript_lines
        .into_iter()
        .skip(transcript_start)
        .take(transcript_height)
        .collect();
    f.render_widget(Paragraph::new(transcript), transcript_area);
    if let Some(scrollbar) = transcript_scrollbar {
        scrollbar.render(f.buffer_mut(), scroll.dragging(ScrollTarget::Transcript));
    }

    *frame_view = Some(FrameView {
        transcript: transcript_area,
        composer: composer_area,
        palette: palette_area,
        palette_start,
        view_start,
        content_width,
        composer_scrollbar,
        transcript_start,
        transcript_scrollbar,
    });

    // Divider rules frame the composer like rho's labeled rules with no
    // captions, in the «Акцентный цвет» ink.
    let rule_style = Style::new().fg(settings.accent());
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(top_rule.width as usize),
            rule_style,
        )),
        top_rule,
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(bottom_rule.width as usize),
            rule_style,
        )),
        bottom_rule,
    );
    f.render_widget(Paragraph::new(palette_lines), palette_area);
    if let Some(open) = picker.as_deref_mut() {
        open.render(picker_area, f.buffer_mut(), settings.accent());
    }
    render_status(f, status_area, view, settings);

    // Composer rows: `❭ ` on the first row, continuation rows indented, the
    // Shift+arrows selection painted inverted.
    let prompt_pad = " ".repeat(prompt_width());
    let line_ranges = editable_input_visual_line_ranges(composer.text(), content_width);
    let selection = composer
        .selection_range()
        .map(|range| (range.start, range.end));
    let composer_lines: Vec<Line> = visual_lines
        .iter()
        .enumerate()
        .skip(view_start)
        .take(composer_height)
        .map(|(index, text)| {
            let mut spans = vec![if index == 0 {
                Span::styled(PROMPT_PREFIX, dim)
            } else {
                Span::raw(prompt_pad.clone())
            }];
            // Slice the row's text at the selected char range it overlaps.
            let line_start = composer.text()[..line_ranges[index].start].chars().count();
            let line_end = line_start + text.chars().count();
            let (mark_start, mark_end) = selection.map_or((line_end, line_end), |range| {
                (
                    range.0.clamp(line_start, line_end),
                    range.1.clamp(line_start, line_end),
                )
            });
            if mark_start < mark_end {
                let start_byte = char_byte_offset(text, mark_start - line_start);
                let end_byte = char_byte_offset(text, mark_end - line_start);
                spans.push(Span::raw(text[..start_byte].to_string()));
                spans.push(Span::styled(
                    text[start_byte..end_byte].to_string(),
                    SELECTION_STYLE,
                ));
                spans.push(Span::raw(text[end_byte..].to_string()));
            } else {
                spans.push(Span::raw(text.clone()));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(composer_lines), composer_area);
    if let Some(scrollbar) = composer_scrollbar {
        scrollbar.render(f.buffer_mut(), scroll.dragging(ScrollTarget::Composer));
    }

    // The mouse-drag screen selection inverts covered transcript cells only
    // — the rules and prefix stay unpainted — then the rows are
    // snapshotted so Ctrl+C can read the selection back.
    // Like the composer's Shift+arrows selection, only text is inverted:
    // each row's highlight stops at its last non-blank cell.
    let text_columns = frame_view
        .as_ref()
        .map_or(0..0, FrameView::transcript_text_columns);
    let buffer = f.buffer_mut();
    if let Some(selection) = screen_selection {
        for row in transcript_area.y..transcript_area.bottom() {
            let Some(columns) = selection.columns(row, text_columns.clone()) else {
                continue;
            };
            let text_end = columns
                .clone()
                .rev()
                .find(|&column| !buffer[(column, row)].symbol().trim().is_empty())
                .map_or(columns.start, |column| column + 1);
            for column in columns.start..text_end {
                buffer[(column, row)].set_style(SELECTION_STYLE);
            }
        }
    }
    *screen_lines = (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buffer[(column, row)].symbol().to_string())
                .collect()
        })
        .collect();

    // The copy notice floats over the top-right corner of the frame, like
    // rho's popup; it renders last so it sits above everything.
    if let Some(notice) = notice {
        render_copy_notice(f, area, notice, Instant::now());
    }

    // The real terminal caret rides the text, clamped to the last text
    // column like rho clamps the composer cursor; scrolled out of view it is
    // hidden rather than pinned to an edge row.
    if (picker.is_none() || fullscreen_picker)
        && composer_area.height > 0
        && (view_start..view_start + composer_height).contains(&caret_row)
    {
        f.set_cursor_position(Position {
            x: composer_area.x.saturating_add(
                (caret_column + prompt_width()).min(prompt_width() + content_width - 1) as u16,
            ),
            y: composer_area
                .y
                .saturating_add((caret_row - view_start) as u16),
        });
    }
}

/// Background of a submitted message, like rho's `Theme::user_message`
/// block: rho blends 10% white over the terminal background, which on a
/// dark VS Code theme lands about here. The text on it is white.
const MESSAGE_STYLE: Style = Style::new()
    .fg(Color::White)
    .bg(Color::Rgb(0x34, 0x34, 0x34));

/// Blank cells inside a message block on each side of its text.
const MESSAGE_PADDING: usize = 1;

/// Transcript rows for `view` at `width`, a blank row between messages.
/// User messages are wrapped on whitespace and padded to the full width on
/// their gray block, like rho's `LineFill::PadToWidth`, with
/// [`MESSAGE_PADDING`] blank cells before and after the text; `user_style`
/// is that block's ink ([`MESSAGE_STYLE`], or plain with «Фон сообщений»
/// off). Model replies are plain text at the same indent, failures red, and
/// the reply in flight ends in a spinner.
fn transcript_lines_for(
    view: &TranscriptView,
    width: usize,
    user_style: Style,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for message in view.messages {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        if message.role != Role::User {
            let thought = message.thought_ms.map(Duration::from_millis);
            push_reasoning(&mut lines, &message.thinking, thought, width);
            if message.text.is_empty() && message.role == Role::Assistant {
                // The reasoning block owns the trailing spacer.
                lines.pop_if(|line| line.spans.is_empty());
            }
        }
        match message.role {
            Role::User => push_text(&mut lines, &message.text, width, user_style, true),
            Role::Assistant => push_text(&mut lines, &message.text, width, Style::new(), false),
            Role::Error => push_text(
                &mut lines,
                &format!("✕ {}", message.text),
                width,
                Style::new().fg(Color::Red),
                false,
            ),
        }
    }
    if let Some(pending) = view.pending {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        if pending.text.is_empty() && pending.thinking.trim().is_empty() {
            // rho's `Thinking...` placeholder until the first delta lands.
            // Devin's usage ticks count the reasoning tokens meanwhile.
            let status = if pending.tokens > 0 {
                format!("Думает… {} ток.", pending.tokens)
            } else {
                String::from("Думает...")
            };
            push_text(&mut lines, &status, width, THINKING_STYLE, false);
        } else {
            push_reasoning(&mut lines, &pending.thinking, pending.thought, width);
            if pending.text.is_empty() {
                lines.pop_if(|line| line.spans.is_empty());
            } else {
                push_text(&mut lines, &pending.text, width, Style::new(), false);
            }
            // A silent stretch after landed content is the next
            // reasoning block warming up (Devin).
            if pending.busy_tokens > 0 {
                lines.push(Line::default());
                push_text(
                    &mut lines,
                    &format!("Думает… {} ток.", pending.busy_tokens),
                    width,
                    THINKING_STYLE,
                    false,
                );
            }
        }
    }
    lines
}

/// Reasoning text, rho's `Theme::reasoning_output`: plain dim ink.
const THINKING_STYLE: Style = Style::new().fg(Color::DarkGray);
/// The `Думал …` receipt, rho's dim summary (`Theme::dim()` + `DIM`).
const RECEIPT_STYLE: Style = Style::new()
    .fg(Color::DarkGray)
    .add_modifier(ratatui::style::Modifier::DIM);

/// rho's reasoning entry: the reasoning text, then — once the stretch
/// closed — a blank row and the `Думал 1.5 с` receipt, then the spacer
/// blank before the answer. Nothing when the model did not reason.
fn push_reasoning(
    lines: &mut Vec<Line<'static>>,
    thinking: &str,
    thought: Option<Duration>,
    width: usize,
) {
    let thinking = thinking.trim();
    if thinking.is_empty() && thought.is_none() {
        return;
    }
    if !thinking.is_empty() {
        push_text(lines, thinking, width, THINKING_STYLE, false);
    }
    if let Some(thought) = thought {
        if !thinking.is_empty() {
            lines.push(Line::default());
        }
        push_text(
            lines,
            &thought_summary(thought),
            width,
            RECEIPT_STYLE,
            false,
        );
    }
    lines.push(Line::default());
}

/// `Думал 9.0 с` under a minute, whole units from there (rho's
/// `thought_summary` / `format_elapsed`).
fn thought_summary(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let time = if secs < 60 {
        format!("{:.1} с", elapsed.as_secs_f64())
    } else if secs < 3600 {
        format!("{} мин {} с", secs / 60, secs % 60)
    } else {
        format!("{} ч {} мин", secs / 3600, secs % 3600 / 60)
    };
    format!("Думал {time}")
}

/// `text` wrapped into `lines` at the message indent; `fill` pads every row
/// to `width` so a background covers the whole block.
fn push_text(lines: &mut Vec<Line<'static>>, text: &str, width: usize, style: Style, fill: bool) {
    let text_width = width.saturating_sub(2 * MESSAGE_PADDING).max(1);
    for raw_line in text.split('\n') {
        for range in wrap_line_at_whitespace_ranges(raw_line, text_width) {
            let mut row = " ".repeat(MESSAGE_PADDING);
            row.push_str(&raw_line[range]);
            if fill {
                // Pad to the full width; the last MESSAGE_PADDING cells are
                // always blank because the text wraps that much narrower.
                let pad = width.saturating_sub(display_width(&row));
                row.extend(std::iter::repeat_n(' ', pad));
            }
            lines.push(Line::from(Span::styled(row, style)));
        }
    }
}

/// The row under the composer: the selected model and its effort on the
/// right, its average `tok/s` on the left.
fn render_status(f: &mut Frame, area: Rect, view: &TranscriptView, settings: &Settings) {
    if area.height == 0 {
        return;
    }
    let dim = Style::new().fg(Color::DarkGray);
    let right: Vec<Span> = match &settings.model {
        Some(model) => vec![
            Span::styled(model.name.clone(), Style::new().fg(Color::Gray)),
            Span::styled(format!(" · {} ", effort_label(settings.effort)), dim),
        ],
        None => vec![Span::styled("модель не выбрана · /models ", dim)],
    };
    let right_width: usize = right.iter().map(|span| display_width(&span.content)).sum();
    let right_width = (right_width as u16).min(area.width);
    f.render_widget(
        Paragraph::new(Line::from(right)).alignment(Alignment::Right),
        Rect::new(area.right() - right_width, area.y, right_width, 1),
    );
    // rho's statusline rate field: the model's average generation rate.
    if let Some(rate) = view.rate {
        f.render_widget(
            Paragraph::new(Line::styled(format!(" {rate} tok/s"), dim)),
            Rect::new(
                area.x,
                area.y,
                area.width.saturating_sub(right_width + 1),
                1,
            ),
        );
    }
}

/// Byte offset of char `char_index` in `text`, or `text.len()` past the end.
fn char_byte_offset(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// First painted composer row, ported from rho's `visible_composer_start`:
/// the view holds still while the caret row stays inside it and scrolls
/// just enough to bring the caret back when it leaves.
fn visible_composer_start(
    cursor_line: usize,
    line_count: usize,
    visible_count: usize,
    current_start: usize,
) -> usize {
    if visible_count == 0 || visible_count >= line_count {
        return 0;
    }
    let max_start = line_count.saturating_sub(visible_count);
    let current_start = current_start.min(max_start);
    if cursor_line < current_start {
        cursor_line
    } else if cursor_line >= current_start.saturating_add(visible_count) {
        cursor_line
            .saturating_add(1)
            .saturating_sub(visible_count)
            .min(max_start)
    } else {
        current_start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(rows: &[&str]) -> ScreenCells {
        rows.iter()
            .map(|row| row.chars().map(String::from).collect())
            .collect()
    }

    fn view() -> FrameView {
        FrameView {
            transcript: Rect::new(0, 0, 20, 4),
            composer: Rect::new(0, 5, 20, 1),
            palette: Rect::new(0, 5, 20, 0),
            palette_start: 0,
            view_start: 0,
            content_width: 18,
            composer_scrollbar: None,
            transcript_start: 0,
            transcript_scrollbar: None,
        }
    }

    #[test]
    fn messages_fill_the_width_on_gray_with_a_blank_row_between() {
        let messages = [ChatMessage::user("ab cd"), ChatMessage::user("x\ny")];
        let view = TranscriptView {
            messages: &messages,
            pending: None,
            rate: None,
        };
        let lines = transcript_lines_for(&view, 6, MESSAGE_STYLE);
        let rows: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        // One blank cell before and after the text inside the gray block.
        assert_eq!(rows, [" ab   ", " cd   ", "", " x    ", " y    "]);
        assert_eq!(lines[0].spans[0].style, MESSAGE_STYLE);
        assert!(lines[2].spans.is_empty());
    }

    #[test]
    fn reasoning_shows_dark_gray_above_the_answer() {
        let messages = [ChatMessage::reply(Role::Assistant, "ok", "M")
            .with_thinking("hm\n")
            .with_thought(Some(Duration::from_millis(1500)))];
        // Still reasoning: the text streams, no receipt yet.
        let streaming = Reply {
            thinking: "step".into(),
            ..Reply::default()
        };
        let view = TranscriptView {
            messages: &messages,
            pending: Some(&streaming),
            rate: None,
        };
        let lines = transcript_lines_for(&view, 20, MESSAGE_STYLE);
        let rows: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert_eq!(rows, [" hm", "", " Думал 1.5 с", "", " ok", "", " step"]);
        assert_eq!(lines[0].spans[0].style, THINKING_STYLE);
        assert_eq!(lines[2].spans[0].style, RECEIPT_STYLE);
        assert_eq!(lines[4].spans[0].style, Style::new());
        assert_eq!(lines[6].spans[0].style, THINKING_STYLE);
        assert_eq!(thought_summary(Duration::from_secs(125)), "Думал 2 мин 5 с");
    }

    #[test]
    fn replies_are_plain_errors_red_and_the_reply_in_flight_spins() {
        let messages = [
            ChatMessage::user("hi"),
            ChatMessage::reply(Role::Assistant, "hello", "M"),
            ChatMessage::reply(Role::Error, "boom", "M"),
        ];
        let view = TranscriptView {
            messages: &messages,
            pending: Some(&Reply::default()),
            rate: None,
        };
        let lines = transcript_lines_for(&view, 14, MESSAGE_STYLE);
        let rows: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert_eq!(
            rows,
            [
                " hi           ",
                "",
                " hello",
                "",
                " ✕ boom",
                "",
                " Думает..."
            ]
        );
        assert_eq!(lines[2].spans[0].style, Style::new());
        assert_eq!(lines[4].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn the_pending_placeholder_counts_reasoning_tokens() {
        // Devin ticks usage while it reasons and before any text lands.
        let streaming = Reply {
            tokens: 28,
            ..Reply::default()
        };
        let view = TranscriptView {
            messages: &[],
            pending: Some(&streaming),
            rate: None,
        };
        let lines = transcript_lines_for(&view, 40, MESSAGE_STYLE);
        let rows: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert_eq!(rows, [" Думает… 28 ток."]);
    }

    #[test]
    fn a_later_silent_stretch_shows_the_counter_too() {
        // Devin reasoning block №2: content already landed, then the
        // usage ticks run silent again — the counter resurfaces.
        let streaming = Reply {
            text: "half".into(),
            tokens: 90,
            busy_tokens: 34,
            ..Reply::default()
        };
        let view = TranscriptView {
            messages: &[],
            pending: Some(&streaming),
            rate: None,
        };
        let lines = transcript_lines_for(&view, 40, MESSAGE_STYLE);
        let rows: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert_eq!(rows, [" half", "", " Думает… 34 ток."]);
        assert_eq!(lines[2].spans[0].style, THINKING_STYLE);
    }

    #[test]
    fn history_drops_failures() {
        let messages = [
            ChatMessage::user("hi"),
            ChatMessage::reply(Role::Assistant, "hello", "M"),
            ChatMessage::reply(Role::Error, "boom", "M"),
        ];
        let mut session = Session::new();
        for message in messages {
            session.push(message);
        }
        assert_eq!(chat_history(&session).len(), 2);
    }

    fn option_rows(items: &[PickerItem]) -> Vec<(&str, &PickerItem)> {
        items
            .iter()
            .filter(|item| item.kind == crate::picker::ItemKind::Option)
            .map(|item| (item.value.as_str(), item))
            .collect()
    }

    #[test]
    fn effort_list_shows_only_the_levels_of_the_model() {
        use crate::picker::ItemKind;
        // A Devin family without «medium», «max» or a no-reasoning
        // variant (GPT-5.6): exactly its levels, the family default marked.
        let gpt = ModelInfo {
            efforts: Some(vec![Effort::Low, Effort::High, Effort::XHigh]),
            effort_off: false,
            default_effort: Some(Effort::High),
            ..Default::default()
        };
        let mut settings = Settings::default();
        settings.effort = Some(Effort::High);
        let items = effort_options(&gpt, "GPT-5.6", &settings);
        assert_eq!(items[0].kind, ItemKind::Heading);
        let options = option_rows(&items);
        assert_eq!(
            options.iter().map(|(value, _)| *value).collect::<Vec<_>>(),
            ["low", "high", "xhigh"]
        );
        assert!(options[1].1.current);
        assert!(options[1].1.detail.contains("по умолчанию"));

        // A model without its own list gets the standard one plus «выкл».
        let items = effort_options(&ModelInfo::default(), "Claude", &settings);
        let values: Vec<&str> = option_rows(&items)
            .into_iter()
            .map(|(value, _)| value)
            .collect();
        assert_eq!(values, ["off", "low", "medium", "high", "max"]);

        // A single Devin config can only turn reasoning off.
        let single = ModelInfo {
            efforts: Some(Vec::new()),
            ..Default::default()
        };
        let items = effort_options(&single, "SWE-1.7", &settings);
        assert_eq!(option_rows(&items).len(), 1);
        assert_eq!(option_rows(&items)[0].0, "off");
    }

    #[test]
    fn usage_lists_every_providers_limits() {
        use crate::picker::ItemKind;
        use dd_providers::{AccountInfo, Quota};

        // Devin and Claude signed in, OpenRouter not; Claude's account is
        // still loading (Idle).
        let mut entries: Vec<ProviderEntry> = ProviderKind::ALL
            .iter()
            .map(|&kind| ProviderEntry::new(kind, kind != ProviderKind::OpenRouter))
            .collect();
        // Devin signed in with a ready account: its rows become line items.
        entries[1].account = Load::Ready(AccountInfo {
            provider: ProviderKind::Devin,
            label: Some("devin@work".into()),
            plan: Some("Pro".into()),
            expires_at: None,
            started_at: None,
            balance_usd: None,
            spent_usd: None,
            credit_limit_usd: None,
            quotas: vec![Quota {
                name: "daily".into(),
                utilization: Some(71.0),
                ..Default::default()
            }],
        });

        let items = usage_items(&entries);
        let headings: Vec<&str> = items
            .iter()
            .filter(|item| item.kind == ItemKind::Heading)
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(headings, ["OpenRouter", "Devin", "Claude"]);
        // Signed out / still loading providers get notes, not rows.
        let notes: Vec<&str> = items
            .iter()
            .filter(|item| item.kind == ItemKind::Note)
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(notes, ["вход не выполнен", "загрузка…"]);
        // The signed-in provider's rows are line items: label + bar.
        let lines: Vec<String> = items
            .iter()
            .filter_map(|item| item.line.as_ref().map(ToString::to_string))
            .collect();
        assert!(lines.iter().any(|line| line.contains("devin@work")));
        assert!(lines.iter().any(|line| line.contains("███████░░░ 71%")));

        // Nobody signed in: one hint instead of per-provider headings.
        let entries: Vec<ProviderEntry> = ProviderKind::ALL
            .iter()
            .map(|&kind| ProviderEntry::new(kind, false))
            .collect();
        let items = usage_items(&entries);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ItemKind::Note);
        assert!(items[0].label.contains("Нет авторизованных провайдеров"));
    }

    #[test]
    fn composer_cap_grows_with_the_screen_from_two_to_eight_rows() {
        assert_eq!(composer_max_rows(4), 2);
        assert_eq!(composer_max_rows(12), 3);
        assert_eq!(composer_max_rows(30), 7);
        assert_eq!(composer_max_rows(32), 8);
        assert_eq!(composer_max_rows(200), 8);
    }

    #[test]
    fn composer_view_holds_still_until_the_caret_leaves_it() {
        // 20 rows, 8 visible, view at 5: caret inside keeps it.
        assert_eq!(visible_composer_start(9, 20, 8, 5), 5);
        // Caret above the view: it becomes the top row.
        assert_eq!(visible_composer_start(2, 20, 8, 5), 2);
        // Caret below: just enough scroll to show it as the bottom row.
        assert_eq!(visible_composer_start(15, 20, 8, 5), 8);
        // Everything fits: no scroll.
        assert_eq!(visible_composer_start(3, 6, 8, 5), 0);
    }

    #[test]
    fn screen_copy_flows_like_a_shift_selection() {
        // Column 0 is the gray block's padding cell.
        let lines = cells(&[" abcdefg", " ijklmno", " qrstuvw"]);
        let drag = ScreenSelection {
            anchor: (3, 0),
            focus: (2, 2),
        };
        assert_eq!(
            selected_screen_text(&lines, view(), drag).as_deref(),
            Some("cdefg\nijklmno\nqr")
        );
        // Dragging backwards selects the same text.
        let reversed = ScreenSelection {
            anchor: (2, 2),
            focus: (3, 0),
        };
        assert_eq!(
            selected_screen_text(&lines, view(), reversed).as_deref(),
            Some("cdefg\nijklmno\nqr")
        );
    }

    #[test]
    fn screen_copy_on_one_row_and_past_short_rows() {
        let lines = cells(&[" ab      ", "", " abcdef  "]);
        let one_row = ScreenSelection {
            anchor: (4, 2),
            focus: (2, 2),
        };
        assert_eq!(
            selected_screen_text(&lines, view(), one_row).as_deref(),
            Some("bcd")
        );
        let drag = ScreenSelection {
            anchor: (0, 0),
            focus: (8, 2),
        };
        assert_eq!(
            selected_screen_text(&lines, view(), drag).as_deref(),
            Some("ab\n\nabcdef")
        );
    }
}
