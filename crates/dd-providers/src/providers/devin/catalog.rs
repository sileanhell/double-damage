//! Каталог Devin в виде «как у остальных»: по образцу oh-my-pi
//! (`catalog/src/discovery/devin.ts`) конфиги одного семейства с разным
//! уровнем рассуждений (`… Low`, `… High`, `… XHigh`) сворачиваются в одну
//! модель, а effort запроса выбирает конкретный wire-uid. Роутеры
//! (`adaptive`, `subagent-default`) и составные Fusion-пары не показываются.

use std::collections::{BTreeMap, HashMap};

use crate::proto;
use crate::types::{Effort, ModelInfo};

/// Уровень рассуждений Devin — наш [`Effort`], включая `Minimal`/`XHigh`.
type Level = Effort;

/// Имя уровня из метаданных или суффикса uid; `Err(())` — «без
/// рассуждений» (`none`, `No Thinking`).
fn parse_level(name: &str) -> Option<Result<Level, ()>> {
    let name: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match name.as_str() {
        "none" | "nothinking" | "off" => Err(()),
        "minimal" => Ok(Effort::Minimal),
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "xhigh" | "extrahigh" => Ok(Effort::XHigh),
        "max" => Ok(Effort::Max),
        _ => return None,
    })
}

/// Куда отправлять запрос семейства при каждом уровне.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Routes {
    /// uid «без рассуждений», если сервер такой даёт.
    pub off: Option<String>,
    pub levels: BTreeMap<Level, String>,
}

impl Routes {
    /// Wire-uid ровно для `effort` (`None` — «выкл»); `None`, если такого
    /// уровня у модели нет — подбора ближайшего нет.
    pub fn resolve(&self, effort: Option<Effort>) -> Option<&str> {
        match effort {
            None => self.off.as_deref(),
            Some(effort) => self.levels.get(&effort).map(String::as_str),
        }
    }
}

/// Нужные поля `ClientModelConfig` (номера из oh-my-pi `devin-proto.ts`).
#[derive(Clone, Debug, Default)]
pub struct RawConfig {
    pub uid: String,
    pub label: String,
    pub disabled: bool,
    /// `ModelInfo.displayOption`: 3 — роутер, 4/6 — внутренние слоты.
    pub display_option: u64,
    pub is_router: bool,
    pub family: Option<FamilyMeta>,
    pub info: ModelInfo,
}

#[derive(Clone, Debug, Default)]
pub struct FamilyMeta {
    pub label: String,
    /// (ключ, order, имя значения).
    pub entries: Vec<(String, u64, String)>,
    pub is_default: bool,
}

const DISPLAY_MODEL_ROUTER: u64 = 3;
const DISPLAY_QUICK_REVIEW: u64 = 4;
const DISPLAY_INTERNAL_DEFAULT: u64 = 6;

/// Разобрать `GetCliModelConfigsResponse`.
pub fn parse_configs(resp: &[u8]) -> Vec<RawConfig> {
    let mut out = Vec::new();
    for c in proto::msgs(resp, 1) {
        let Some(uid) = proto::str_val(c, 22).map(|u| u.trim().to_string()) else {
            continue;
        };
        if uid.is_empty() {
            continue;
        }
        let mi = proto::msgs(c, 23).into_iter().next();
        let (ctx, max_out) = match mi {
            Some(mi) => (
                proto::varint_val(c, 18)
                    .filter(|&v| v > 0)
                    .or_else(|| proto::varint_val(mi, 4)),
                proto::varint_val(mi, 13),
            ),
            None => (proto::varint_val(c, 18), None),
        };
        let (input_price, output_price) = prices(c);
        let label = proto::str_val(c, 1).unwrap_or_default().trim().to_string();
        let family = proto::msgs(c, 30).into_iter().next().map(|f| FamilyMeta {
            label: proto::str_val(f, 1).unwrap_or_default().trim().to_string(),
            entries: proto::msgs(f, 2)
                .into_iter()
                .filter_map(|e| {
                    let value = proto::msgs(e, 2).into_iter().next()?;
                    Some((
                        proto::str_val(e, 1).unwrap_or_default(),
                        proto::varint_val(value, 1).unwrap_or(0),
                        proto::str_val(value, 2).unwrap_or_default(),
                    ))
                })
                .collect(),
            is_default: proto::varint_val(f, 3) == Some(1) || proto::varint_val(c, 31) == Some(1),
        });
        out.push(RawConfig {
            info: ModelInfo {
                id: uid.clone(),
                display_name: if label.is_empty() {
                    uid.clone()
                } else {
                    label.clone()
                },
                description: proto::str_val(c, 27),
                context_window: ctx,
                max_output_tokens: max_out,
                input_price,
                output_price,
                ..Default::default()
            },
            uid,
            label,
            disabled: proto::varint_val(c, 4) == Some(1),
            display_option: mi.and_then(|mi| proto::varint_val(mi, 22)).unwrap_or(0),
            is_router: mi.and_then(|mi| proto::varint_val(mi, 25)) == Some(1),
            family,
        });
    }
    out
}

