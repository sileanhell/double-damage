//! Slash commands and their suggestion palette, ported from rho's
//! `commands.rs` and `tui/command_palette.rs`: typing `/` in the composer
//! lists matching commands above the field; Up/Down pick a row, Tab
//! completes it, Enter completes it and submits, Esc closes the list.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::composer::display_width;

/// One slash command: its name without the `/`, the usage line the palette
/// shows, and what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    pub(crate) name: &'static str,
    pub(crate) usage: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) const EXIT_COMMAND: &str = "/exit";
/// Both open the settings menu, like oh-my-pi's `/settings`.
pub(crate) const SETTINGS_COMMANDS: &[&str] = &["/config", "/settings"];
pub(crate) const MODELS_COMMAND: &str = "/models";
/// The reasoning-effort list under its three names.
pub(crate) const EFFORT_COMMANDS: &[&str] = &["/effort", "/reasoning", "/thinking"];
pub(crate) const SESSIONS_COMMAND: &str = "/sessions";
pub(crate) const NEW_COMMAND: &str = "/new";
/// Both open the provider limits list.
pub(crate) const USAGE_COMMANDS: &[&str] = &["/usage", "/limits"];

/// Every command, kept alphabetical by name; filtering preserves the order.
pub(crate) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "config",
        usage: "/config",
        description: "открыть меню настроек",
    },
    CommandSpec {
        name: "effort",
        usage: "/effort",
        description: "выбрать уровень рассуждений модели",
    },
    CommandSpec {
        name: "exit",
        usage: "/exit",
        description: "выйти из терминала",
    },
    CommandSpec {
        name: "limits",
        usage: "/limits",
        description: "лимиты провайдеров (псевдоним /usage)",
    },
    CommandSpec {
        name: "models",
        usage: "/models",
        description: "выбрать модель для чата",
    },
    CommandSpec {
        name: "new",
        usage: "/new",
        description: "начать новую сессию",
    },
    CommandSpec {
        name: "reasoning",
        usage: "/reasoning",
        description: "уровень рассуждений (псевдоним /effort)",
    },
    CommandSpec {
        name: "sessions",
        usage: "/sessions",
        description: "история сессий, свежие сверху",
    },
    CommandSpec {
        name: "settings",
        usage: "/settings",
        description: "открыть меню настроек (псевдоним /config)",
    },
    CommandSpec {
        name: "thinking",
        usage: "/thinking",
        description: "уровень рассуждений (псевдоним /effort)",
    },
    CommandSpec {
        name: "usage",
        usage: "/usage",
        description: "лимиты и баланс каждого провайдера",
    },
];

/// Most palette rows shown at once, like rho's `MAX_COMMAND_SUGGESTIONS`;
/// the list scrolls to keep the selection in view.
pub(crate) const MAX_COMMAND_SUGGESTIONS: usize = 5;
/// Narrowest description column before the usage column gives way.
const MIN_COMMAND_DESCRIPTION_WIDTH: usize = 7;
/// Between a command and its description: a long dash, e.g. `/exit — выйти`.
const COMMAND_DESCRIPTION_SEPARATOR: &str = " — ";
/// Cells a row spends besides usage and description: the `> ` marker and
/// [`COMMAND_DESCRIPTION_SEPARATOR`].
const ROW_CHROME_WIDTH: usize = 5;

/// The command token being typed without its `/`, or `None` when the text
/// is not a slash command (`//` escapes one, like in rho).
pub(crate) fn command_prefix(input: &str) -> Option<&str> {
    let token_end = input
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .unwrap_or(input.len());
    let prefix = input[..token_end].strip_prefix('/')?;
    (!prefix.starts_with('/')).then_some(prefix)
}

/// Commands whose name starts with `prefix`, case-insensitively.
pub(crate) fn matching_commands(prefix: &str) -> Vec<&'static CommandSpec> {
    let prefix = prefix.strip_prefix('/').unwrap_or(prefix).to_lowercase();
    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(&prefix))
        .collect()
}

/// Whether the caret (a char index) sits inside the leading `/command`
/// token, where the palette answers.
pub(crate) fn cursor_in_command_token(input: &str, cursor: usize) -> bool {
    if !input.starts_with('/') {
        return false;
    }
    let token_len = input
        .chars()
        .position(char::is_whitespace)
        .unwrap_or_else(|| input.chars().count());
    cursor <= token_len
}

/// `input` with its command token replaced by `/name`, any arguments kept,
/// and the caret's new char index: at the token's end while the caret was in
/// the token, else shifted along with the arguments. Ported from rho's
/// `complete_slash_command`.
pub(crate) fn complete_command(input: &str, cursor: usize, name: &str) -> (String, usize) {
    let token_end = input
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .unwrap_or(input.len());
    let token_len = input[..token_end].chars().count();
    let args = input[token_end..].trim_start();
    let completed = if args.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {args}")
    };
    let completed_token_len = name.chars().count() + 1;
    let new_cursor = if cursor <= token_len {
        completed_token_len
    } else {
        completed
            .chars()
            .count()
            .min(completed_token_len.saturating_add(cursor.saturating_sub(token_len)))
    };
    (completed, new_cursor)
}

