//! The `/config` menu (alias `/settings`), modeled on oh-my-pi's settings
//! overlay (`settings-selector.ts` over `settings-list.ts`): a fullscreen
//! rounded frame titled «Настройки», a tab strip, the setting rows under
//! their section headings (a blank spacer row before each heading but the
//! first), a fixed three-row description and a footer hint line.
//! Enter/Space flips a switch or opens a choice submenu; every change
//! applies at once and persists to a small `key = value` file. The
//! «Провайдеры» tab ([`providers_tab`]) lists providers in a column with
//! login, account limits and the model catalog beside them.

mod providers_tab;

pub(crate) use providers_tab::account_lines;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::Range;
use std::path::PathBuf;

use dd_providers::{Effort, ProviderKind};

use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::composer::{display_width, wrap_line_at_whitespace_ranges};
use crate::hub::ProviderEntry;
use crate::scrollbar::Scrollbar;

/// Settings-panel tabs, in strip order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Appearance,
    Input,
    Chat,
    Providers,
}

const TABS: &[Tab] = &[Tab::Appearance, Tab::Input, Tab::Chat, Tab::Providers];

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Внешний вид",
            Self::Input => "Ввод",
            Self::Chat => "Чат",
            Self::Providers => "Провайдеры",
        }
    }
}

/// One choice of a [`Kind::Choice`] setting: the stored key, the label the
/// panel shows and an optional one-line note.
#[derive(Debug)]
pub(crate) struct Choice {
    key: &'static str,
    label: &'static str,
    description: &'static str,
}

/// How a setting edits: a switch flipped in place, a list of choices
/// picked in a submenu, or a text line edited in place.
#[derive(Debug)]
pub(crate) enum Kind {
    Toggle,
    Choice(&'static [Choice]),
    /// Free text kept outside `values` — [`Settings::text`] by `id`.
    Text,
}

/// One setting as the panel shows it, like oh-my-pi's `SettingDef`:
/// declaration order is the panel order, `group` is its section.
#[derive(Debug)]
pub(crate) struct SettingDef {
    id: &'static str,
    tab: Tab,
    group: &'static str,
    label: &'static str,
    description: &'static str,
    kind: Kind,
    /// Default value: 0/1 for a toggle, a choice index otherwise.
    default: usize,
}

impl SettingDef {
    /// How many values the setting takes.
    fn value_count(&self) -> usize {
        match self.kind {
            Kind::Toggle => 2,
            Kind::Choice(choices) => choices.len(),
            Kind::Text => 1,
        }
    }

    fn value_key(&self, value: usize) -> &'static str {
        match self.kind {
            Kind::Toggle if value != 0 => "true",
            Kind::Toggle => "false",
            Kind::Choice(choices) => choices[value.min(choices.len() - 1)].key,
            // Text serializes through [`Settings::text`], not `values`.
            Kind::Text => "",
        }
    }

    fn value_label(&self, value: usize) -> &'static str {
        match self.kind {
            Kind::Toggle if value != 0 => "вкл",
            Kind::Toggle => "выкл",
            Kind::Choice(choices) => choices[value.min(choices.len() - 1)].label,
            // `item_line` paints the stored text itself.
            Kind::Text => "",
        }
    }
}

pub(crate) const ACCENT: &str = "appearance.accent";
pub(crate) const MESSAGE_BACKGROUND: &str = "appearance.messageBackground";
pub(crate) const SCROLLBARS: &str = "appearance.scrollbars";
pub(crate) const COMPOSER_ROWS: &str = "input.composerRows";
pub(crate) const HISTORY_RECALL: &str = "input.historyRecall";
pub(crate) const COMMAND_HINTS: &str = "input.commandHints";
pub(crate) const MOUSE_CAPTURE: &str = "input.mouseCapture";
const CHAT_MODEL: &str = "chat.model";
const CHAT_MODEL_NAME: &str = "chat.modelName";
const CHAT_EFFORT: &str = "chat.effort";
const CHAT_SYSTEM_PROMPT: &str = "chat.systemPrompt";

const ACCENT_CHOICES: &[Choice] = &[
    Choice {
        key: "cyan",
        label: "Бирюзовый",
        description: "",
    },
    Choice {
        key: "green",
        label: "Зелёный",
        description: "",
    },
    Choice {
        key: "magenta",
        label: "Пурпурный",
        description: "",
    },
    Choice {
        key: "yellow",
        label: "Жёлтый",
        description: "",
    },
    Choice {
        key: "blue",
        label: "Синий",
        description: "",
    },
];

const COMPOSER_ROW_CHOICES: &[Choice] = &[
    Choice {
        key: "auto",
        label: "Авто",
        description: "от 2 до 8 строк в зависимости от высоты окна",
    },
    Choice {
        key: "2",
        label: "2 строки",
        description: "",
    },
    Choice {
        key: "4",
        label: "4 строки",
        description: "",
    },
    Choice {
        key: "6",
        label: "6 строк",
        description: "",
    },
    Choice {
        key: "8",
        label: "8 строк",
        description: "",
    },
    Choice {
        key: "12",
        label: "12 строк",
        description: "",
    },
];

/// Every setting, in panel order: tab, then section, then row.
pub(crate) const SETTINGS: &[SettingDef] = &[
    SettingDef {
        id: ACCENT,
        tab: Tab::Appearance,
        group: "Тема",
        label: "Акцентный цвет",
        description: "Цвет рамок поля ввода, выделенных подсказок команд, вкладок и курсора этого меню.",
        kind: Kind::Choice(ACCENT_CHOICES),
        default: 0,
    },
    SettingDef {
        id: MESSAGE_BACKGROUND,
        tab: Tab::Appearance,
        group: "Сообщения",
        label: "Фон сообщений",
        description: "Рисовать отправленные сообщения на сером блоке во всю ширину окна.",
        kind: Kind::Toggle,
        default: 1,
    },
    SettingDef {
        id: SCROLLBARS,
        tab: Tab::Appearance,
        group: "Сообщения",
        label: "Полосы прокрутки",
        description: "Показывать полосу прокрутки у истории и поля ввода, когда текст не помещается.",
        kind: Kind::Toggle,
        default: 1,
    },
    SettingDef {
        id: COMPOSER_ROWS,
        tab: Tab::Input,
        group: "Поле ввода",
        label: "Высота поля ввода",
        description: "Сколько строк поле ввода занимает, прежде чем его текст начнёт прокручиваться.",
        kind: Kind::Choice(COMPOSER_ROW_CHOICES),
        default: 0,
    },
    SettingDef {
        id: HISTORY_RECALL,
        tab: Tab::Input,
        group: "Поле ввода",
        label: "История по ↑/↓",
        description: "Стрелки на первой и последней строке поля листают ранее отправленные сообщения.",
        kind: Kind::Toggle,
        default: 1,
    },
    SettingDef {
        id: COMMAND_HINTS,
        tab: Tab::Input,
        group: "Команды",
        label: "Подсказки команд",
        description: "Показывать список подходящих команд над полем ввода, когда текст начинается с /.",
        kind: Kind::Toggle,
        default: 1,
    },
    SettingDef {
        id: MOUSE_CAPTURE,
        tab: Tab::Input,
        group: "Мышь",
        label: "Захват мыши",
        description: "Выделение, колесо и полосы прокрутки мышью. Выключите, чтобы вернуть \
                      терминалу его собственное выделение текста (и меню перестанет \
                      реагировать на мышь).",
        kind: Kind::Toggle,
        default: 1,
    },
    SettingDef {
        id: CHAT_SYSTEM_PROMPT,
        tab: Tab::Chat,
        group: "Запрос",
        label: "Системный промпт",
        description: "Текст, который уходит системным сообщением в каждый запрос — один \
                      для всех провайдеров. Пустое значение — без промпта. \
                      Enter открывает поле ввода.",
        kind: Kind::Text,
        default: 0,
    },
];

