//! Chat sessions: the transcript of one conversation, stored as a small
//! binary file in `sessions/` next to the executable.
//!
//! File layout (all integers LEB128 varints, strings length-prefixed UTF-8):
//!
//! ```text
//! magic  "DDS\0"
//! u8     format version (3; 2 lacks the thought time, 1 the thinking)
//! varint created_ms, updated_ms
//! varint message count
//! per message: u8 role (0 user, 1 assistant, 2 error), string text,
//!              string model label (empty for user messages),
//!              string thinking (the model's reasoning, often empty),
//!              varint thought time in ms + 1 (0: unknown)
//! ```

use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 4] = b"DDS\0";
const VERSION: u8 = 3;
const EXTENSION: &str = "dds";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
    /// A failed request: shown in red, never sent to the model.
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub(crate) role: Role,
    pub(crate) text: String,
    /// Which model answered; empty for user messages.
    pub(crate) model: String,
    /// The reasoning the model streamed before answering.
    pub(crate) thinking: String,
    /// How long it reasoned, for the «Думал …» receipt.
    pub(crate) thought_ms: Option<u64>,
}

impl ChatMessage {
    pub(crate) fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            model: String::new(),
            thinking: String::new(),
            thought_ms: None,
        }
    }

    pub(crate) fn reply(role: Role, text: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            model: model.into(),
            thinking: String::new(),
            thought_ms: None,
        }
    }

    pub(crate) fn with_thinking(mut self, thinking: impl Into<String>) -> Self {
        self.thinking = thinking.into();
        self
    }

    pub(crate) fn with_thought(mut self, thought: Option<std::time::Duration>) -> Self {
        self.thought_ms = thought.map(|d| d.as_millis() as u64);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) created_ms: u64,
    pub(crate) updated_ms: u64,
    pub(crate) messages: Vec<ChatMessage>,
}

/// What the `/sessions` list shows without keeping whole transcripts.
#[derive(Clone, Debug)]
pub(crate) struct SessionSummary {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) updated_ms: u64,
    pub(crate) messages: usize,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Session {
    /// A fresh, unsaved session.
    pub(crate) fn new() -> Self {
        let now = now_ms();
        Self {
            id: format!("{now:013x}-{:04x}", std::process::id() & 0xffff),
            created_ms: now,
            updated_ms: now,
            messages: Vec::new(),
        }
    }

    /// The first line of the first user message, or a placeholder.
    pub(crate) fn title(&self) -> String {
        title_of(&self.messages)
    }

    /// Texts the user sent, oldest first, for ↑/↓ history recall.
    pub(crate) fn user_texts(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.text.clone())
            .collect()
    }

    pub(crate) fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
        self.updated_ms = now_ms();
    }

    pub(crate) fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            title: self.title(),
            updated_ms: self.updated_ms,
            messages: self.messages.len(),
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        put_varint(&mut out, self.created_ms);
        put_varint(&mut out, self.updated_ms);
        put_varint(&mut out, self.messages.len() as u64);
        for message in &self.messages {
            out.push(match message.role {
                Role::User => 0,
                Role::Assistant => 1,
                Role::Error => 2,
            });
            put_str(&mut out, &message.text);
            put_str(&mut out, &message.model);
            put_str(&mut out, &message.thinking);
            put_varint(&mut out, message.thought_ms.map_or(0, |ms| ms + 1));
        }
        out
    }

    pub(crate) fn decode(id: &str, bytes: &[u8]) -> io::Result<Self> {
        let mut reader = Reader { bytes, pos: 0 };
        if reader.take(4)? != MAGIC {
            return Err(invalid("not a session file"));
        }
        let version = reader.byte()?;
        if !(1..=VERSION).contains(&version) {
            return Err(invalid("unknown session format version"));
        }
        let created_ms = reader.varint()?;
        let updated_ms = reader.varint()?;
        let count = reader.varint()? as usize;
        let mut messages = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let role = match reader.byte()? {
                0 => Role::User,
                1 => Role::Assistant,
                2 => Role::Error,
                _ => return Err(invalid("bad message role")),
            };
            let text = reader.string()?;
            let model = reader.string()?;
            let thinking = if version >= 2 {
                reader.string()?
            } else {
                String::new()
            };
            let thought_ms = if version >= 3 {
                reader.varint()?.checked_sub(1)
            } else {
                None
            };
            messages.push(ChatMessage {
                role,
                text,
                model,
                thinking,
                thought_ms,
            });
        }
        Ok(Self {
            id: id.to_string(),
            created_ms,
            updated_ms,
            messages,
        })
    }

    /// Write the session; empty sessions are not stored.
    pub(crate) fn save(&self) -> io::Result<()> {
        if self.messages.is_empty() {
            return Ok(());
        }
        let dir = sessions_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.{EXTENSION}", self.id));
        // Through a temp file, so a crash never leaves half a session.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.encode())?;
        std::fs::rename(tmp, path)
    }

    pub(crate) fn load(id: &str) -> io::Result<Self> {
        let bytes = std::fs::read(sessions_dir().join(format!("{id}.{EXTENSION}")))?;
        Self::decode(id, &bytes)
    }
}