/// First visible row of a palette window: it holds still while the
/// selection is inside and scrolls just enough to reveal it, like rho's
/// `file_palette_scroll_counts`.
fn window_start(match_count: usize, selected: usize, visible: usize, previous: usize) -> usize {
    if match_count == 0 || visible == 0 {
        return 0;
    }
    let selected = selected.min(match_count - 1);
    if selected < previous {
        selected
    } else if selected >= previous.saturating_add(visible) {
        selected + 1 - visible
    } else {
        previous
    }
    .min(match_count.saturating_sub(visible))
}

/// What a palette key did, like rho's `CommandPaletteKeyOutcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaletteKey {
    Up,
    Down,
    Tab,
    Enter,
    Esc,
}

/// Palette state the composer keeps between keys and frames.
#[derive(Debug, Default)]
pub(crate) struct CommandPalette {
    /// Highlighted row, an index into the current matches.
    selection: usize,
    /// First painted row of the scrolled window.
    window_start: usize,
    /// Esc or a history recall closed the list until the next edit.
    dismissed: bool,
    /// The command prefix the selection was made for; a new prefix starts
    /// the selection over.
    prefix: Option<String>,
    /// The «Подсказки команд» setting is off: the list never shows.
    hidden: bool,
}

impl CommandPalette {
    /// Matches to show for `text` with the caret at char `cursor`, or `None`
    /// when the palette is hidden: dismissed, caret past the command token,
    /// or nothing matches.
    pub(crate) fn visible_matches(
        &self,
        text: &str,
        cursor: usize,
    ) -> Option<Vec<&'static CommandSpec>> {
        if self.hidden || self.dismissed || !cursor_in_command_token(text, cursor) {
            return None;
        }
        let matches = matching_commands(command_prefix(text)?);
        (!matches.is_empty()).then_some(matches)
    }

    #[cfg(test)]
    fn selection(&self) -> usize {
        self.selection
    }

    /// The highlighted command among `matches`.
    pub(crate) fn selected<'a>(&self, matches: &[&'a CommandSpec]) -> Option<&'a CommandSpec> {
        matches
            .get(self.selection.min(matches.len().saturating_sub(1)))
            .copied()
    }

    /// Move the highlight one row, wrapping at either end.
    pub(crate) fn step(&mut self, match_count: usize, up: bool) {
        if match_count == 0 {
            return;
        }
        let current = self.selection.min(match_count - 1);
        self.selection = if up {
            current.checked_sub(1).unwrap_or(match_count - 1)
        } else {
            (current + 1) % match_count
        };
    }

    /// Hide or show the list for good, following the settings.
    pub(crate) fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }

    /// Highlight row `index` directly (a pointer pick).
    pub(crate) fn select(&mut self, index: usize) {
        self.selection = index;
    }

    /// Close the list until the next edit.
    pub(crate) fn dismiss(&mut self) {
        self.dismissed = true;
        self.selection = 0;
    }

    /// The text changed: reopen the list and refit the selection.
    pub(crate) fn input_changed(&mut self, text: &str, cursor: usize) {
        self.dismissed = false;
        self.clamp(text, cursor);
    }

    /// Keep the selection meaningful for the current text: a different
    /// prefix starts it over at the first row, and it never points past the
    /// last match.
    pub(crate) fn clamp(&mut self, text: &str, cursor: usize) {
        let prefix = cursor_in_command_token(text, cursor)
            .then(|| command_prefix(text).map(str::to_lowercase))
            .flatten();
        if self.prefix != prefix {
            self.prefix = prefix;
            self.selection = 0;
        }
        let count = self
            .prefix
            .as_deref()
            .map_or(0, |prefix| matching_commands(prefix).len());
        if count == 0 {
            self.selection = 0;
        } else {
            self.selection = self.selection.min(count - 1);
        }
    }

    /// Palette rows for `matches` at `width`, scrolled to keep the selection
    /// visible, with the absolute match index of the first painted row. The
    /// layout follows rho's `command_suggestion_lines`: a `>` marker on the
    /// highlighted row, the usage column padded to the widest visible usage,
    /// then the description, truncated to fit.
    pub(crate) fn lines(
        &mut self,
        matches: &[&CommandSpec],
        width: usize,
        accent: Color,
    ) -> (Vec<Line<'static>>, usize) {
        let selected = self.selection.min(matches.len().saturating_sub(1));
        let start = window_start(
            matches.len(),
            selected,
            MAX_COMMAND_SUGGESTIONS,
            self.window_start,
        );
        self.window_start = start;
        let visible = || {
            matches
                .iter()
                .enumerate()
                .skip(start)
                .take(MAX_COMMAND_SUGGESTIONS)
        };
        let usage_width = visible()
            .map(|(_, command)| display_width(command.usage))
            .max()
            .unwrap_or(1)
            .min(
                width
                    .saturating_sub(MIN_COMMAND_DESCRIPTION_WIDTH + ROW_CHROME_WIDTH)
                    .max(1),
            );
        let lines = visible()
            .map(|(index, command)| {
                let is_selected = index == selected;
                let marker = if is_selected { ">" } else { " " };
                let usage = truncate_one_line(command.usage, usage_width);
                let text = truncate_one_line(
                    &format!(
                        "{marker} {usage}{COMMAND_DESCRIPTION_SEPARATOR}{}",
                        command.description
                    ),
                    width.max(1),
                );
                Line::from(Span::styled(text, palette_row_style(is_selected, accent)))
            })
            .collect();
        (lines, start)
    }
}