/// Position of setting `id` in [`SETTINGS`].
fn setting_index(id: &str) -> usize {
    SETTINGS
        .iter()
        .position(|def| def.id == id)
        .expect("known setting id")
}

/// The model chats go to, picked in `/models`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectedModel {
    pub(crate) provider: ProviderKind,
    pub(crate) id: String,
    /// Display name, kept so the status line has it before catalogs load.
    pub(crate) name: String,
}

/// Current value of every setting, indexed like [`SETTINGS`], plus the
/// chat choices and per-provider model visibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    values: Vec<usize>,
    /// `None` until a model is picked in `/models`.
    pub(crate) model: Option<SelectedModel>,
    /// `/effort`; `None` is «выкл».
    pub(crate) effort: Option<Effort>,
    /// The system prompt sent with every request; empty — none.
    pub(crate) system_prompt: String,
    /// Model ids hidden from `/models`, per provider (`ProviderKind::as_str`).
    hidden_models: BTreeMap<String, BTreeSet<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            values: SETTINGS.iter().map(|def| def.default).collect(),
            model: None,
            effort: Some(Effort::Medium),
            system_prompt: String::new(),
            hidden_models: BTreeMap::new(),
        }
    }
}

/// Russian name of an effort level, as `/effort` and the status line show it.
pub(crate) fn effort_label(effort: Option<Effort>) -> &'static str {
    match effort {
        None => "выкл",
        Some(Effort::Minimal) => "минимальный",
        Some(Effort::Low) => "низкий",
        Some(Effort::Medium) => "средний",
        Some(Effort::High) => "высокий",
        Some(Effort::XHigh) => "сверхвысокий",
        Some(Effort::Max) => "максимальный",
    }
}

impl Settings {
    /// Whether model `id` of `provider` is left out of `/models`.
    pub(crate) fn model_hidden(&self, provider: ProviderKind, id: &str) -> bool {
        self.hidden_models
            .get(provider.as_str())
            .is_some_and(|hidden| hidden.contains(id))
    }

    pub(crate) fn set_model_hidden(&mut self, provider: ProviderKind, id: &str, hidden: bool) {
        let set = self
            .hidden_models
            .entry(provider.as_str().to_string())
            .or_default();
        if hidden {
            set.insert(id.to_string());
        } else {
            set.remove(id);
        }
    }

    /// Show every model of `provider`, or hide every one of `ids`.
    pub(crate) fn set_all_hidden<'a>(
        &mut self,
        provider: ProviderKind,
        ids: impl Iterator<Item = &'a str>,
        hidden: bool,
    ) {
        let set = self
            .hidden_models
            .entry(provider.as_str().to_string())
            .or_default();
        set.clear();
        if hidden {
            set.extend(ids.map(str::to_string));
        }
    }

    /// A toggle's state.
    pub(crate) fn flag(&self, id: &str) -> bool {
        self.values[setting_index(id)] != 0
    }

    fn choice_key(&self, id: &str) -> &'static str {
        let index = setting_index(id);
        SETTINGS[index].value_key(self.values[index])
    }

    /// A `Kind::Text` setting's stored text, `""` when unset.
    pub(crate) fn text(&self, id: &str) -> &str {
        match id {
            CHAT_SYSTEM_PROMPT => self.system_prompt.as_str(),
            _ => "",
        }
    }

    fn set_text(&mut self, id: &str, value: &str) {
        if id == CHAT_SYSTEM_PROMPT {
            self.system_prompt = value.to_string();
        }
    }

    /// The accent color the palette and this menu highlight with.
    pub(crate) fn accent(&self) -> Color {
        match self.choice_key(ACCENT) {
            "green" => Color::Green,
            "magenta" => Color::Magenta,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            _ => Color::Cyan,
        }
    }

    /// The fixed composer row cap, or `None` for the screen-derived default.
    pub(crate) fn composer_rows(&self) -> Option<usize> {
        self.choice_key(COMPOSER_ROWS).parse().ok()
    }

    fn set(&mut self, index: usize, value: usize) -> bool {
        let value = value.min(SETTINGS[index].value_count() - 1);
        let changed = self.values[index] != value;
        self.values[index] = value;
        changed
    }

    /// `key = value` lines for the settings file, every setting included.
    pub(crate) fn serialize(&self) -> String {
        let mut out: String = SETTINGS
            .iter()
            .zip(&self.values)
            .map(|(def, &value)| match def.kind {
                Kind::Text => format!("{} = {}\n", def.id, self.text(def.id)),
                _ => format!("{} = {}\n", def.id, def.value_key(value)),
            })
            .collect();
        if let Some(model) = &self.model {
            out.push_str(&format!(
                "{CHAT_MODEL} = {}:{}\n{CHAT_MODEL_NAME} = {}\n",
                model.provider.as_str(),
                model.id,
                model.name
            ));
        }
        out.push_str(&format!(
            "{CHAT_EFFORT} = {}\n",
            self.effort.map_or("off", |e| e.as_str())
        ));
        for (provider, hidden) in &self.hidden_models {
            if !hidden.is_empty() {
                let ids: Vec<&str> = hidden.iter().map(String::as_str).collect();
                out.push_str(&format!(
                    "providers.{provider}.hiddenModels = {}\n",
                    ids.join(" ")
                ));
            }
        }
        out
    }

    /// Settings read from `key = value` lines; unknown keys, unknown values
    /// and malformed lines keep the default.
    pub(crate) fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            if settings.parse_extra(key, value) {
                continue;
            }
            let Some(index) = SETTINGS.iter().position(|def| def.id == key) else {
                continue;
            };
            let def = &SETTINGS[index];
            if matches!(def.kind, Kind::Text) {
                settings.set_text(def.id, value);
                continue;
            }
            if let Some(found) = (0..def.value_count()).find(|&v| def.value_key(v) == value) {
                settings.values[index] = found;
            }
        }
        settings.tidy()
    }

    /// Chat and provider keys outside [`SETTINGS`]; true when handled.
    fn parse_extra(&mut self, key: &str, value: &str) -> bool {
        match key {
            CHAT_MODEL => {
                self.model = value.split_once(':').and_then(|(provider, id)| {
                    Some(SelectedModel {
                        provider: provider.parse().ok()?,
                        id: id.to_string(),
                        name: self
                            .model
                            .take()
                            .map_or_else(|| id.to_string(), |model| model.name),
                    })
                });
            }
            CHAT_MODEL_NAME => match &mut self.model {
                Some(model) => model.name = value.to_string(),
                // The name came first: park it until the model line.
                None => {
                    self.model = Some(SelectedModel {
                        provider: ProviderKind::OpenRouter,
                        id: String::new(),
                        name: value.to_string(),
                    })
                }
            },
            CHAT_EFFORT => self.effort = value.parse().ok(),
            _ => {
                let Some(provider) = key
                    .strip_prefix("providers.")
                    .and_then(|rest| rest.strip_suffix(".hiddenModels"))
                else {
                    return false;
                };
                self.hidden_models.insert(
                    provider.to_string(),
                    value.split_whitespace().map(str::to_string).collect(),
                );
            }
        }
        true
    }

    /// The saved settings, or the defaults when there is no readable file.
    pub(crate) fn load() -> Self {
        config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    /// Drop a model line whose id never arrived (a stray name line).
    fn tidy(mut self) -> Self {
        if self.model.as_ref().is_some_and(|model| model.id.is_empty()) {
            self.model = None;
        }
        self
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let path = config_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.serialize())
    }
}

