//! The «Провайдеры» tab: providers in a column on the left; beside the
//! highlighted one either an «Авторизоваться» button or, once signed in,
//! the account and its limits, the «Модель-классификатор» choice, the
//! «Включить всё» / «Выключить всё» buttons and one switch per model that
//! shows or hides it in `/models`.
//!
//! Keys: ↑/↓ pick a provider, → or Enter step into the pane, ← or Esc step
//! back out; inside the pane ↑/↓ walk the stops (buttons and rows).

use dd_providers::{AccountInfo, ModelInfo, Quota};

use super::*;
use crate::hub::Load;

/// A focusable place in the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Login,
    Logout,
    Classifier,
    EnableAll,
    DisableAll,
    /// Index into the provider's model list.
    Model(usize),
}

/// One painted pane row.
enum PaneRow {
    Spacer,
    Heading(String),
    Text(Line<'static>),
    Stop(Stop),
    /// «Включить всё» and «Выключить всё» side by side.
    Buttons,
}

/// Label column of the account rows.
const FIELD_WIDTH: usize = 12;
const ENABLE_ALL: &str = " Включить всё ";
const DISABLE_ALL: &str = " Выключить всё ";
const CLASSIFIER_LABEL: &str = "Модель-классификатор";

fn provider_description(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenRouter => {
            "OpenRouter — сотни моделей разных компаний по одному ключу, оплата с баланса в долларах."
        }
        ProviderKind::Devin => {
            "Devin (Windsurf) — модели по подписке Devin с дневными и недельными лимитами."
        }
        ProviderKind::Claude => {
            "Claude — модели Anthropic по подписке Claude Pro/Max с пятичасовыми и недельными лимитами."
        }
    }
}

fn quota_name(name: &str) -> String {
    match name {
        "five_hour" => "5 часов".into(),
        "seven_day" => "7 дней".into(),
        "seven_day_opus" => "7 дней Opus".into(),
        "seven_day_oauth_apps" => "7 дней прил.".into(),
        "extra_usage" => "Доп. лимит".into(),
        "daily" => "День".into(),
        "weekly" => "Неделя".into(),
        "monthly" => "Месяц".into(),
        other => other.replace('_', " "),
    }
}

/// «через 2 ч 10 мин» until unix second `at`.
fn until(at: i64) -> String {
    let now = crate::sessions::now_ms() as i64 / 1000;
    let left = at - now;
    match left {
        ..=0 => "сейчас".into(),
        1..3600 => format!("через {} мин", (left / 60).max(1)),
        3600..86_400 => format!("через {} ч {} мин", left / 3600, left % 3600 / 60),
        _ => format!("через {} дн", left / 86_400),
    }
}

fn money(value: f64) -> String {
    format!("${value:.2}")
}

fn tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else {
        format!("{}K", count / 1000)
    }
}

fn field(name: &str, value: Vec<Span<'static>>) -> PaneRow {
    let mut spans = vec![Span::styled(
        format!("  {}", pad(name, FIELD_WIDTH)),
        Style::new().fg(Color::Gray),
    )];
    spans.extend(value);
    PaneRow::Text(Line::from(spans))
}

/// A ten-cell usage bar for `percent` used, green → yellow → red.
fn usage_bar(percent: f64) -> Vec<Span<'static>> {
    let percent = percent.clamp(0.0, 100.0);
    let filled = (percent / 10.0).round() as usize;
    let color = if percent < 60.0 {
        Color::Green
    } else if percent < 85.0 {
        Color::Yellow
    } else {
        Color::Red
    };
    vec![
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled("░".repeat(10 - filled), DIM),
        Span::raw(format!(" {percent:.0}%")),
    ]
}

fn quota_row(quota: &Quota) -> PaneRow {
    let mut value = if let Some(percent) = quota.utilization {
        usage_bar(percent)
    } else if let (Some(used), Some(limit)) = (quota.used, quota.limit) {
        vec![Span::raw(format!("{used:.2} из {limit:.2}"))]
    } else if let Some(remaining) = quota.remaining {
        vec![Span::raw(format!("осталось {remaining:.2}"))]
    } else {
        vec![Span::styled("нет данных", DIM)]
    };
    if let Some(at) = quota.resets_at {
        value.push(Span::styled(format!(" · сброс {}", until(at)), DIM));
    }
    field(&quota_name(&quota.name), value)
}

