//! A framed pick-one list above the composer, shared by `/models`,
//! `/effort` and `/sessions`: a rounded box titled like the settings
//! panel, headings and notes between the options, a `✓` on the current
//! value and a footer hint. `/models` filters as you type.

use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::composer::display_width;
use crate::scrollbar::Scrollbar;
use crate::settings::{CURSOR, DIM, pad, rule, truncate};

/// What the list is for, so the owner knows how to apply a pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerPurpose {
    Models,
    Effort,
    Sessions,
    /// `/usage` — a read-only limits view; picking just closes it.
    Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    Option,
    Heading,
    /// Non-selectable text: loading, errors, hints.
    Note,
    /// A styled read-only row (the account lines of `/usage`): shown,
    /// never highlighted — the list is a scrollable view, not a choice.
    Row,
}

#[derive(Clone, Debug)]
pub(crate) struct PickerItem {
    pub(crate) kind: ItemKind,
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) current: bool,
    pub(crate) style: Style,
    /// A ready-made styled row (the account lines of `/usage`), painted
    /// instead of label+detail.
    pub(crate) line: Option<Line<'static>>,
}

impl PickerItem {
    pub(crate) fn option(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: ItemKind::Option,
            value: value.into(),
            label: label.into(),
            detail: String::new(),
            current: false,
            style: Style::new(),
            line: None,
        }
    }

    /// A read-only styled row for `/usage`, painted as-is.
    pub(crate) fn line(line: Line<'static>) -> Self {
        Self {
            kind: ItemKind::Row,
            line: Some(line),
            ..Self::option("", "")
        }
    }

    pub(crate) fn heading(label: impl Into<String>) -> Self {
        Self {
            kind: ItemKind::Heading,
            ..Self::option("", label)
        }
    }

    pub(crate) fn note(label: impl Into<String>, style: Style) -> Self {
        Self {
            kind: ItemKind::Note,
            style,
            ..Self::option("", label)
        }
    }

    pub(crate) fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub(crate) fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerOutcome {
    Stay,
    Close,
    Pick(String),
    Delete(String),
}

pub(crate) struct Picker {
    pub(crate) purpose: PickerPurpose,
    title: String,
    items: Vec<PickerItem>,
    filterable: bool,
    deletable: bool,
    filter: String,
    /// Highlighted row, an index into [`Self::visible`]; on an option.
    selected: usize,
    start: usize,
    /// Rows painted by the last frame, for the mouse.
    rows_area: Rect,
}

/// Most option rows the box grows to before it scrolls.
const MAX_ROWS: usize = 14;
/// Rows one wheel notch scrolls in the `/usage` view.
const USAGE_WHEEL_ROWS: usize = 3;

impl Picker {
    pub(crate) fn new(purpose: PickerPurpose, title: impl Into<String>) -> Self {
        Self {
            purpose,
            title: title.into(),
            items: Vec::new(),
            filterable: purpose == PickerPurpose::Models,
            deletable: purpose == PickerPurpose::Sessions,
            filter: String::new(),
            selected: 0,
            start: 0,
            rows_area: Rect::default(),
        }
    }

    /// Replace the items, keeping the highlight on the same value and
    /// otherwise landing on the current one.
    pub(crate) fn set_items(&mut self, items: Vec<PickerItem>) {
        let keep = self.selected_item().map(|item| item.value.clone());
        self.items = items;
        let visible = self.visible();
        let find = |pred: &dyn Fn(&PickerItem) -> bool| {
            visible
                .iter()
                .position(|&i| self.items[i].kind == ItemKind::Option && pred(&self.items[i]))
        };
        self.selected = keep
            .and_then(|value| find(&|item| item.value == value))
            .or_else(|| find(&|item| item.current))
            .or_else(|| find(&|_| true))
            .unwrap_or(0);
    }