/// `%APPDATA%\double-damage\dd-terminal.conf` on Windows, else under
/// `$XDG_CONFIG_HOME` or `~/.config`.
fn config_path() -> Option<PathBuf> {
    let env_dir = |name| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env_dir("APPDATA")
    } else {
        env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|home| home.join(".config")))
    }?;
    Some(base.join("double-damage").join("dd-terminal.conf"))
}

/// What a key or click in the menu did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuOutcome {
    Stay,
    /// A setting changed: the owner saves and applies it.
    Changed,
    Close,
    /// Start the provider's browser login.
    Login(ProviderKind),
    CancelLogin(ProviderKind),
    Logout(ProviderKind),
}

/// A row of the setting list: a blank spacer, a section heading or setting
/// `SETTINGS[i]`. Only items take the highlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Spacer,
    Heading(&'static str),
    Item(usize),
}

/// The in-place editor a `Kind::Text` row opens: the draft the keys and
/// paste go into, applied to the setting on Enter.
struct TextEdit {
    /// The `SETTINGS` index being edited.
    target: usize,
    /// Draft text; Enter stores it (trimmed), Esc drops it.
    text: String,
    /// Caret as a char index into `text`.
    cursor: usize,
}

/// What a choice submenu edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubTarget {
    Setting(usize),
    /// The provider's «Модель-классификатор»: only «Отключить» for now.
    Classifier,
}

/// The choice list open over the panel for one setting.
struct Submenu {
    target: SubTarget,
    selected: usize,
}

/// A submenu's title, description, options and current option.
struct SubSpec {
    title: &'static str,
    description: &'static str,
    options: Vec<(&'static str, &'static str)>,
    current: usize,
}

const CLASSIFIER_DESCRIPTION: &str = "Модель, которая заранее определяет тип запроса. \
                                      Классификаторов пока нет.";

fn sub_spec(target: SubTarget, settings: &Settings) -> SubSpec {
    match target {
        SubTarget::Setting(index) => {
            let def = &SETTINGS[index];
            let options = match def.kind {
                Kind::Choice(choices) => choices.iter().map(|c| (c.label, c.description)).collect(),
                Kind::Toggle => vec![("выкл", ""), ("вкл", "")],
                // Text rows open the editor, never a choice list.
                Kind::Text => Vec::new(),
            };
            SubSpec {
                title: def.label,
                description: def.description,
                options,
                current: settings.values[index],
            }
        }
        SubTarget::Classifier => SubSpec {
            title: "Модель-классификатор",
            description: CLASSIFIER_DESCRIPTION,
            options: vec![("Отключить", "")],
            current: 0,
        },
    }
}

/// Where the last frame painted what a click can land on.
#[derive(Default)]
struct Geometry {
    tab_row: u16,
    tabs: Vec<(Range<u16>, usize)>,
    rows: Rect,
    rows_start: usize,
    options: Rect,
    options_start: usize,
    /// Providers tab: the provider column, the pane rows and the two
    /// «Включить всё» / «Выключить всё» buttons.
    provider_list: Rect,
    pane: Rect,
    pane_start: usize,
    buttons: Vec<(u16, Range<u16>, providers_tab::Stop)>,
}

/// The open settings menu.
#[derive(Default)]
pub(crate) struct SettingsMenu {
    tab: usize,
    /// Highlighted row, an index into [`Self::rows`]; always on an item.
    selected: usize,
    submenu: Option<Submenu>,
    /// The text editor a `Kind::Text` row opened; while set it owns keys.
    text_edit: Option<TextEdit>,
    geometry: Geometry,
    /// Providers tab: highlighted provider, whether keys go to the pane
    /// beside the column, and the highlighted pane stop.
    provider: usize,
    pane_focus: bool,
    stop: usize,
    /// The cursor sits on the tab bar: only then ←/→ switch tabs.
    tabs_focus: bool,
}

/// Widest label column before values start.
const MAX_LABEL_WIDTH: usize = 30;
/// Description rows under the list, like oh-my-pi's fixed three.
const DESCRIPTION_ROWS: usize = 3;
pub(crate) const CURSOR: &str = "❯ ";
pub(crate) const DIM: Style = Style::new().fg(Color::DarkGray);

impl SettingsMenu {
    pub(crate) fn new() -> Self {
        // Opens with the cursor on the tab bar: ←/→ pick a category at once.
        let mut menu = Self {
            tabs_focus: true,
            ..Self::default()
        };
        menu.select_first();
        menu
    }

    /// The provider the Providers tab shows, whose data should be loaded.
    pub(crate) fn shown_provider(&self) -> Option<ProviderKind> {
        (TABS[self.tab] == Tab::Providers).then(|| ProviderKind::ALL[self.provider])
    }

    fn on_providers_tab(&self) -> bool {
        TABS[self.tab] == Tab::Providers
    }

