//! Отпечаток Claude Code для OAuth-запросов (по oh-my-pi
//! `claude-code-fingerprint.ts` / `anthropic.ts`): User-Agent CLI, набор
//! `anthropic-beta`, billing-блок system с аттестацией `cch` и
//! «identity»-блок. Плюс разбор id модели: какой режим рассуждений она
//! понимает и сколько токенов можно просить.

use sha2::{Digest, Sha256};

use crate::types::Effort;

/// Версия Claude Code CLI, которой представляемся (oh-my-pi
/// `DEFAULT_CLAUDE_CODE_VERSION`).
pub const CLAUDE_CODE_VERSION: &str = "2.1.326";

/// Первый блок system у Claude Code.
pub const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const BILLING_PREFIX: &str = "x-anthropic-billing-header:";
const CCH_PLACEHOLDER: &str = "cch=00000";
const CCH_SEED: u64 = 0x4d65_9218_e32a_3268;
const FINGERPRINT_SALT: &str = "59cf53e54c78";

pub fn user_agent() -> String {
    format!("claude-cli/{CLAUDE_CODE_VERSION} (external, cli)")
}

/// `anthropic-beta` агентного запроса Claude Code; `effort` добавляется,
/// когда в теле есть `output_config.effort`.
pub fn betas(effort: bool) -> String {
    let mut betas = vec![
        "claude-code-20250219",
        "oauth-2025-04-20",
        "interleaved-thinking-2025-05-14",
    ];
    if effort {
        betas.push("effort-2025-11-24");
    }
    betas.join(",")
}

/// Billing-строка system[0]: `cc_version` с хэш-суффиксом от символов 4, 7
/// и 20 первого user-сообщения; `cch` патчится по готовому телу.
pub fn billing_header(first_user_text: &str) -> String {
    let chars: Vec<char> = first_user_text.chars().collect();
    let k: String = [4, 7, 20]
        .iter()
        .map(|&i| chars.get(i).copied().unwrap_or('0'))
        .collect();
    let digest = Sha256::digest(format!("{FINGERPRINT_SALT}{k}{CLAUDE_CODE_VERSION}").as_bytes());
    let suffix: String = digest
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .take(3)
        .map(|n| char::from_digit(n as u32, 16).unwrap_or('0'))
        .collect();
    format!(
        "{BILLING_PREFIX} cc_version={CLAUDE_CODE_VERSION}.{suffix}; cc_entrypoint=cli; {CCH_PLACEHOLDER};"
    )
}

/// Вписать аттестацию `cch` в сериализованное тело: xxHash64 тела с
/// плейсхолдером на месте, младшие 20 бит в hex. Плейсхолдер ищется в
/// последнем `"system":[` — `messages` сериализуются раньше (ключи
/// отсортированы), так что текст пользователя его не подменит.
pub fn patch_cch(body: &mut [u8]) {
    let Some(system) = rfind(body, br#""system":["#) else {
        return;
    };
    let Some(offset) = find(&body[system..], CCH_PLACEHOLDER.as_bytes()) else {
        return;
    };
    let at = system + offset + 4;
    let cch = format!("{:05x}", xxh64(body, CCH_SEED) & 0xfffff);
    body[at..at + 5].copy_from_slice(cch.as_bytes());
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

// --- xxHash64 ------------------------------------------------------------------

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(acc: u64, val: u64) -> u64 {
    (acc ^ round(0, val)).wrapping_mul(P1).wrapping_add(P4)
}

fn read64(data: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(data[i..i + 8].try_into().unwrap())
}

fn read32(data: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as u64
}

pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let len = data.len();
    let mut i = 0;
    let mut h = if len >= 32 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while i + 32 <= len {
            for (lane, acc) in v.iter_mut().enumerate() {
                *acc = round(*acc, read64(data, i + lane * 8));
            }
            i += 32;
        }
        let mut h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        for acc in v {
            h = merge(h, acc);
        }
        h
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(len as u64);
    while i + 8 <= len {
        h ^= round(0, read64(data, i));
        h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        i += 8;
    }
    if i + 4 <= len {
        h ^= read32(data, i).wrapping_mul(P1);
        h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        i += 4;
    }
    while i < len {
        h ^= (data[i] as u64).wrapping_mul(P5);
        h = h.rotate_left(11).wrapping_mul(P1);
        i += 1;
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^= h >> 32;
    h
}

// --- модели --------------------------------------------------------------------

/// Как модель включает рассуждения (oh-my-pi `defaultThinkingMode` +
/// правила `classes/anthropic.kdl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingMode {
    /// `thinking.type: "enabled"` + `budget_tokens`.
    Budget,
    /// Бюджет плюс `output_config.effort` (Opus 4.5).
    BudgetEffort,
    /// `thinking.type: "adaptive"` + `output_config.effort` (4.6+).
    Adaptive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Opus,
    Sonnet,
    Haiku,
    Other,
}

/// Что известно о модели по её id (`claude-opus-4-6`,
/// `claude-3-7-sonnet-20250219`, `claude-sonnet-4-5-20250929`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelTraits {
    pub family: Family,
    /// (major, minor); (0, 0) — не распознано.
    pub version: (u32, u32),
}