fn account_rows(info: &AccountInfo, rows: &mut Vec<PaneRow>) {
    let text = |s: String| vec![Span::raw(s)];
    if let Some(label) = &info.label {
        rows.push(field("Логин", text(label.clone())));
    }
    if let Some(plan) = &info.plan {
        rows.push(field("Тариф", text(plan.clone())));
    }
    if let Some(balance) = info.balance_usd {
        rows.push(field(
            "Баланс",
            vec![Span::styled(
                money(balance),
                Style::new().add_modifier(Modifier::BOLD),
            )],
        ));
    }
    if let Some(spent) = info.spent_usd {
        rows.push(field("Потрачено", text(money(spent))));
    }
    if let Some(limit) = info.credit_limit_usd {
        rows.push(field("Лимит", text(money(limit))));
    }
    if let Some(at) = info.expires_at {
        // У Claude это дата ежемесячного продления, не конец доступа.
        let verb = match info.provider {
            ProviderKind::Claude => "продление",
            _ => "истекает",
        };
        rows.push(field("Подписка", text(format!("{verb} {}", until(at)))));
    }
    for quota in &info.quotas {
        rows.push(quota_row(quota));
    }
    if rows.is_empty() {
        rows.push(PaneRow::Text(Line::styled(
            "  Провайдер не прислал сведений",
            DIM,
        )));
    }
}

/// The account rows as standalone lines — the `/usage` list paints the
/// same fields the Providers tab does.
pub(crate) fn account_lines(info: &AccountInfo) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    account_rows(info, &mut rows);
    rows.into_iter()
        .filter_map(|row| match row {
            PaneRow::Text(line) => Some(line),
            _ => None,
        })
        .collect()
}

/// `text` cut into rows of `width` cells (for unbreakable URLs and errors).
fn chunked(text: &str, width: usize, style: Style, rows: &mut Vec<PaneRow>) {
    let width = width.saturating_sub(2).max(8);
    let mut line = String::new();
    for ch in text.chars() {
        if display_width(&line) + display_width(ch.encode_utf8(&mut [0; 4])) > width {
            rows.push(PaneRow::Text(Line::styled(format!("  {line}"), style)));
            line.clear();
        }
        line.push(ch);
    }
    if !line.is_empty() {
        rows.push(PaneRow::Text(Line::styled(format!("  {line}"), style)));
    }
}

fn pane_rows(entry: &ProviderEntry, settings: &Settings, width: usize) -> Vec<PaneRow> {
    let mut rows = Vec::new();
    let error = Style::new().fg(Color::Red);
    if !entry.authenticated {
        rows.push(PaneRow::Text(Line::styled(
            "  Вход не выполнен.",
            Style::new().fg(Color::Gray),
        )));
        rows.push(PaneRow::Spacer);
        rows.push(PaneRow::Stop(Stop::Login));
        if let Some(wait) = &entry.login {
            rows.push(PaneRow::Spacer);
            rows.push(PaneRow::Text(Line::styled(
                "  Если браузер не открылся, откройте ссылку:",
                DIM,
            )));
            chunked(&wait.url, width, DIM, &mut rows);
        }
        if let Some(message) = &entry.login_error {
            rows.push(PaneRow::Spacer);
            chunked(&format!("Ошибка входа: {message}"), width, error, &mut rows);
        }
        return rows;
    }

    // «Выйти из аккаунта» занимает место кнопки входа.
    rows.push(PaneRow::Text(Line::styled(
        "  Вход выполнен.",
        Style::new().fg(Color::Gray),
    )));
    rows.push(PaneRow::Spacer);
    rows.push(PaneRow::Stop(Stop::Logout));
    rows.push(PaneRow::Spacer);
    rows.push(PaneRow::Heading("Аккаунт".into()));
    match &entry.account {
        Load::Idle | Load::Loading => rows.push(PaneRow::Text(Line::styled(
            "  Загрузка данных аккаунта…",
            DIM,
        ))),
        Load::Failed(message) => chunked(
            &format!("Не удалось загрузить: {message}"),
            width,
            error,
            &mut rows,
        ),
        Load::Ready(info) => {
            let mut account = Vec::new();
            account_rows(info, &mut account);
            rows.extend(account);
        }
    }

    rows.push(PaneRow::Spacer);
    rows.push(PaneRow::Heading("Настройки".into()));
    rows.push(PaneRow::Stop(Stop::Classifier));

    rows.push(PaneRow::Spacer);
    match &entry.models {
        Load::Ready(models) => {
            let shown = models
                .iter()
                .filter(|m| !settings.model_hidden(entry.kind, &m.id))
                .count();
            rows.push(PaneRow::Heading(format!(
                "Модели — {shown} из {} в /models",
                models.len()
            )));
            if models.is_empty() {
                rows.push(PaneRow::Text(Line::styled(
                    "  Провайдер не вернул моделей",
                    DIM,
                )));
            } else {
                rows.push(PaneRow::Buttons);
                rows.extend((0..models.len()).map(|i| PaneRow::Stop(Stop::Model(i))));
            }
        }
        Load::Failed(message) => {
            rows.push(PaneRow::Heading("Модели".into()));
            chunked(
                &format!("Не удалось загрузить: {message}"),
                width,
                error,
                &mut rows,
            );
        }
        Load::Idle | Load::Loading => {
            rows.push(PaneRow::Heading("Модели".into()));
            rows.push(PaneRow::Text(Line::styled("  Загрузка моделей…", DIM)));
        }
    }
    rows
}