    /// The list rows: the active tab's settings under their section
    /// headings, a spacer row before every heading but the first.
    fn rows(&self) -> Vec<Row> {
        let tab = TABS[self.tab];
        let mut rows = Vec::new();
        let mut group = None;
        for (index, def) in SETTINGS
            .iter()
            .enumerate()
            .filter(|(_, def)| def.tab == tab)
        {
            if group != Some(def.group) {
                if group.is_some() {
                    rows.push(Row::Spacer);
                }
                group = Some(def.group);
                rows.push(Row::Heading(def.group));
            }
            rows.push(Row::Item(index));
        }
        rows
    }

    fn select_first(&mut self) {
        self.selected = self
            .rows()
            .iter()
            .position(|row| matches!(row, Row::Item(_)))
            .unwrap_or(0);
    }

    /// The highlighted setting's index in [`SETTINGS`].
    fn selected_setting(&self, rows: &[Row]) -> Option<usize> {
        match rows.get(self.selected) {
            Some(Row::Item(index)) => Some(*index),
            _ => None,
        }
    }

    /// Move the highlight `forward` or back to the next item. Up from the
    /// first item climbs onto the tab bar; down from the last one stays.
    fn step(&mut self, rows: &[Row], forward: bool) {
        let items: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row, Row::Item(_)).then_some(index))
            .collect();
        let Some(current) = items.iter().position(|&index| index == self.selected) else {
            self.selected = items.first().copied().unwrap_or(0);
            return;
        };
        if forward {
            self.selected = items[(current + 1).min(items.len() - 1)];
        } else if current == 0 {
            self.tabs_focus = true;
        } else {
            self.selected = items[current - 1];
        }
    }

    /// Keys while the cursor is on the tab bar.
    fn tabs_key(&mut self, key: KeyEvent) -> MenuOutcome {
        match key.code {
            KeyCode::Esc => return MenuOutcome::Close,
            KeyCode::Left => self.switch_tab(false),
            KeyCode::Right => self.switch_tab(true),
            KeyCode::Down | KeyCode::Enter | KeyCode::Char(' ') => self.leave_tabs(),
            _ => {}
        }
        MenuOutcome::Stay
    }

    /// Step down from the tab bar onto the tab's first row.
    fn leave_tabs(&mut self) {
        self.tabs_focus = false;
        self.pane_focus = false;
        self.select_first();
    }

    /// ←/→ on setting `index`: the previous or next value in place.
    fn cycle(&mut self, index: usize, forward: bool, settings: &mut Settings) -> MenuOutcome {
        if matches!(SETTINGS[index].kind, Kind::Text) {
            return MenuOutcome::Stay;
        }
        let count = SETTINGS[index].value_count();
        let value = settings.values[index];
        let next = if forward {
            (value + 1) % count
        } else {
            value.checked_sub(1).unwrap_or(count - 1)
        };
        if settings.set(index, next) {
            MenuOutcome::Changed
        } else {
            MenuOutcome::Stay
        }
    }

    fn switch_tab(&mut self, forward: bool) {
        self.tab = if forward {
            (self.tab + 1) % TABS.len()
        } else {
            self.tab.checked_sub(1).unwrap_or(TABS.len() - 1)
        };
        self.select_first();
        self.pane_focus = false;
    }

    /// Enter on setting `index`: flip a switch, open a choice list.
    fn activate(&mut self, index: usize, settings: &mut Settings) -> MenuOutcome {
        match SETTINGS[index].kind {
            Kind::Toggle => {
                let value = 1 - settings.values[index].min(1);
                settings.set(index, value);
                MenuOutcome::Changed
            }
            Kind::Choice(_) => {
                self.submenu = Some(Submenu {
                    target: SubTarget::Setting(index),
                    selected: settings.values[index],
                });
                MenuOutcome::Stay
            }
            Kind::Text => {
                let text = settings.text(SETTINGS[index].id).to_string();
                self.text_edit = Some(TextEdit {
                    target: index,
                    cursor: text.chars().count(),
                    text,
                });
                MenuOutcome::Stay
            }
        }
    }

    /// Apply option `value` of the open submenu and close it.
    fn apply_submenu(&mut self, value: usize, settings: &mut Settings) -> MenuOutcome {
        let Some(submenu) = self.submenu.take() else {
            return MenuOutcome::Stay;
        };
        match submenu.target {
            SubTarget::Setting(index) if settings.set(index, value) => MenuOutcome::Changed,
            _ => MenuOutcome::Stay,
        }
    }

    /// Keys while a `Kind::Text` editor is open: editing keys, Enter to
    /// store the draft, Esc to drop it.
    fn text_edit_key(&mut self, key: KeyEvent, settings: &mut Settings) -> MenuOutcome {
        let Some(edit) = self.text_edit.as_mut() else {
            return MenuOutcome::Stay;
        };
        match key.code {
            KeyCode::Esc => self.text_edit = None,
            KeyCode::Enter => {
                settings.set_text(SETTINGS[edit.target].id, edit.text.trim());
                self.text_edit = None;
                return MenuOutcome::Changed;
            }
            KeyCode::Left => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::Right => edit.cursor = (edit.cursor + 1).min(edit.text.chars().count()),
            KeyCode::Home => edit.cursor = 0,
            KeyCode::End => edit.cursor = edit.text.chars().count(),
            KeyCode::Backspace if edit.cursor > 0 => {
                edit.cursor -= 1;
                edit.text = remove_char(&edit.text, edit.cursor);
            }
            KeyCode::Delete if edit.cursor < edit.text.chars().count() => {
                edit.text = remove_char(&edit.text, edit.cursor);
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                edit.text = insert_str(&edit.text, edit.cursor, &ch.to_string());
                edit.cursor += 1;
            }
            _ => {}
        }
        MenuOutcome::Stay
    }

    /// Pasted text while the menu owns the screen: into the open editor,
    /// flattened to one line so the file format stays `key = value`.
    pub(crate) fn handle_paste(&mut self, pasted: &str) {
        if let Some(edit) = self.text_edit.as_mut() {
            let flat: String = pasted
                .chars()
                .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
                .collect();
            let len = flat.chars().count();
            edit.text = insert_str(&edit.text, edit.cursor, &flat);
            edit.cursor += len;
        }
    }

    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        settings: &mut Settings,
        providers: &[ProviderEntry],
    ) -> MenuOutcome {
        if self.text_edit.is_some() {
            return self.text_edit_key(key, settings);
        }
        if let Some(submenu) = self.submenu.as_mut() {
            let count = sub_spec(submenu.target, settings).options.len();
            match key.code {
                KeyCode::Esc => self.submenu = None,
                KeyCode::Up => {
                    submenu.selected = submenu.selected.checked_sub(1).unwrap_or(count - 1);
                }
                KeyCode::Down | KeyCode::Tab => submenu.selected = (submenu.selected + 1) % count,
                KeyCode::Home => submenu.selected = 0,
                KeyCode::End => submenu.selected = count - 1,
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let value = submenu.selected;
                    return self.apply_submenu(value, settings);
                }
                _ => {}
            }
            return MenuOutcome::Stay;
        }
        if self.tabs_focus {
            return self.tabs_key(key);
        }
        if self.on_providers_tab() {
            return self.providers_key(key, settings, providers);
        }

        let rows = self.rows();
        match key.code {
            KeyCode::Esc => return MenuOutcome::Close,
            KeyCode::Up => self.step(&rows, false),
            KeyCode::Down => self.step(&rows, true),
            KeyCode::Home => self.select_first(),
            KeyCode::End => {
                if let Some(last) = rows.iter().rposition(|row| matches!(row, Row::Item(_))) {
                    self.selected = last;
                }
            }
            KeyCode::Left | KeyCode::Right => {
                if let Some(index) = self.selected_setting(&rows) {
                    return self.cycle(index, key.code == KeyCode::Right, settings);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(index) = self.selected_setting(&rows) {
                    return self.activate(index, settings);
                }
            }
            _ => {}
        }
        MenuOutcome::Stay
    }

    pub(crate) fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        settings: &mut Settings,
        providers: &[ProviderEntry],
    ) -> MenuOutcome {
        if self.text_edit.is_some() {
            return MenuOutcome::Stay;
        }
        let (column, row) = (mouse.column, mouse.row);
        let inside = |area: Rect| area.contains((column, row).into());
        let wheel = match mouse.kind {
            MouseEventKind::ScrollUp => Some(false),
            MouseEventKind::ScrollDown => Some(true),
            _ => None,
        };
        if let Some(submenu) = self.submenu.as_mut() {
            let count = sub_spec(submenu.target, settings).options.len();
            if let Some(forward) = wheel {
                submenu.selected = if forward {
                    (submenu.selected + 1).min(count - 1)
                } else {
                    submenu.selected.saturating_sub(1)
                };
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && inside(self.geometry.options)
            {
                let index = self.geometry.options_start + (row - self.geometry.options.y) as usize;
                if index < count {
                    if index == submenu.selected {
                        return self.apply_submenu(index, settings);
                    }
                    submenu.selected = index;
                }
            }
            return MenuOutcome::Stay;
        }

        if mouse.kind == MouseEventKind::Down(MouseButton::Left) && row == self.geometry.tab_row {
            if let Some((_, tab)) = self
                .geometry
                .tabs
                .iter()
                .find(|(columns, _)| columns.contains(&column))
            {
                self.tab = *tab;
                self.select_first();
                self.pane_focus = false;
                self.tabs_focus = true;
            }
            return MenuOutcome::Stay;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.tabs_focus = false;
        }
        if self.on_providers_tab() {
            return self.providers_mouse(mouse, settings, providers);
        }
        let rows = self.rows();
        if let Some(forward) = wheel {
            self.step(&rows, forward);
            return MenuOutcome::Stay;
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return MenuOutcome::Stay;
        }
        if inside(self.geometry.rows) {
            let index = self.geometry.rows_start + (row - self.geometry.rows.y) as usize;
            if let Some(Row::Item(setting)) = rows.get(index) {
                if index == self.selected {
                    return self.activate(*setting, settings);
                }
                self.selected = index;
            }
        }
        MenuOutcome::Stay
    }

    /// Paint the fullscreen menu over `area`.
    pub(crate) fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        settings: &Settings,
        providers: &[ProviderEntry],
    ) {
        self.geometry = Geometry::default();
        let accent = settings.accent();
        if area.width < 12 || area.height < 8 {
            Paragraph::new("Окно слишком мало для настроек").render(area, buf);
            return;
        }
        let rows = self.rows();
        if self.selected_setting(&rows).is_none() {
            self.select_first();
        }

        let top = area.y;
        let bottom = area.bottom() - 1;
        let inner_x = area.x + 2;
        let inner_width = area.width - 4;
        let inner = |y: u16, height: u16| Rect::new(inner_x, y, inner_width, height);

        // Frame: title border, tabs, divider, content, divider,
        // hint, bottom border.
        rule(buf, area, top, ('╭', '╮'), Some(("Настройки", accent)));
        for y in top + 1..bottom {
            buf.set_string(area.x, y, "│", DIM);
            buf.set_string(area.right() - 1, y, "│", DIM);
        }
        rule(buf, area, top + 2, ('├', '┤'), None);
        rule(buf, area, bottom - 2, ('├', '┤'), None);
        rule(buf, area, bottom, ('╰', '╯'), None);

        self.render_tabs(inner(top + 1, 1), buf, accent);

        let content_top = top + 3;
        let content = inner(content_top, (bottom - 2).saturating_sub(content_top));
        if let Some(edit) = &self.text_edit {
            Self::render_text_edit(content, buf, edit, accent);
        } else if let Some(submenu) = &self.submenu {
            let (target, selected) = (submenu.target, submenu.selected);
            self.render_submenu(content, buf, settings, target, selected, accent);
        } else if self.on_providers_tab() {
            self.render_providers(content, buf, settings, providers, accent);
        } else {
            self.render_list(content, buf, &rows, settings, accent);
        }

        let hint = if self.text_edit.is_some() {
            "ввод текста · Enter применить · Esc отмена"
        } else if self.submenu.is_some() {
            "↑/↓ выбрать · Enter применить · Esc назад"
        } else if self.tabs_focus {
            "←/→ вкладки · ↓ или Enter к настройкам · Esc закрыть"
        } else if self.on_providers_tab() && self.pane_focus {
            "↑/↓ выбрать · ←/→ перейти · Enter/Space изменить · Esc назад"
        } else if self.on_providers_tab() {
            "↑/↓ провайдер · → или Enter открыть · ↑ к вкладкам · Esc закрыть"
        } else {
            "↑/↓ выбрать · ←/→ или Enter изменить · ↑ к вкладкам · Esc закрыть"
        };
        Paragraph::new(Line::styled(truncate(hint, inner_width as usize), DIM))
            .render(inner(bottom - 1, 1), buf);
    }

    fn render_tabs(&mut self, area: Rect, buf: &mut Buffer, accent: Color) {
        self.geometry.tab_row = area.y;
        let mut spans = Vec::new();
        let mut x = area.x;
        for (index, &tab) in TABS.iter().enumerate() {
            let label = format!(" {} ", tab.label());
            let width = display_width(&label) as u16;
            // The bar lights up only while the cursor is on it.
            let style = if index == self.tab && self.tabs_focus {
                Style::new()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else if index == self.tab {
                Style::new()
                    .fg(accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::new().fg(Color::Gray)
            };
            self.geometry.tabs.push((x..x.saturating_add(width), index));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
            x = x.saturating_add(width + 1);
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }

    fn render_list(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        rows: &[Row],
        settings: &Settings,
        accent: Color,
    ) {
        if area.height == 0 {
            return;
        }
        // The list keeps a blank row and the description under it when the
        // panel has room for them.
        let reserve = DESCRIPTION_ROWS as u16 + 1;
        let list_height = if area.height > reserve + 1 {
            area.height - reserve
        } else {
            area.height
        };
        let pane = Rect::new(area.x, area.y, area.width, list_height);

        let visible = pane.height as usize;
        let start = self
            .selected
            .saturating_sub(visible / 2)
            .min(rows.len().saturating_sub(visible));
        let scrollbar = Scrollbar::new(pane, rows.len(), start);
        let text_width = if scrollbar.is_some() {
            pane.width.saturating_sub(2)
        } else {
            pane.width
        };
        let label_width = rows
            .iter()
            .filter_map(|row| match row {
                Row::Item(index) => Some(display_width(SETTINGS[*index].label)),
                Row::Heading(_) | Row::Spacer => None,
            })
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_WIDTH);
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, row)| match row {
                Row::Spacer => Line::default(),
                Row::Heading(name) => Line::from(vec![
                    Span::raw("  "),
                    Span::styled(*name, DIM.add_modifier(Modifier::BOLD)),
                ]),
                Row::Item(setting) => item_line(
                    *setting,
                    settings,
                    index == self.selected && !self.tabs_focus,
                    label_width,
                    text_width as usize,
                    accent,
                ),
            })
            .collect();
        let rows_area = Rect::new(pane.x, pane.y, text_width, pane.height);
        Paragraph::new(lines).render(rows_area, buf);
        if let Some(scrollbar) = scrollbar {
            scrollbar.render(buf, false);
        }
        self.geometry.rows = rows_area;
        self.geometry.rows_start = start;

        if list_height < area.height
            && let Some(index) = self.selected_setting(rows)
        {
            let description = Rect::new(
                area.x,
                area.y + list_height + 1,
                area.width,
                DESCRIPTION_ROWS as u16,
            );
            let lines = wrapped(
                SETTINGS[index].description,
                (area.width as usize).saturating_sub(4),
                DESCRIPTION_ROWS,
            )
            .into_iter()
            .map(|line| Line::styled(format!("  {line}"), Style::new().fg(Color::Gray)))
            .collect::<Vec<_>>();
            Paragraph::new(lines).render(description, buf);
        }
    }

    /// The `Kind::Text` editor: the row's title and description, then the
    /// draft with a `▏` caret, wrapped and scrolled to keep it in view.
    fn render_text_edit(area: Rect, buf: &mut Buffer, edit: &TextEdit, accent: Color) {
        let def = &SETTINGS[edit.target];
        let mut header = vec![Line::styled(
            format!("  {}", def.label),
            Style::new().fg(accent).add_modifier(Modifier::BOLD),
        )];
        header.extend(
            wrapped(def.description, (area.width as usize).saturating_sub(4), 2)
                .into_iter()
                .map(|line| Line::styled(format!("  {line}"), DIM)),
        );
        header.push(Line::default());
        let header_height = (header.len() as u16).min(area.height);
        Paragraph::new(header).render(Rect::new(area.x, area.y, area.width, header_height), buf);

        let body = Rect::new(
            area.x,
            area.y + header_height,
            area.width,
            area.height - header_height,
        );
        if body.height == 0 {
            return;
        }
        let byte = edit
            .text
            .char_indices()
            .nth(edit.cursor)
            .map_or(edit.text.len(), |(byte, _)| byte);
        let mut draft = String::with_capacity(edit.text.len() + 1);
        draft.push_str(&edit.text[..byte]);
        draft.push('▏');
        draft.push_str(&edit.text[byte..]);
        let width = (body.width as usize).saturating_sub(4).max(1);
        let lines: Vec<String> = wrap_line_at_whitespace_ranges(&draft, width)
            .into_iter()
            .map(|range| format!("  {}", draft[range].trim_end()))
            .collect();
        let caret_row = lines
            .iter()
            .position(|line| line.contains('▏'))
            .unwrap_or(0);
        let height = body.height as usize;
        let start = caret_row.saturating_sub(height.saturating_sub(1));
        let shown: Vec<Line> = lines
            .into_iter()
            .skip(start)
            .take(height)
            .map(Line::from)
            .collect();
        Paragraph::new(shown).render(body, buf);
    }

    fn render_submenu(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        settings: &Settings,
        target: SubTarget,
        selected: usize,
        accent: Color,
    ) {
        let spec = sub_spec(target, settings);
        let choices = &spec.options;
        let mut header = vec![Line::styled(
            format!("  {}", spec.title),
            Style::new().fg(accent).add_modifier(Modifier::BOLD),
        )];
        header.extend(
            wrapped(spec.description, (area.width as usize).saturating_sub(4), 2)
                .into_iter()
                .map(|line| Line::styled(format!("  {line}"), DIM)),
        );
        header.push(Line::default());
        let header_height = (header.len() as u16).min(area.height);
        Paragraph::new(header).render(Rect::new(area.x, area.y, area.width, header_height), buf);

        let options = Rect::new(
            area.x,
            area.y + header_height,
            area.width,
            area.height - header_height,
        );
        let visible = options.height as usize;
        let start = selected
            .saturating_sub(visible / 2)
            .min(choices.len().saturating_sub(visible));
        let label_width = choices
            .iter()
            .map(|choice| display_width(choice.0))
            .max()
            .unwrap_or(0);
        let current = spec.current;
        let lines: Vec<Line> = choices
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, choice)| {
                let is_selected = index == selected;
                let mut spans = vec![if is_selected {
                    Span::styled(CURSOR, Style::new().fg(accent))
                } else {
                    Span::raw("  ")
                }];
                let label_style = if is_selected {
                    Style::new().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                spans.push(Span::styled(pad(choice.0, label_width), label_style));
                spans.push(Span::styled(
                    if index == current { " ✓" } else { "  " },
                    Style::new().fg(Color::Green),
                ));
                if !choice.1.is_empty() {
                    spans.push(Span::styled(format!("  {}", choice.1), DIM));
                }
                Line::from(spans)
            })
            .collect();
        Paragraph::new(lines).render(options, buf);
        self.geometry.options = Rect::new(
            options.x,
            options.y,
            options.width,
            (choices.len() - start).min(visible) as u16,
        );
        self.geometry.options_start = start;
    }
}