    /// Indices of the items shown under the filter: matching options plus
    /// the headings and notes of groups that still hold one.
    fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let query = self.filter.to_lowercase();
        let matches = |item: &PickerItem| {
            item.label.to_lowercase().contains(&query)
                || item.detail.to_lowercase().contains(&query)
        };
        let mut out = Vec::new();
        let mut heading = None;
        for (index, item) in self.items.iter().enumerate() {
            match item.kind {
                ItemKind::Heading => heading = Some(index),
                ItemKind::Note | ItemKind::Row => {}
                ItemKind::Option if matches(item) => {
                    if let Some(h) = heading.take() {
                        out.push(h);
                    }
                    out.push(index);
                }
                ItemKind::Option => {}
            }
        }
        out
    }

    fn selected_item(&self) -> Option<&PickerItem> {
        let visible = self.visible();
        visible
            .get(self.selected)
            .map(|&i| &self.items[i])
            .filter(|item| item.kind == ItemKind::Option)
    }

    fn step(&mut self, forward: bool) {
        let visible = self.visible();
        let options: Vec<usize> = visible
            .iter()
            .enumerate()
            .filter_map(|(row, &i)| (self.items[i].kind == ItemKind::Option).then_some(row))
            .collect();
        if options.is_empty() {
            return;
        }
        let current = options.iter().position(|&row| row == self.selected);
        let next = match (current, forward) {
            (None, _) => 0,
            (Some(at), true) => (at + 1) % options.len(),
            (Some(at), false) => at.checked_sub(1).unwrap_or(options.len() - 1),
        };
        self.selected = options[next];
    }

    fn refilter(&mut self) {
        self.selected = 0;
        self.start = 0;
        if self.selected_item().is_none() {
            self.step(true);
        }
    }

    fn pick(&self) -> PickerOutcome {
        self.selected_item().map_or(PickerOutcome::Stay, |item| {
            PickerOutcome::Pick(item.value.clone())
        })
    }

    /// Scroll the viewport `rows` lines — the `/usage` view has no
    /// selection to move, so keys and the wheel shift `start` itself.
    fn scroll(&mut self, forward: bool, rows: usize) {
        let max = self
            .visible()
            .len()
            .saturating_sub(usize::from(self.rows_area.height.max(1)));
        self.start = if forward {
            (self.start + rows).min(max)
        } else {
            self.start.saturating_sub(rows)
        };
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PickerOutcome {
        if self.purpose == PickerPurpose::Usage {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => return PickerOutcome::Close,
                KeyCode::Up => self.scroll(false, 1),
                KeyCode::Down | KeyCode::Tab => self.scroll(true, 1),
                KeyCode::PageUp | KeyCode::PageDown => self.scroll(
                    key.code == KeyCode::PageDown,
                    (usize::from(self.rows_area.height) / 2).max(1),
                ),
                _ => {}
            }
            return PickerOutcome::Stay;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Esc => return PickerOutcome::Close,
            KeyCode::Up => self.step(false),
            KeyCode::Down | KeyCode::Tab => self.step(true),
            KeyCode::PageUp | KeyCode::PageDown => {
                for _ in 0..MAX_ROWS / 2 {
                    self.step(key.code == KeyCode::PageDown);
                }
            }
            KeyCode::Enter => return self.pick(),
            KeyCode::Delete if self.deletable => {
                if let Some(item) = self.selected_item() {
                    return PickerOutcome::Delete(item.value.clone());
                }
            }
            KeyCode::Backspace if self.filterable => {
                self.filter.pop();
                self.refilter();
            }
            KeyCode::Char(ch) if self.filterable && plain => {
                self.filter.push(ch);
                self.refilter();
            }
            _ => {}
        }
        PickerOutcome::Stay
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> PickerOutcome {
        if self.purpose == PickerPurpose::Usage {
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                self.scroll(
                    matches!(mouse.kind, MouseEventKind::ScrollDown),
                    USAGE_WHEEL_ROWS,
                );
            }
            return PickerOutcome::Stay;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.step(false),
            MouseEventKind::ScrollDown => self.step(true),
            MouseEventKind::Down(MouseButton::Left)
                if self.rows_area.contains((mouse.column, mouse.row).into()) =>
            {
                let row = self.start + (mouse.row - self.rows_area.y) as usize;
                let visible = self.visible();
                if let Some(&i) = visible.get(row)
                    && self.items[i].kind == ItemKind::Option
                {
                    if row == self.selected {
                        return self.pick();
                    }
                    self.selected = row;
                }
            }
            _ => {}
        }
        PickerOutcome::Stay
    }

    /// Rows the box wants: frame, filter line, items, hint.
    pub(crate) fn height(&self) -> u16 {
        let rows = self.visible().len().clamp(1, MAX_ROWS);
        let filter = usize::from(!self.filter.is_empty());
        (rows + filter + 4) as u16
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer, accent: Color) {
        self.rows_area = Rect::default();
        if area.height < 4 || area.width < 8 {
            return;
        }
        let top = area.y;
        let bottom = area.bottom() - 1;
        rule(buf, area, top, ('╭', '╮'), Some((&self.title, accent)));
        for y in top + 1..bottom {
            buf.set_string(area.x, y, "│", DIM);
            buf.set_string(area.right() - 1, y, "│", DIM);
        }
        rule(buf, area, bottom - 2, ('├', '┤'), None);
        rule(buf, area, bottom, ('╰', '╯'), None);
        let inner_width = area.width - 4;
        let inner = |y: u16, height: u16| Rect::new(area.x + 2, y, inner_width, height);

        let mut content_top = top + 1;
        if !self.filter.is_empty() {
            Paragraph::new(Line::from(vec![
                Span::styled("Фильтр: ", Style::new().fg(accent)),
                Span::styled(
                    self.filter.clone(),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::styled("▏", Style::new().fg(accent)),
            ]))
            .render(inner(content_top, 1), buf);
            content_top += 1;
        }
        let list = inner(content_top, (bottom - 2).saturating_sub(content_top));
        let visible = self.visible();
        let height = list.height as usize;
        // `/usage` scrolls freely; other lists keep the selection in view.
        if self.purpose != PickerPurpose::Usage {
            if self.selected < self.start {
                self.start = self.selected;
            } else if self.selected >= self.start + height {
                self.start = self.selected + 1 - height;
            }
        }
        self.start = self.start.min(visible.len().saturating_sub(height));
        let scrollbar = Scrollbar::new(list, visible.len(), self.start);
        let width = if scrollbar.is_some() {
            list.width.saturating_sub(2)
        } else {
            list.width
        } as usize;
        let label_width = visible
            .iter()
            .map(|&i| &self.items[i])
            .filter(|item| item.kind == ItemKind::Option)
            .map(|item| display_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(width / 2);
        let lines: Vec<Line> = if visible.is_empty() {
            vec![Line::styled("Ничего не найдено", DIM)]
        } else {
            visible
                .iter()
                .enumerate()
                .skip(self.start)
                .take(height)
                .map(|(row, &i)| {
                    let item = &self.items[i];
                    match item.kind {
                        ItemKind::Heading => Line::styled(
                            truncate(&item.label, width),
                            DIM.add_modifier(Modifier::BOLD),
                        ),
                        ItemKind::Note => {
                            Line::styled(truncate(&format!("  {}", item.label), width), item.style)
                        }
                        ItemKind::Row => row_line(item, width),
                        ItemKind::Option => {
                            option_line(item, row == self.selected, label_width, width, accent)
                        }
                    }
                })
                .collect()
        };
        let rows_area = Rect::new(list.x, list.y, width as u16, list.height);
        Paragraph::new(lines).render(rows_area, buf);
        if let Some(scrollbar) = scrollbar {
            scrollbar.render(buf, false);
        }
        self.rows_area = rows_area;

        let hint = match self.purpose {
            PickerPurpose::Models => "↑/↓ выбрать · Enter применить · набор — фильтр · Esc закрыть",
            PickerPurpose::Sessions => "↑/↓ выбрать · Enter открыть · Del удалить · Esc закрыть",
            PickerPurpose::Effort => "↑/↓ выбрать · Enter применить · Esc закрыть",
            PickerPurpose::Usage => "↑/↓ прокрутка · Enter или Esc закрыть",
        };
        Paragraph::new(Line::styled(truncate(hint, inner_width as usize), DIM))
            .render(inner(bottom - 1, 1), buf);
    }
}

/// `spans` cut to `width` cells; a cut span ends in `…`, like `truncate`.
fn truncate_spans(spans: &[Span<'static>], width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let span_width = display_width(&span.content);
        if used + span_width <= width {
            used += span_width;
            out.push(span.clone());
        } else {
            let cut = truncate(&span.content, width.saturating_sub(used));
            if !cut.is_empty() {
                out.push(Span::styled(cut, span.style));
            }
            break;
        }
    }
    out
}

/// A `/usage` row: the styled line indented, no highlight anywhere.
fn row_line(item: &PickerItem, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    if let Some(line) = &item.line {
        spans.extend(truncate_spans(&line.spans, width.saturating_sub(2)));
    }
    Line::from(spans)
}

fn option_line(
    item: &PickerItem,
    selected: bool,
    label_width: usize,
    width: usize,
    accent: Color,
) -> Line<'static> {
    let label_style = if selected {
        Style::new().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        item.style
    };
    let label = pad(&truncate(&item.label, label_width), label_width);
    let detail_width = width.saturating_sub(2 + label_width + 4);
    let mut spans = vec![
        if selected {
            Span::styled(CURSOR, Style::new().fg(accent))
        } else {
            Span::raw("  ")
        },
        Span::styled(label, label_style),
        Span::styled(
            if item.current { " ✓" } else { "  " },
            Style::new().fg(Color::Green),
        ),
    ];
    if !item.detail.is_empty() && detail_width > 1 {
        spans.push(Span::styled(
            format!("  {}", truncate(&item.detail, detail_width)),
            DIM,
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn models() -> Picker {
        let mut picker = Picker::new(PickerPurpose::Models, "Модели");
        picker.set_items(vec![
            PickerItem::heading("OpenRouter"),
            PickerItem::option("openrouter:a", "Alpha"),
            PickerItem::option("openrouter:b", "Beta").current(true),
            PickerItem::heading("Claude"),
            PickerItem::note("загрузка…", DIM),
            PickerItem::option("claude:g", "Gamma"),
        ]);
        picker
    }

    #[test]
    fn opens_on_the_current_option_and_skips_headings_and_notes() {
        let mut picker = models();
        assert_eq!(picker.pick(), PickerOutcome::Pick("openrouter:b".into()));
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.pick(), PickerOutcome::Pick("claude:g".into()));
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.pick(), PickerOutcome::Pick("openrouter:a".into()));
    }

    #[test]
    fn typing_filters_and_esc_clears_before_closing() {
        let mut picker = models();
        for ch in "gam".chars() {
            picker.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(picker.visible().len(), 2); // «Claude» heading + Gamma
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerOutcome::Pick("claude:g".into())
        );
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), PickerOutcome::Stay);
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), PickerOutcome::Close);
    }

    #[test]
    fn usage_rows_render_without_a_highlight_and_the_wheel_scrolls() {
        let mut picker = Picker::new(PickerPurpose::Usage, "Лимиты");
        let mut items = vec![PickerItem::heading("Devin")];
        items.extend((0..20).map(|i| {
            PickerItem::line(Line::from(format!("  строка {i}")))
        }));
        items.push(PickerItem::line(Line::from(vec![
            Span::raw("  День    "),
            Span::styled("███████░░░", Style::new().fg(Color::Red)),
            Span::raw(" 71%"),
        ])));
        picker.set_items(items);
        // A 10-row box fits a heading plus ~5 list rows: it overflows.
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, Color::Cyan);
        let text = |buf: &Buffer, area: Rect| -> Vec<String> {
            (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect()
                })
                .collect()
        };
        assert!(text(&buf, area).iter().any(|row| row.contains("строка 0")));
        assert!(!text(&buf, area).iter().any(|row| row.contains('❯')));

        // The wheel scrolls the viewport, not a selection — there is none.
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        for _ in 0..2 {
            picker.handle_mouse(wheel(MouseEventKind::ScrollDown));
        }
        assert!(picker.start > 0);
        picker.handle_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(picker.start, 3);

        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerOutcome::Close
        );
        assert_eq!(
            picker.handle_key(key(KeyCode::Esc)),
            PickerOutcome::Close
        );
    }

    #[test]
    fn renders_a_titled_box_with_the_current_mark() {
        let mut picker = models();
        let area = Rect::new(0, 0, 50, picker.height());
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, Color::Cyan);
        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        assert!(rows[0].starts_with("╭─ Модели ─"));
        assert!(rows.iter().any(|row| row.contains("❯ Beta  ✓")));
        assert!(rows.iter().any(|row| row.contains("загрузка…")));
    }
}