/// Every stop in walking order with the pane row that holds it.
fn stops(rows: &[PaneRow]) -> Vec<(usize, Stop)> {
    let mut out = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match row {
            PaneRow::Stop(stop) => out.push((index, *stop)),
            PaneRow::Buttons => {
                out.push((index, Stop::EnableAll));
                out.push((index, Stop::DisableAll));
            }
            _ => {}
        }
    }
    out
}

fn stop_description(stop: Stop, entry: &ProviderEntry) -> String {
    let name = entry.kind.display_name();
    match stop {
        Stop::Login if entry.login.is_some() => {
            "Ждём завершения входа в браузере. Enter — отменить вход.".into()
        }
        Stop::Login => format!(
            "Откроет страницу входа {name} в браузере. После входа здесь появятся аккаунт, лимиты и модели."
        ),
        Stop::Logout => format!(
            "Удалит сохранённый вход {name}. Модели провайдера пропадут из /models до нового входа."
        ),
        Stop::Classifier => CLASSIFIER_DESCRIPTION.into(),
        Stop::EnableAll => format!("Показать все модели {name} в списке /models."),
        Stop::DisableAll => format!("Скрыть все модели {name} из списка /models."),
        Stop::Model(index) => entry
            .models()
            .get(index)
            .map(model_description)
            .unwrap_or_default(),
    }
}

fn model_description(model: &ModelInfo) -> String {
    let mut parts = vec![model.id.clone()];
    if let Some(context) = model.context_window {
        parts.push(format!("контекст {}", tokens(context)));
    }
    if let (Some(input), Some(output)) = (model.input_price, model.output_price) {
        parts.push(format!("${input}/${output} за 1M токенов"));
    }
    let mut text = parts.join(" · ");
    text.push_str(". Enter — показать или скрыть в /models.");
    text
}

impl SettingsMenu {
    /// ↑/↓ in the pane: to the stop on the next or previous row (the
    /// two buttons share one). Up from the first row climbs onto the tabs.
    fn step_stop(&mut self, stops: &[(usize, Stop)], forward: bool) {
        let Some(&(row, _)) = stops.get(self.stop) else {
            return;
        };
        let next = if forward {
            stops.iter().position(|&(at, _)| at > row)
        } else {
            stops.iter().rposition(|&(at, _)| at < row).map(|last| {
                stops
                    .iter()
                    .position(|&(at, _)| at == stops[last].0)
                    .unwrap_or(last)
            })
        };
        match next {
            Some(index) => self.stop = index,
            None if !forward => self.tabs_focus = true,
            None => {}
        }
    }