/// One setting row: cursor, label padded to the label column, value; a
/// value changed from its default is bold.
fn item_line(
    index: usize,
    settings: &Settings,
    selected: bool,
    label_width: usize,
    width: usize,
    accent: Color,
) -> Line<'static> {
    let def = &SETTINGS[index];
    let value = settings.values[index];
    let label = pad(&truncate(def.label, label_width), label_width);
    let value_width = width.saturating_sub(2 + label_width + 2);
    let (value_text, changed) = match def.kind {
        Kind::Text => {
            let text = settings.text(def.id).trim();
            (
                truncate(if text.is_empty() { "не задан" } else { text }, value_width),
                !text.is_empty(),
            )
        }
        _ => (
            truncate(def.value_label(value), value_width),
            value != def.default,
        ),
    };
    let (label_style, value_style) = if selected {
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
        if selected {
            Span::styled(CURSOR, Style::new().fg(accent))
        } else {
            Span::raw("  ")
        },
        Span::styled(label, label_style),
        Span::raw("  "),
        Span::styled(value_text, value_style),
    ])
}

/// A horizontal frame rule across `area` on row `y` with corner glyphs
/// `ends`, optionally carrying a bold title inset like oh-my-pi's
/// `topBorder`.
pub(crate) fn rule(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    ends: (char, char),
    title: Option<(&str, Color)>,
) {
    let inner = (area.width as usize).saturating_sub(2);
    let mut spans = vec![Span::styled(ends.0.to_string(), DIM)];
    let mut used = 0;
    if let Some((title, color)) = title {
        let shown = truncate(&format!(" {title} "), inner.saturating_sub(2));
        used = 1 + display_width(&shown);
        spans.push(Span::styled("─", DIM));
        spans.push(Span::styled(
            shown,
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled("─".repeat(inner.saturating_sub(used)), DIM));
    spans.push(Span::styled(ends.1.to_string(), DIM));
    buf.set_line(area.x, y, &Line::from(spans), area.width);
}

/// `text` wrapped on whitespace to `width`, at most `max_rows` rows; a cut
/// last row ends with `…`.
fn wrapped(text: &str, width: usize, max_rows: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = wrap_line_at_whitespace_ranges(text, width)
        .into_iter()
        .map(|range| text[range].trim_end().to_string())
        .collect();
    if lines.len() > max_rows {
        lines.truncate(max_rows);
        if let Some(last) = lines.last_mut() {
            *last = truncate(&format!("{last}…"), width);
            if !last.ends_with('…') {
                last.push('…');
            }
        }
    }
    lines
}

/// `text` cut to `width` cells with a trailing `…` when it does not fit.
pub(crate) fn truncate(text: &str, width: usize) -> String {
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

/// `text` padded with spaces to `width` cells.
pub(crate) fn pad(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    out.extend(std::iter::repeat_n(
        ' ',
        width.saturating_sub(display_width(text)),
    ));
    out
}

/// `text` with `insert` pasted at char `index`.
fn insert_str(text: &str, index: usize, insert: &str) -> String {
    let byte = text
        .char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte);
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..byte]);
    out.push_str(insert);
    out.push_str(&text[byte..]);
    out
}