/// Palette row ink: the highlighted row stands out, the rest recede.
fn palette_row_style(selected: bool, accent: Color) -> Style {
    if selected {
        Style::new().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

/// `text` cut to `width` cells with a trailing `…` when it does not fit.
fn truncate_one_line(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + ch_width + 1 > width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Map a key to a palette key; only bare keys count, so Shift+Up still
/// selects text.
pub(crate) fn palette_key(key: ratatui::crossterm::event::KeyEvent) -> Option<PaletteKey> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    if key.modifiers != KeyModifiers::NONE {
        return None;
    }
    Some(match key.code {
        KeyCode::Up => PaletteKey::Up,
        KeyCode::Down => PaletteKey::Down,
        KeyCode::Tab => PaletteKey::Tab,
        KeyCode::Enter => PaletteKey::Enter,
        KeyCode::Esc => PaletteKey::Esc,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_reads_the_leading_slash_token() {
        assert_eq!(command_prefix("/ex"), Some("ex"));
        assert_eq!(command_prefix("/exit now"), Some("exit"));
        assert_eq!(command_prefix("/"), Some(""));
        assert_eq!(command_prefix("//ex"), None);
        assert_eq!(command_prefix("ex"), None);
    }

    #[test]
    fn matching_keeps_alphabetical_order_and_ignores_case() {
        let names = |prefix| {
            matching_commands(prefix)
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(""),
            [
                "config",
                "effort",
                "exit",
                "limits",
                "models",
                "new",
                "reasoning",
                "sessions",
                "settings",
                "thinking",
                "usage"
            ]
        );
        assert_eq!(names("s"), ["sessions", "settings"]);
        assert_eq!(names("E"), ["effort", "exit"]);
        assert!(names("zz").is_empty());
    }

    #[test]
    fn palette_shows_only_while_the_caret_is_in_the_token() {
        let palette = CommandPalette::default();
        assert!(palette.visible_matches("/e", 2).is_some());
        assert!(palette.visible_matches("/exit x", 7).is_none());
        assert!(palette.visible_matches("hello", 5).is_none());
        assert!(palette.visible_matches("/zz", 3).is_none());
    }

    #[test]
    fn dismiss_holds_until_the_next_edit() {
        let mut palette = CommandPalette::default();
        palette.dismiss();
        assert!(palette.visible_matches("/", 1).is_none());
        palette.input_changed("/e", 2);
        assert!(palette.visible_matches("/e", 2).is_some());
    }

    #[test]
    fn selection_wraps_and_restarts_on_a_new_prefix() {
        let mut palette = CommandPalette::default();
        palette.clamp("/", 1);
        palette.step(2, true);
        assert_eq!(palette.selection(), 1);
        palette.step(2, false);
        assert_eq!(palette.selection(), 0);
        palette.step(2, false);
        palette.clamp("/e", 2);
        assert_eq!(palette.selection(), 0);
    }

    #[test]
    fn completion_replaces_the_token_and_keeps_arguments() {
        assert_eq!(complete_command("/ex", 3, "exit"), ("/exit".to_string(), 5));
        assert_eq!(
            complete_command("/c  abc", 1, "config"),
            ("/config abc".to_string(), 7)
        );
    }

    #[test]
    fn rows_mark_the_selection_and_align_descriptions() {
        let mut palette = CommandPalette::default();
        let matches = matching_commands("");
        palette.step(matches.len(), false);
        let (lines, start) = palette.lines(&matches, 40, Color::Cyan);
        let rows: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(start, 0);
        assert_eq!(rows[0], "  /config — открыть меню настроек");
        assert_eq!(rows[1], "> /effort — выбрать уровень рассуждений…");
        assert_eq!(rows[2], "  /exit — выйти из терминала");
    }

    #[test]
    fn truncation_adds_an_ellipsis() {
        assert_eq!(truncate_one_line("abcdef", 4), "abc…");
        assert_eq!(truncate_one_line("abc", 4), "abc");
    }
}