impl ModelTraits {
    pub fn parse(id: &str) -> Self {
        let id = id.to_ascii_lowercase();
        let parts: Vec<&str> = id.split(['-', '.', '@', ':']).collect();
        let family_at = parts
            .iter()
            .position(|p| matches!(*p, "opus" | "sonnet" | "haiku"));
        let family = match family_at.map(|i| parts[i]) {
            Some("opus") => Family::Opus,
            Some("sonnet") => Family::Sonnet,
            Some("haiku") => Family::Haiku,
            _ => Family::Other,
        };
        // Короткие числа (не даты) после семейства (новая схема) или до него.
        fn short(p: &str) -> Option<u32> {
            (p.len() <= 2 && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        }
        let mut version = Vec::new();
        if let Some(at) = family_at {
            for part in &parts[at + 1..] {
                match short(part) {
                    Some(n) => version.push(n),
                    None => break,
                }
            }
            if version.is_empty() {
                for part in parts[..at].iter().rev() {
                    match short(part) {
                        Some(n) => version.insert(0, n),
                        None => break,
                    }
                }
            }
        }
        Self {
            family,
            version: (
                version.first().copied().unwrap_or(0),
                version.get(1).copied().unwrap_or(0),
            ),
        }
    }

    fn at_least(&self, major: u32, minor: u32) -> bool {
        self.version >= (major, minor)
    }

    pub fn thinking_mode(&self) -> ThinkingMode {
        let adaptive = match self.family {
            Family::Haiku => self.at_least(5, 5),
            _ => self.at_least(4, 6),
        };
        if adaptive {
            ThinkingMode::Adaptive
        } else if self.family == Family::Opus && self.at_least(4, 5) {
            ThinkingMode::BudgetEffort
        } else {
            ThinkingMode::Budget
        }
    }

    /// Поддерживает ли модель extended thinking вообще (с Claude 3.7).
    pub fn supports_thinking(&self) -> bool {
        self.family == Family::Other || self.at_least(3, 7)
    }

    /// `output_config.effort` для `effort` на этой модели: Sonnet 4.6 не
    /// знает `max`, Opus 4.6 не знает `xhigh` (мы его и не шлём).
    pub fn output_effort(&self, effort: Effort) -> &'static str {
        let max_ok = !(self.family == Family::Sonnet && self.version == (4, 6));
        match effort {
            Effort::Minimal | Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High | Effort::XHigh => "high",
            Effort::Max if max_ok => "max",
            Effort::Max => "high",
        }
    }

    /// Потолок `max_tokens`, который модель примет.
    pub fn max_output_tokens(&self) -> u32 {
        match (self.family, self.version) {
            (Family::Opus, (4, 0 | 1)) => 32_000,
            (Family::Opus, (3, _)) | (Family::Haiku, (3, 0)) => 4_096,
            (Family::Haiku, (3, _)) | (Family::Sonnet, (3, 5)) => 8_192,
            _ => 64_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxh64_matches_reference_vectors() {
        assert_eq!(xxh64(b"", 0), 0xEF46_DB37_51D8_E999);
        assert_eq!(xxh64(b"abc", 0), 0x44BC_2CF5_AD77_0999);
        // Длиннее 32 байт: проходит через 4 аккумулятора.
        let long = b"Nobody inspects the spammish repetition; nobody at all.";
        assert_ne!(xxh64(long, 0), xxh64(long, 1));
    }

    #[test]
    fn model_ids_parse_family_and_version() {
        let t = ModelTraits::parse("claude-opus-4-6");
        assert_eq!((t.family, t.version), (Family::Opus, (4, 6)));
        assert_eq!(t.thinking_mode(), ThinkingMode::Adaptive);
        let t = ModelTraits::parse("claude-3-7-sonnet-20250219");
        assert_eq!((t.family, t.version), (Family::Sonnet, (3, 7)));
        assert_eq!(t.thinking_mode(), ThinkingMode::Budget);
        let t = ModelTraits::parse("claude-sonnet-4-5-20250929");
        assert_eq!(t.version, (4, 5));
        assert_eq!(t.thinking_mode(), ThinkingMode::Budget);
        assert_eq!(
            ModelTraits::parse("claude-opus-4-5-20251101").thinking_mode(),
            ThinkingMode::BudgetEffort
        );
        assert_eq!(
            ModelTraits::parse("claude-opus-4-1-20250805").max_output_tokens(),
            32_000
        );
        assert!(!ModelTraits::parse("claude-3-5-haiku-20241022").supports_thinking());
    }

    #[test]
    fn billing_header_gets_a_cch_attestation() {
        let header = billing_header("hello world, how are you?");
        assert!(header.starts_with("x-anthropic-billing-header: cc_version=2.1.326."));
        let mut body =
            format!(r#"{{"messages":[{{"text":"cch=00000"}}],"system":[{{"text":"{header}"}}]}}"#)
                .into_bytes();
        patch_cch(&mut body);
        let text = String::from_utf8(body).unwrap();
        // Текст пользователя не тронут, плейсхолдер в system заменён.
        assert!(text.starts_with(r#"{"messages":[{"text":"cch=00000"}]"#));
        assert_eq!(text.matches("cch=00000").count(), 1);
    }
}