/// `text` without the char at `index`.
fn remove_char(text: &str, index: usize) -> String {
    let Some((byte, ch)) = text.char_indices().nth(index) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..byte]);
    out.push_str(&text[byte + ch.len_utf8()..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: ratatui::crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn screen(
        menu: &mut SettingsMenu,
        settings: &Settings,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        menu.render(area, &mut buf, settings, &[]);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// A menu with the cursor already stepped down into the first tab.
    pub(super) fn content_menu() -> SettingsMenu {
        let mut menu = SettingsMenu::new();
        menu.tabs_focus = false;
        menu
    }

    #[test]
    fn the_menu_opens_with_the_cursor_on_the_tabs() {
        let mut settings = Settings::default();
        let mut menu = SettingsMenu::new();
        assert!(menu.tabs_focus);
        menu.handle_key(key(KeyCode::Right), &mut settings, &[]);
        assert_eq!(menu.tab, 1);
        menu.handle_key(key(KeyCode::Down), &mut settings, &[]);
        assert!(!menu.tabs_focus);
        assert_eq!(
            menu.selected_setting(&menu.rows()),
            Some(setting_index(COMPOSER_ROWS))
        );
    }

    #[test]
    fn every_setting_has_a_valid_default() {
        for def in SETTINGS {
            assert!(def.default < def.value_count(), "{}", def.id);
        }
    }

    #[test]
    fn settings_round_trip_through_the_file_format() {
        let mut settings = Settings::default();
        settings.set(setting_index(ACCENT), 2);
        settings.set(setting_index(MOUSE_CAPTURE), 0);
        let parsed = Settings::parse(&settings.serialize());
        assert_eq!(parsed, settings);
        assert_eq!(parsed.accent(), Color::Magenta);
        assert!(!parsed.flag(MOUSE_CAPTURE));
    }

    #[test]
    fn chat_choices_and_hidden_models_round_trip() {
        let mut settings = Settings::default();
        assert_eq!(settings.model, None);
        assert_eq!(Settings::parse(&settings.serialize()), settings);
        settings.model = Some(SelectedModel {
            provider: ProviderKind::OpenRouter,
            id: "openai/gpt-5:free".into(),
            name: "OpenAI: GPT-5".into(),
        });
        settings.effort = None;
        settings.set_model_hidden(ProviderKind::Claude, "claude-a", true);
        settings.set_model_hidden(ProviderKind::Claude, "claude-b", true);
        let parsed = Settings::parse(&settings.serialize());
        assert_eq!(parsed, settings);
        assert!(parsed.model_hidden(ProviderKind::Claude, "claude-b"));
        assert!(!parsed.model_hidden(ProviderKind::Devin, "claude-b"));
        assert_eq!(effort_label(parsed.effort), "выкл");
    }

    #[test]
    fn parsing_ignores_unknown_keys_and_values() {
        let parsed =
            Settings::parse("nope = 1\ninput.composerRows = 99\ngarbage\ninput.composerRows=4");
        assert_eq!(parsed.composer_rows(), Some(4));
        assert_eq!(Settings::parse("").composer_rows(), None);
    }

    #[test]
    fn the_chat_tab_edits_the_system_prompt_and_round_trips() {
        let mut settings = Settings::default();
        let mut menu = content_menu();
        menu.tab = 2;
        menu.select_first();
        menu.tabs_focus = false;
        // Enter on «Системный промпт» opens the editor; typing fills it.
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &[]),
            MenuOutcome::Stay
        );
        assert!(
            screen(&mut menu, &settings, 90, 24)
                .iter()
                .any(|row| row.contains('▏'))
        );
        for ch in "sys".chars() {
            menu.handle_key(key(KeyCode::Char(ch)), &mut settings, &[]);
        }
        menu.handle_key(key(KeyCode::Left), &mut settings, &[]);
        menu.handle_key(key(KeyCode::Char('!')), &mut settings, &[]);
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &[]),
            MenuOutcome::Changed
        );
        assert_eq!(settings.system_prompt, "sy!s");
        assert_eq!(
            Settings::parse(&settings.serialize()).system_prompt,
            "sy!s"
        );
        assert!(
            screen(&mut menu, &settings, 90, 24)
                .iter()
                .any(|row| row.contains("Системный промпт") && row.contains("sy!s"))
        );

        // Esc drops the draft without touching the stored value.
        menu.handle_key(key(KeyCode::Enter), &mut settings, &[]);
        for ch in "junk".chars() {
            menu.handle_key(key(KeyCode::Char(ch)), &mut settings, &[]);
        }
        menu.handle_key(key(KeyCode::Esc), &mut settings, &[]);
        assert_eq!(settings.system_prompt, "sy!s");

        // Paste lands flattened to a single line.
        menu.handle_key(key(KeyCode::Enter), &mut settings, &[]);
        menu.handle_paste("a\nb\tc");
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &[]),
            MenuOutcome::Changed
        );
        assert_eq!(settings.system_prompt, "sy!sa b c");
    }

    #[test]
    fn enter_flips_a_toggle_and_reports_the_change() {
        let mut settings = Settings::default();
        let mut menu = content_menu();
        menu.switch_tab(true); // Ввод: composer rows first, then history recall.
        let rows = menu.rows();
        menu.step(&rows, true);
        assert_eq!(
            menu.selected_setting(&rows),
            Some(setting_index(HISTORY_RECALL))
        );
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &[]),
            MenuOutcome::Changed
        );
        assert!(!settings.flag(HISTORY_RECALL));
    }

    #[test]
    fn arrows_change_values_and_switch_tabs_only_from_the_tab_bar() {
        let mut settings = Settings::default();
        let mut menu = content_menu();
        // ←/→ on «Акцентный цвет» cycle its value, the tab stays.
        assert_eq!(
            menu.handle_key(key(KeyCode::Right), &mut settings, &[]),
            MenuOutcome::Changed
        );
        assert_eq!(settings.accent(), Color::Green);
        menu.handle_key(key(KeyCode::Left), &mut settings, &[]);
        assert_eq!(settings.accent(), Color::Cyan);
        assert_eq!(menu.tab, 0);
        // ↑ from the first row climbs onto the tabs; there → switches.
        menu.handle_key(key(KeyCode::Up), &mut settings, &[]);
        assert!(menu.tabs_focus);
        menu.handle_key(key(KeyCode::Right), &mut settings, &[]);
        assert_eq!(menu.tab, 1);
        menu.handle_key(key(KeyCode::Down), &mut settings, &[]);
        assert!(!menu.tabs_focus);
        menu.handle_key(key(KeyCode::Right), &mut settings, &[]);
        assert_eq!(menu.tab, 1);
    }

    #[test]
    fn a_choice_opens_a_submenu_that_applies_on_enter() {
        let mut settings = Settings::default();
        let mut menu = content_menu();
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &[]),
            MenuOutcome::Stay
        );
        assert!(menu.submenu.is_some());
        menu.handle_key(key(KeyCode::Down), &mut settings, &[]);
        assert_eq!(
            menu.handle_key(key(KeyCode::Enter), &mut settings, &[]),
            MenuOutcome::Changed
        );
        assert!(menu.submenu.is_none());
        assert_eq!(settings.accent(), Color::Green);
        // Esc from a submenu goes back without closing the menu.
        menu.handle_key(key(KeyCode::Enter), &mut settings, &[]);
        assert_eq!(
            menu.handle_key(key(KeyCode::Esc), &mut settings, &[]),
            MenuOutcome::Stay
        );
        assert_eq!(
            menu.handle_key(key(KeyCode::Esc), &mut settings, &[]),
            MenuOutcome::Close
        );
    }

    #[test]
    fn a_spacer_precedes_every_heading_but_the_first_and_is_skipped() {
        let mut menu = content_menu();
        let rows = menu.rows();
        assert_eq!(rows[0], Row::Heading("Тема"));
        assert_eq!(rows[2], Row::Spacer);
        assert_eq!(rows[3], Row::Heading("Сообщения"));
        // Down from «Акцентный цвет» jumps the spacer and the heading.
        menu.step(&rows, true);
        assert_eq!(
            menu.selected_setting(&rows),
            Some(setting_index(MESSAGE_BACKGROUND))
        );
        // Typing does nothing: there is no search.
        let mut settings = Settings::default();
        menu.handle_key(key(KeyCode::Char('ф')), &mut settings, &[]);
        assert_eq!(menu.rows(), rows);
    }

    #[test]
    fn panel_draws_the_frame_tabs_rows_and_description() {
        let settings = Settings::default();
        let mut menu = content_menu();
        let rows = screen(&mut menu, &settings, 80, 16);
        assert!(rows[0].starts_with("╭─ Настройки ─"));
        assert!(rows[1].contains(" Внешний вид   Ввод "));
        assert!(rows[2].starts_with("├─"));
        assert_eq!(
            rows[3],
            "│   Тема                                                                       │"
        );
        assert!(rows[4].starts_with("│ ❯ Акцентный цвет    Бирюзовый"));
        assert_eq!(rows[5].trim_end_matches('│').trim(), "│");
        assert!(rows[6].starts_with("│   Сообщения"));
        assert!(rows.iter().any(|row| row.contains("Цвет рамок поля ввода")));
        assert!(rows[15].starts_with("╰"));
        assert!(rows[14].contains("Esc закрыть"));
    }

    #[test]
    fn clicking_a_tab_switches_and_clicking_the_selected_row_activates_it() {
        let mut settings = Settings::default();
        let mut menu = content_menu();
        screen(&mut menu, &settings, 80, 16);
        let (columns, _) = menu.geometry.tabs[1].clone();
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        menu.handle_mouse(click(columns.start, 1), &mut settings, &[]);
        assert_eq!(menu.tab, 1);
        screen(&mut menu, &settings, 80, 16);
        let rows_y = menu.geometry.rows.y;
        let x = menu.geometry.rows.x;
        // Row 2 of the Ввод tab is «История по ↑/↓»: select, then flip.
        let target = (2 - menu.geometry.rows_start) as u16;
        assert_eq!(
            menu.handle_mouse(click(x, rows_y + target), &mut settings, &[]),
            MenuOutcome::Stay
        );
        assert_eq!(
            menu.handle_mouse(click(x, rows_y + target), &mut settings, &[]),
            MenuOutcome::Changed
        );
        assert!(!settings.flag(HISTORY_RECALL));
    }

    #[test]
    fn tiny_windows_do_not_panic() {
        let settings = Settings::default();
        let mut menu = content_menu();
        for (width, height) in [(1, 1), (11, 30), (12, 8), (20, 9), (200, 60)] {
            screen(&mut menu, &settings, width, height);
            menu.handle_key(key(KeyCode::Enter), &mut settings.clone(), &[]);
            screen(&mut menu, &settings, width, height);
            menu.submenu = None;
        }
    }
}