fn title_of(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| m.text.lines().find(|line| !line.trim().is_empty()))
        .map_or_else(
            || "Пустая сессия".to_string(),
            |line| line.trim().to_string(),
        )
}

/// `sessions/` next to the executable, else in the working directory.
pub(crate) fn sessions_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("sessions")))
        .unwrap_or_else(|| PathBuf::from("sessions"))
}

/// Every readable saved session, newest first.
pub(crate) fn list() -> Vec<SessionSummary> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    let mut sessions: Vec<SessionSummary> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != EXTENSION {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let bytes = std::fs::read(&path).ok()?;
            Session::decode(&id, &bytes).ok().map(|s| s.summary())
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    sessions
}

pub(crate) fn delete(id: &str) -> io::Result<()> {
    std::fs::remove_file(sessions_dir().join(format!("{id}.{EXTENSION}")))
}

/// «5 мин назад»-style age of a unix-ms instant.
pub(crate) fn ago(ms: u64) -> String {
    let seconds = now_ms().saturating_sub(ms) / 1000;
    match seconds {
        0..60 => "только что".to_string(),
        60..3600 => format!("{} мин назад", seconds / 60),
        3600..86_400 => format!("{} ч назад", seconds / 3600),
        _ => format!("{} дн назад", seconds / 86_400),
    }
}

// --- encoding --------------------------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    put_varint(out, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, len: usize) -> io::Result<&[u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| invalid("truncated session file"))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> io::Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(invalid("varint too long"))
    }

    fn string(&mut self) -> io::Result<String> {
        let len = self.varint()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| invalid("bad utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip_through_the_binary_format() {
        let mut session = Session::new();
        session.push(ChatMessage::user("привет\nвторая строка"));
        session.push(
            ChatMessage::reply(Role::Assistant, "здравствуйте", "Claude")
                .with_thinking("хм")
                .with_thought(Some(std::time::Duration::from_millis(1500))),
        );
        session.push(ChatMessage::reply(Role::Error, "http 500", "Claude"));
        let bytes = session.encode();
        assert_eq!(&bytes[..4], MAGIC);
        let decoded = Session::decode(&session.id, &bytes).unwrap();
        assert_eq!(decoded, session);
        assert_eq!(decoded.title(), "привет");
    }

    #[test]
    fn damaged_files_are_rejected_not_panicked_on() {
        let mut session = Session::new();
        session.push(ChatMessage::user("hi"));
        let bytes = session.encode();
        for len in 0..bytes.len() {
            assert!(Session::decode("x", &bytes[..len]).is_err());
        }
        assert!(Session::decode("x", b"nope").is_err());
        assert_eq!(Session::new().title(), "Пустая сессия");
    }
}