    /// ↑/↓ in the provider column; up from the first climbs onto the tabs.
    fn step_provider(&mut self, count: usize, forward: bool) {
        if count == 0 {
            return;
        }
        if forward {
            if self.provider + 1 < count {
                self.provider += 1;
                self.stop = 0;
            }
        } else if self.provider == 0 {
            self.tabs_focus = true;
        } else {
            self.provider -= 1;
            self.stop = 0;
        }
    }

    /// Mouse wheel in the pane: plain stop by stop, no climbing.
    fn wheel_stop(&mut self, count: usize, forward: bool) {
        if count == 0 {
            return;
        }
        self.stop = if forward {
            (self.stop + 1).min(count - 1)
        } else {
            self.stop.saturating_sub(1)
        };
    }

    fn pane_width(&self) -> usize {
        (self.geometry.pane.width as usize).max(40)
    }

    pub(super) fn providers_key(
        &mut self,
        key: KeyEvent,
        settings: &mut Settings,
        providers: &[ProviderEntry],
    ) -> MenuOutcome {
        let entry = providers.get(self.provider);
        if !self.pane_focus || entry.is_none() {
            self.pane_focus = false;
            match key.code {
                KeyCode::Esc => return MenuOutcome::Close,
                KeyCode::Up => self.step_provider(providers.len(), false),
                KeyCode::Down => self.step_provider(providers.len(), true),
                KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') if entry.is_some() => {
                    self.pane_focus = true;
                    self.stop = 0;
                }
                _ => {}
            }
            return MenuOutcome::Stay;
        }
        let entry = entry.expect("checked above");
        let stops = stops(&pane_rows(entry, settings, self.pane_width()));
        self.stop = self.stop.min(stops.len().saturating_sub(1));
        let current = stops.get(self.stop).map(|&(_, stop)| stop);
        match key.code {
            KeyCode::Esc => self.pane_focus = false,
            // ←/→ walk the two buttons, ← from the left one returns to the
            // provider column.
            KeyCode::Left if current == Some(Stop::DisableAll) => self.stop -= 1,
            KeyCode::Left => self.pane_focus = false,
            KeyCode::Right if current == Some(Stop::EnableAll) => self.stop += 1,
            KeyCode::Up => self.step_stop(&stops, false),
            KeyCode::Down => self.step_stop(&stops, true),
            KeyCode::Home => self.stop = 0,
            KeyCode::End => self.stop = stops.len().saturating_sub(1),
            KeyCode::PageUp => self.stop = self.stop.saturating_sub(10),
            KeyCode::PageDown => self.stop = (self.stop + 10).min(stops.len().saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(&(_, stop)) = stops.get(self.stop) {
                    return self.activate_stop(stop, entry, settings);
                }
            }
            _ => {}
        }
        MenuOutcome::Stay
    }

    fn activate_stop(
        &mut self,
        stop: Stop,
        entry: &ProviderEntry,
        settings: &mut Settings,
    ) -> MenuOutcome {
        let kind = entry.kind;
        match stop {
            Stop::Login if entry.login.is_some() => MenuOutcome::CancelLogin(kind),
            Stop::Login => MenuOutcome::Login(kind),
            Stop::Logout => MenuOutcome::Logout(kind),
            Stop::Classifier => {
                self.submenu = Some(Submenu {
                    target: SubTarget::Classifier,
                    selected: 0,
                });
                MenuOutcome::Stay
            }
            Stop::EnableAll => {
                settings.set_all_hidden(kind, std::iter::empty(), false);
                MenuOutcome::Changed
            }
            Stop::DisableAll => {
                settings.set_all_hidden(kind, entry.models().iter().map(|m| m.id.as_str()), true);
                MenuOutcome::Changed
            }
            Stop::Model(index) => {
                let Some(model) = entry.models().get(index) else {
                    return MenuOutcome::Stay;
                };
                let hidden = settings.model_hidden(kind, &model.id);
                settings.set_model_hidden(kind, &model.id, !hidden);
                MenuOutcome::Changed
            }
        }
    }

    pub(super) fn providers_mouse(
        &mut self,
        mouse: MouseEvent,
        settings: &mut Settings,
        providers: &[ProviderEntry],
    ) -> MenuOutcome {
        let (column, row) = (mouse.column, mouse.row);
        let inside = |area: Rect| area.contains((column, row).into());
        let in_list = inside(self.geometry.provider_list);
        let Some(entry) = providers.get(self.provider) else {
            return MenuOutcome::Stay;
        };
        let stops = stops(&pane_rows(entry, settings, self.pane_width()));
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let forward = mouse.kind == MouseEventKind::ScrollDown;
                if in_list || !self.pane_focus {
                    self.step_provider(providers.len(), forward);
                } else {
                    self.wheel_stop(stops.len(), forward);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_list => {
                let index = (row - self.geometry.provider_list.y) as usize;
                if index < providers.len() {
                    if index != self.provider {
                        self.stop = 0;
                    }
                    self.provider = index;
                    self.pane_focus = false;
                }
            }
            MouseEventKind::Down(MouseButton::Left) if inside(self.geometry.pane) => {
                let pane_row = self.geometry.pane_start + (row - self.geometry.pane.y) as usize;
                let button = self
                    .geometry
                    .buttons
                    .iter()
                    .find(|(y, columns, _)| *y == row && columns.contains(&column))
                    .map(|(_, _, stop)| *stop);
                let found = stops.iter().position(|&(at, stop)| {
                    at == pane_row
                        && match stop {
                            Stop::EnableAll | Stop::DisableAll => button == Some(stop),
                            _ => true,
                        }
                });
                if let Some(index) = found {
                    if self.pane_focus && index == self.stop {
                        return self.activate_stop(stops[index].1, entry, settings);
                    }
                    self.pane_focus = true;
                    self.stop = index;
                }
            }
            _ => {}
        }
        MenuOutcome::Stay
    }

    pub(super) fn render_providers(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        settings: &Settings,
        providers: &[ProviderEntry],
        accent: Color,
    ) {
        if area.height == 0 || area.width < 20 {
            return;
        }
        let reserve = DESCRIPTION_ROWS as u16 + 1;
        let list_height = if area.height > reserve + 1 {
            area.height - reserve
        } else {
            area.height
        };
        self.provider = self.provider.min(providers.len().saturating_sub(1));

        // The provider column.
        let name_width = providers
            .iter()
            .map(|entry| display_width(entry.kind.display_name()))
            .max()
            .unwrap_or(0);
        let column_width = (name_width + 6) as u16;
        let lines: Vec<Line> = providers
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let selected = index == self.provider;
                let cursor = if selected && !self.pane_focus && !self.tabs_focus {
                    Span::styled(CURSOR, Style::new().fg(accent))
                } else {
                    Span::raw("  ")
                };
                let dot = if entry.authenticated {
                    Span::styled("● ", Style::new().fg(Color::Green))
                } else {
                    Span::styled("○ ", DIM)
                };
                let style = if selected {
                    Style::new().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                Line::from(vec![
                    cursor,
                    dot,
                    Span::styled(entry.kind.display_name(), style),
                ])
            })
            .collect();
        let column = Rect::new(area.x, area.y, column_width.min(area.width), list_height);
        Paragraph::new(lines).render(column, buf);
        self.geometry.provider_list = Rect::new(
            column.x,
            column.y,
            column.width,
            (providers.len() as u16).min(list_height),
        );
        for y in column.y..column.bottom() {
            buf.set_string(column.right(), y, "│", DIM);
        }

        let pane = Rect::new(
            column.right() + 2,
            area.y,
            area.width.saturating_sub(column.width + 2),
            list_height,
        );
        let Some(entry) = providers.get(self.provider) else {
            return;
        };
        let rows = pane_rows(entry, settings, pane.width as usize);
        let stops = stops(&rows);
        self.stop = self.stop.min(stops.len().saturating_sub(1));
        let selected = stops
            .get(self.stop)
            .copied()
            .filter(|_| self.pane_focus && !self.tabs_focus);

        let visible = pane.height as usize;
        let start = selected
            .map_or(0, |(row, _)| row.saturating_sub(visible / 2))
            .min(rows.len().saturating_sub(visible));
        let scrollbar = Scrollbar::new(pane, rows.len(), start);
        let width = if scrollbar.is_some() {
            pane.width.saturating_sub(2)
        } else {
            pane.width
        };
        let label_width = entry
            .models()
            .iter()
            .map(|m| display_width(&m.display_name))
            .chain([display_width(CLASSIFIER_LABEL)])
            .max()
            .unwrap_or(0)
            .min((width as usize / 2).max(10));
        let is = |stop: Stop| selected.is_some_and(|(_, s)| s == stop);
        let item = |stop: Stop, label: &str, value: &str, changed: bool| {
            let on = is(stop);
            let (label_style, value_style) = if on {
                let style = Style::new().fg(accent).add_modifier(Modifier::BOLD);
                (style, style)
            } else {
                (Style::new(), Style::new().fg(Color::Gray))
            };
            let value_style = if changed {
                value_style.add_modifier(Modifier::BOLD)
            } else {
                value_style
            };
            Line::from(vec![
                if on {
                    Span::styled(CURSOR, Style::new().fg(accent))
                } else {
                    Span::raw("  ")
                },
                Span::styled(pad(&truncate(label, label_width), label_width), label_style),
                Span::raw("  "),
                Span::styled(value.to_string(), value_style),
            ])
        };
        let button = |label: &str, on: bool| {
            if on {
                Span::styled(
                    format!("[{label}]"),
                    Style::new()
                        .fg(Color::Black)
                        .bg(accent)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!("[{label}]"), Style::new().fg(accent))
            }
        };

        self.geometry.buttons.clear();
        let mut lines = Vec::new();
        for (index, row) in rows.iter().enumerate().skip(start).take(visible) {
            let y = pane.y + (index - start) as u16;
            lines.push(match row {
                PaneRow::Spacer => Line::default(),
                PaneRow::Heading(name) => Line::from(vec![
                    Span::raw("  "),
                    Span::styled(name.clone(), DIM.add_modifier(Modifier::BOLD)),
                ]),
                PaneRow::Text(line) => line.clone(),
                PaneRow::Stop(Stop::Login) => {
                    let label = if entry.login.is_some() {
                        " Ожидание входа в браузере… · Enter — отменить "
                    } else {
                        " Авторизоваться "
                    };
                    let on = is(Stop::Login);
                    Line::from(vec![
                        if on {
                            Span::styled(CURSOR, Style::new().fg(accent))
                        } else {
                            Span::raw("  ")
                        },
                        button(label, on),
                    ])
                }
                PaneRow::Stop(Stop::Logout) => {
                    let on = is(Stop::Logout);
                    Line::from(vec![
                        if on {
                            Span::styled(CURSOR, Style::new().fg(accent))
                        } else {
                            Span::raw("  ")
                        },
                        button(" Выйти из аккаунта ", on),
                    ])
                }
                PaneRow::Stop(Stop::Classifier) => {
                    item(Stop::Classifier, CLASSIFIER_LABEL, "Отключить", false)
                }
                PaneRow::Stop(Stop::Model(i)) => {
                    let model = &entry.models()[*i];
                    let hidden = settings.model_hidden(entry.kind, &model.id);
                    item(
                        Stop::Model(*i),
                        &model.display_name,
                        if hidden { "выкл" } else { "вкл" },
                        hidden,
                    )
                }
                PaneRow::Stop(_) => Line::default(),
                PaneRow::Buttons => {
                    let enable_on = is(Stop::EnableAll);
                    let disable_on = is(Stop::DisableAll);
                    let x = pane.x + 2;
                    let enable_width = display_width(ENABLE_ALL) as u16 + 2;
                    let disable_x = x + enable_width + 2;
                    let disable_width = display_width(DISABLE_ALL) as u16 + 2;
                    self.geometry
                        .buttons
                        .push((y, x..x + enable_width, Stop::EnableAll));
                    self.geometry.buttons.push((
                        y,
                        disable_x..disable_x + disable_width,
                        Stop::DisableAll,
                    ));
                    let cursor = if enable_on || disable_on {
                        Span::styled(CURSOR, Style::new().fg(accent))
                    } else {
                        Span::raw("  ")
                    };
                    Line::from(vec![
                        cursor,
                        button(ENABLE_ALL, enable_on),
                        Span::raw("  "),
                        button(DISABLE_ALL, disable_on),
                    ])
                }
            });
        }
        let rows_area = Rect::new(pane.x, pane.y, width, pane.height);
        Paragraph::new(lines).render(rows_area, buf);
        if let Some(scrollbar) = scrollbar {
            scrollbar.render(buf, false);
        }
        self.geometry.pane = rows_area;
        self.geometry.pane_start = start;

        if list_height < area.height {
            let description = match selected {
                Some((_, stop)) => stop_description(stop, entry),
                None => provider_description(entry.kind).to_string(),
            };
            let lines: Vec<Line> = wrapped(
                &description,
                (area.width as usize).saturating_sub(4),
                DESCRIPTION_ROWS,
            )
            .into_iter()
            .map(|line| Line::styled(format!("  {line}"), Style::new().fg(Color::Gray)))
            .collect();
            Paragraph::new(lines).render(
                Rect::new(
                    area.x,
                    area.y + list_height + 1,
                    area.width,
                    DESCRIPTION_ROWS as u16,
                ),
                buf,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn providers() -> Vec<ProviderEntry> {
        let mut claude = ProviderEntry::new(ProviderKind::Claude, true);
        claude.account = Load::Ready(AccountInfo {
            provider: ProviderKind::Claude,
            label: Some("me@example.com".into()),
            plan: Some("Max".into()),
            expires_at: None,
            started_at: None,
            balance_usd: None,
            spent_usd: None,
            credit_limit_usd: None,
            quotas: vec![Quota {
                name: "five_hour".into(),
                utilization: Some(40.0),
                ..Default::default()
            }],
        });
        claude.models = Load::Ready(vec![
            ModelInfo {
                id: "claude-a".into(),
                display_name: "Claude A".into(),
                ..Default::default()
            },
            ModelInfo {
                id: "claude-b".into(),
                display_name: "Claude B".into(),
                ..Default::default()
            },
        ]);
        vec![
            ProviderEntry::new(ProviderKind::OpenRouter, false),
            ProviderEntry::new(ProviderKind::Devin, false),
            claude,
        ]
    }

    fn screen(
        menu: &mut SettingsMenu,
        settings: &Settings,
        providers: &[ProviderEntry],
    ) -> Vec<String> {
        let area = Rect::new(0, 0, 90, 24);
        let mut buf = Buffer::empty(area);
        menu.render(area, &mut buf, settings, providers);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn providers_menu() -> SettingsMenu {
        let mut menu = crate::settings::tests::content_menu();
        menu.tab = 3;
        menu
    }

    #[test]
    fn signed_out_providers_offer_one_login_button() {
        let (mut settings, providers) = (Settings::default(), providers());
        let mut menu = providers_menu();
        let rows = screen(&mut menu, &settings, &providers);
        assert!(rows.iter().any(|row| row.contains("❯ ○ OpenRouter")));
        assert!(rows.iter().any(|row| row.contains("[ Авторизоваться ]")));
        menu.handle_key(key(KeyCode::Right), &mut settings, &providers);
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &providers),
            MenuOutcome::Login(ProviderKind::OpenRouter)
        );
    }

    #[test]
    fn signed_in_pane_shows_the_account_classifier_and_model_switches() {
        let (mut settings, providers) = (Settings::default(), providers());
        let mut menu = providers_menu();
        menu.provider = 2;
        let rows = screen(&mut menu, &settings, &providers);
        assert!(rows.iter().any(|row| row.contains("me@example.com")));
        assert!(rows.iter().any(|row| row.contains("████░░░░░░ 40%")));
        assert!(
            rows.iter()
                .any(|row| row.contains("Модель-классификатор  Отключить"))
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Модели — 2 из 2 в /models"))
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("[ Включить всё ]  [ Выключить всё ]"))
        );

        assert!(rows.iter().any(|row| row.contains("[ Выйти из аккаунта ]")));

        let mut press = |code| menu.handle_key(key(code), &mut settings, &providers);
        // Rows of stops: logout, classifier, [enable all | disable all],
        // Claude A, Claude B.
        press(KeyCode::Right);
        assert_eq!(
            press(KeyCode::Enter),
            MenuOutcome::Logout(ProviderKind::Claude)
        );
        press(KeyCode::Down);
        press(KeyCode::Enter);
        press(KeyCode::Enter); // classifier list opened and closed
        press(KeyCode::Down); // «Включить всё»
        press(KeyCode::Right); // «Выключить всё»
        press(KeyCode::Down); // Claude A: ↓ leaves the button row
        assert_eq!(press(KeyCode::Enter), MenuOutcome::Changed);
        drop(press);
        assert!(settings.model_hidden(ProviderKind::Claude, "claude-a"));
        let mut press = |code| menu.handle_key(key(code), &mut settings, &providers);
        press(KeyCode::Up); // back on «Включить всё»
        press(KeyCode::Right);
        press(KeyCode::Enter);
        drop(press);
        assert!(settings.model_hidden(ProviderKind::Claude, "claude-b"));
        let mut press = |code| menu.handle_key(key(code), &mut settings, &providers);
        press(KeyCode::Left);
        press(KeyCode::Enter);
        drop(press);
        assert!(!settings.model_hidden(ProviderKind::Claude, "claude-a"));
        // ← from the left button returns to the column, ↑ climbs to the
        // tabs, and only there ←/→ switch tabs.
        let mut press = |code| menu.handle_key(key(code), &mut settings, &providers);
        press(KeyCode::Left);
        press(KeyCode::Left);
        drop(press);
        assert_eq!((menu.tab, menu.pane_focus), (3, false));
        for _ in 0..3 {
            menu.handle_key(key(KeyCode::Up), &mut settings, &providers);
        }
        assert!(menu.tabs_focus);
        menu.handle_key(key(KeyCode::Left), &mut settings, &providers);
        assert_eq!(menu.tab, 2);
        assert_eq!(
            menu.handle_key(key(KeyCode::Esc), &mut settings, &providers),
            MenuOutcome::Close
        );
    }

    fn text_of(row: &PaneRow) -> String {
        match row {
            PaneRow::Text(line) => line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect(),
            _ => String::new(),
        }
    }

    #[test]
    fn devin_quotas_render_as_usage_bars() {
        let mut rows = Vec::new();
        account_rows(
            &AccountInfo {
                provider: ProviderKind::Devin,
                label: None,
                plan: Some("Pro".into()),
                expires_at: None,
                started_at: None,
                balance_usd: None,
                spent_usd: None,
                credit_limit_usd: None,
                quotas: vec![Quota {
                    name: "daily".into(),
                    remaining: Some(29.0),
                    utilization: Some(71.0),
                    ..Default::default()
                }],
            },
            &mut rows,
        );
        assert!(
            rows.iter()
                .map(text_of)
                .any(|row| row.contains("███████░░░ 71%"))
        );
    }

    #[test]
    fn claude_shows_the_subscription_renewal_not_expiry() {
        let mut rows = Vec::new();
        account_rows(
            &AccountInfo {
                provider: ProviderKind::Claude,
                label: None,
                plan: Some("Max".into()),
                expires_at: Some(crate::sessions::now_ms() as i64 / 1000 + 5 * 86400),
                started_at: None,
                balance_usd: None,
                spent_usd: None,
                credit_limit_usd: None,
                quotas: Vec::new(),
            },
            &mut rows,
        );
        assert!(rows.iter().map(text_of).any(|row| {
            row.contains("Подписка") && row.contains("продление через 5 дн")
        }));
    }

    #[test]
    fn clicking_a_provider_and_a_button_works() {
        let (mut settings, providers) = (Settings::default(), providers());
        let mut menu = providers_menu();
        screen(&mut menu, &settings, &providers);
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let list = menu.geometry.provider_list;
        menu.handle_mouse(click(list.x, list.y + 2), &mut settings, &providers);
        assert_eq!(menu.provider, 2);
        screen(&mut menu, &settings, &providers);
        let (y, columns, _) = menu.geometry.buttons[1].clone();
        menu.handle_mouse(click(columns.start, y), &mut settings, &providers);
        screen(&mut menu, &settings, &providers);
        let (y, columns, _) = menu.geometry.buttons[1].clone();
        assert_eq!(
            menu.handle_mouse(click(columns.start, y), &mut settings, &providers),
            MenuOutcome::Changed
        );
        assert!(settings.model_hidden(ProviderKind::Claude, "claude-a"));
    }
}