/// Цены input/output за 1M токенов из `modelDimensions` (поле 32) до
/// маркера `Sidekick`.
fn prices(c: &[u8]) -> (Option<f64>, Option<f64>) {
    let (mut input, mut output) = (None, None);
    for dim in proto::msgs(c, 32) {
        let name = proto::str_val(dim, 1)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if name == "sidekick" {
            break;
        }
        let price = proto::f32_val(dim, 2)
            .or_else(|| proto::f32_val(dim, 4))
            .or_else(|| proto::f32_val(dim, 5))
            .map(|v| (v as f64 * 1e6).round() / 1e6);
        match name.as_str() {
            "input" => input = price,
            "output" => output = price,
            _ => {}
        }
    }
    (input, output)
}

/// `GPT-5.6 Sol` → `gpt-5-6-sol`.
fn slugify(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn normalize_key(key: &str) -> String {
    key.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Одна полоса семейства (обычная / Fast / 1M) до сворачивания.
struct Lane {
    id: String,
    name: String,
    members: Vec<usize>,
    default_member: Option<usize>,
    off: Option<usize>,
    levels: BTreeMap<Level, usize>,
}

/// Полоса и уровень конфига по `modelFamilyMetadata`.
/// Уровень из метаданных: `Some(Err(()))` — «без рассуждений».
type LaneLevel = Option<Result<Level, ()>>;

fn family_lane(config: &RawConfig) -> Option<(String, String, LaneLevel)> {
    let meta = config.family.as_ref()?;
    if meta.label.is_empty() {
        return None;
    }
    let (mut level, mut thinking, mut fast, mut one_m) = (None, None, false, false);
    for (key, order, name) in &meta.entries {
        match normalize_key(key).as_str() {
            "fast mode" => fast = *order == 1,
            "thinking" => thinking = Some(*order == 1),
            "1m context" => one_m = *order == 1,
            "effort" | "reasoning effort" => level = parse_level(name),
            _ => {}
        }
    }
    // У Claude «High» есть и с рассуждениями, и без: решает ось Thinking.
    if thinking == Some(false) {
        level = Some(Err(()));
    }
    let base = slugify(&meta.label);
    if base.is_empty() {
        return None;
    }
    let id = format!(
        "{base}{}{}",
        if one_m { "-1m" } else { "" },
        if fast { "-fast" } else { "" }
    );
    let name = format!(
        "{}{}{}",
        meta.label,
        if one_m { " 1M" } else { "" },
        if fast { " Fast" } else { "" }
    );
    Some((id, name, level))
}

/// Запасной путь для конфигов без метаданных семейства: суффикс uid
/// `<base>-<level>[-fast]`.
fn suffix_lane(config: &RawConfig) -> Option<(String, String, Result<Level, ()>)> {
    let (uid, fast) = match config.uid.strip_suffix("-fast") {
        Some(rest) => (rest, true),
        None => (config.uid.as_str(), false),
    };
    let (base, last) = uid.rsplit_once('-')?;
    let level = parse_level(last)?;
    // «Kimi K3 High Fast» → «Kimi K3»: срезаем хвостовые слова уровня и Fast.
    let mut words: Vec<&str> = config.label.split_whitespace().collect();
    while let Some(last) = words.last() {
        if parse_level(last).is_some() || last.eq_ignore_ascii_case("fast") {
            words.pop();
        } else {
            break;
        }
    }
    let name = words.join(" ");
    let id = format!("{base}{}", if fast { "-fast" } else { "" });
    let name = format!(
        "{}{}",
        if name.is_empty() { base } else { &name },
        if fast { " Fast" } else { "" }
    );
    Some((id, name, level))
}

/// Свернуть каталог: модели для `/models` и маршруты effort → uid.
pub fn collapse(configs: Vec<RawConfig>) -> (Vec<ModelInfo>, HashMap<String, Routes>) {
    let mut seen = std::collections::HashSet::new();
    let live: Vec<RawConfig> = configs
        .into_iter()
        .filter(|c| {
            !c.disabled
                && !matches!(c.display_option, DISPLAY_QUICK_REVIEW | DISPLAY_INTERNAL_DEFAULT)
                // Роутеры (`adaptive`) и Fusion-пары — не модели.
                && c.display_option != DISPLAY_MODEL_ROUTER
                && !c.is_router
                && !c.uid.starts_with("fusion")
                && seen.insert(c.uid.clone())
        })
        .collect();

    let mut lanes: Vec<Lane> = Vec::new();
    let mut lane_of: HashMap<String, usize> = HashMap::new();
    let mut in_lane = vec![false; live.len()];
    let mut file = |id: String,
                    name: String,
                    index: usize,
                    level: Option<Result<Level, ()>>,
                    is_default: bool| {
        let at = *lane_of.entry(id.clone()).or_insert_with(|| {
            lanes.push(Lane {
                id,
                name,
                members: Vec::new(),
                default_member: None,
                off: None,
                levels: BTreeMap::new(),
            });
            lanes.len() - 1
        });
        let lane = &mut lanes[at];
        lane.members.push(index);
        if is_default && lane.default_member.is_none() {
            lane.default_member = Some(index);
        }
        match level {
            Some(Ok(level)) => {
                lane.levels.entry(level).or_insert(index);
            }
            Some(Err(())) => {
                lane.off.get_or_insert(index);
            }
            None => {}
        }
    };
    // Сначала серверные семейства, потом суффиксы uid у остальных.
    for (index, config) in live.iter().enumerate() {
        if let Some((id, name, level)) = family_lane(config) {
            let is_default = config.family.as_ref().is_some_and(|f| f.is_default);
            file(id, name, index, level, is_default);
        }
    }
    let family_ids: std::collections::HashSet<String> = lanes
        .iter()
        .filter(|lane| !lane.levels.is_empty())
        .flat_map(|lane| lane.members.iter().map(|&i| live[i].uid.clone()))
        .collect();
    let mut suffix_lanes: Vec<Lane> = Vec::new();
    {
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for (index, config) in live.iter().enumerate() {
            if family_ids.contains(&config.uid) {
                continue;
            }
            let Some((id, name, level)) = suffix_lane(config) else {
                continue;
            };
            let at = *by_id.entry(id.clone()).or_insert_with(|| {
                suffix_lanes.push(Lane {
                    id,
                    name,
                    members: Vec::new(),
                    default_member: None,
                    off: None,
                    levels: BTreeMap::new(),
                });
                suffix_lanes.len() - 1
            });
            let lane = &mut suffix_lanes[at];
            lane.members.push(index);
            match level {
                Ok(level) => {
                    lane.levels.entry(level).or_insert(index);
                }
                Err(()) => {
                    lane.off.get_or_insert(index);
                }
            }
        }
    }
    // Полоса без единого уровня рассуждений ничего не маршрутизирует, а
    // суффиксная из одного конфига — не семейство: такие остаются как есть.
    let lanes: Vec<Lane> = lanes
        .into_iter()
        .filter(|lane| !lane.levels.is_empty())
        .chain(
            suffix_lanes
                .into_iter()
                .filter(|lane| lane.members.len() >= 2 && !lane.levels.is_empty()),
        )
        .collect();

    let mut models = Vec::new();
    let mut routes = HashMap::new();
    for lane in &lanes {
        for &member in &lane.members {
            in_lane[member] = true;
        }
        let default = lane
            .default_member
            .or_else(|| lane.levels.get(&Level::Medium).copied())
            .or_else(|| lane.levels.values().next().copied())
            .unwrap_or(lane.members[0]);
        // Не даём полосе затереть отдельную модель с тем же id.
        if routes.contains_key(&lane.id) {
            continue;
        }
        let mut info = live[default].info.clone();
        info.id = lane.id.clone();
        info.display_name = lane.name.clone();
        // `/effort` предлагает ровно уровни семейства; «выкл» — только при
        // варианте без рассуждений.
        info.efforts = Some(lane.levels.keys().copied().collect());
        info.effort_off = lane.off.is_some();
        info.default_effort = lane
            .levels
            .iter()
            .find(|&(_, &i)| i == default)
            .map(|(&level, _)| level);
        models.push(info);
        routes.insert(
            lane.id.clone(),
            Routes {
                off: lane.off.map(|i| live[i].uid.clone()),
                levels: lane
                    .levels
                    .iter()
                    .map(|(&level, &i)| (level, live[i].uid.clone()))
                    .collect(),
            },
        );
    }
    for (index, config) in live.iter().enumerate() {
        if !in_lane[index] && !routes.contains_key(&config.uid) {
            // Одиночный конфиг: уровнем рассуждений Devin тут не управляет.
            let mut info = config.info.clone();
            info.efforts = Some(Vec::new());
            info.effort_off = true;
            models.push(info);
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    (models, routes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(uid: &str, label: &str) -> RawConfig {
        RawConfig {
            uid: uid.into(),
            label: label.into(),
            info: ModelInfo {
                id: uid.into(),
                display_name: label.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn family(
        mut c: RawConfig,
        label: &str,
        entries: &[(&str, u64, &str)],
        is_default: bool,
    ) -> RawConfig {
        c.family = Some(FamilyMeta {
            label: label.into(),
            entries: entries
                .iter()
                .map(|(k, o, n)| (k.to_string(), *o, n.to_string()))
                .collect(),
            is_default,
        });
        c
    }

    #[test]
    fn families_collapse_and_effort_picks_the_uid() {
        let mut adaptive = config("adaptive", "Adaptive");
        adaptive.display_option = DISPLAY_MODEL_ROUTER;
        let configs = vec![
            family(
                config("gpt-5-6-low", "GPT-5.6 Low"),
                "GPT-5.6",
                &[("Reasoning Effort", 0, "Low")],
                false,
            ),
            family(
                config("gpt-5-6-high", "GPT-5.6 High"),
                "GPT-5.6",
                &[("Reasoning Effort", 2, "High")],
                true,
            ),
            family(
                config("gpt-5-6-xhigh", "GPT-5.6 XHigh"),
                "GPT-5.6",
                &[("Reasoning Effort", 3, "X High")],
                false,
            ),
            family(
                config("claude-x", "Claude X"),
                "Claude X",
                &[("Thinking", 0, "Off"), ("Effort", 2, "High")],
                false,
            ),
            family(
                config("claude-x-thinking", "Claude X Thinking"),
                "Claude X",
                &[("Thinking", 1, "On"), ("Effort", 2, "High")],
                true,
            ),
            config("swe-1-7", "SWE-1.7"),
            config("kimi-k3-low", "Kimi K3 Low"),
            config("kimi-k3-high", "Kimi K3 High"),
            adaptive,
            config("fusion-gpt-5-6-high-sidekick-swe-1-7", "Fusion"),
        ];
        let (models, routes) = collapse(configs);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["claude-x", "gpt-5-6", "kimi-k3", "swe-1-7"]);
        assert_eq!(models[1].display_name, "GPT-5.6");
        assert_eq!(models[2].display_name, "Kimi K3");

        // Только объявленные уровни, без подбора ближайшего.
        let gpt = &routes["gpt-5-6"];
        assert_eq!(gpt.resolve(Some(Effort::High)), Some("gpt-5-6-high"));
        assert_eq!(gpt.resolve(Some(Effort::XHigh)), Some("gpt-5-6-xhigh"));
        assert_eq!(gpt.resolve(Some(Effort::Medium)), None);
        assert_eq!(gpt.resolve(Some(Effort::Max)), None);
        assert_eq!(gpt.resolve(None), None);
        assert_eq!(
            models[1].efforts.as_deref(),
            Some(&[Effort::Low, Effort::High, Effort::XHigh][..])
        );
        assert!(!models[1].effort_off);
        assert_eq!(models[1].default_effort, Some(Effort::High));
        assert_eq!(models[1].fit(Some(Effort::Medium)), Some(Effort::High));
        let claude = &routes["claude-x"];
        assert_eq!(claude.resolve(None), Some("claude-x"));
        assert_eq!(
            claude.resolve(Some(Effort::High)),
            Some("claude-x-thinking")
        );
        assert!(models[0].effort_off);
        assert_eq!(routes["kimi-k3"].resolve(Some(Effort::Max)), None);
        // Одиночная модель: уровней нет, только «выкл».
        assert_eq!(models[3].efforts.as_deref(), Some(&[][..]));
        assert_eq!(models[3].fit(Some(Effort::High)), None);
    }
}
