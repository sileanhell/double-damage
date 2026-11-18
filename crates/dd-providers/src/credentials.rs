//! Хранилище кредов: `credentials.toml` рядом с исполняемым файлом.
//!
//! ```toml
//! [openrouter]
//! api_key = "sk-or-v1-..."
//!
//! [devin]
//! session_token = "devin-session-token$..."
//! api_server_url = "https://server.codeium.com"
//! devin_api_url = "https://api.devin.ai"
//!
//! [claude]
//! access_token = "sk-ant-oat01-..."
//! refresh_token = "sk-ant-ort01-..."
//! expires_at_ms = 1763600000000
//! scopes = "user:profile user:inference ..."
//! subscription_type = "max"
//! rate_limit_tier = "default_claude_max_20x"
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::types::ProviderKind;

/// Путь к credentials.toml — рядом с бинарником, иначе в cwd.
pub fn credentials_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("credentials.toml")))
        .unwrap_or_else(|| PathBuf::from("credentials.toml"))
}

/// Чтение/запись секций провайдеров в credentials.toml.
/// Секция провайдера — плоская таблица строк (`api_key`, `session_token`, ...).
#[derive(Clone, Debug)]
pub struct CredentialsStore {
    path: PathBuf,
}

impl CredentialsStore {
    pub fn new() -> Self {
        Self { path: credentials_path() }
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    fn read_table(&self) -> Result<toml::Table> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => s.parse::<toml::Table>().map_err(|e| {
                Error::Store(format!("bad {}: {e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
            Err(e) => Err(Error::Store(format!(
                "read {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn write_table(&self, table: &toml::Table) -> Result<()> {
        let text = toml::to_string_pretty(table)
            .map_err(|e| Error::Store(format!("serialize credentials: {e}")))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        // Пишем через временный файл, чтобы не оставить обрезанный credentials.toml.
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| Error::Store(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| Error::Store(format!("rename {}: {e}", tmp.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Секция провайдера как карта ключ → значение.
    pub fn section(&self, kind: ProviderKind) -> Result<BTreeMap<String, String>> {
        let table = self.read_table()?;
        let mut out = BTreeMap::new();
        if let Some(toml::Value::Table(sec)) = table.get(kind.as_str()) {
            for (k, v) in sec {
                match v {
                    toml::Value::String(s) => {
                        out.insert(k.clone(), s.clone());
                    }
                    toml::Value::Integer(i) => {
                        out.insert(k.clone(), i.to_string());
                    }
                    toml::Value::Float(f) => {
                        out.insert(k.clone(), f.to_string());
                    }
                    toml::Value::Boolean(b) => {
                        out.insert(k.clone(), b.to_string());
                    }
                    toml::Value::Array(a) => {
                        let items: Vec<String> = a
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect();
                        out.insert(k.clone(), items.join(" "));
                    }
                    _ => {}
                }
            }
        }
        Ok(out)
    }

    /// Одно значение из секции провайдера.
    pub fn get(&self, kind: ProviderKind, key: &str) -> Result<Option<String>> {
        Ok(self.section(kind)?.get(key).map(String::clone))
    }

    /// Записать значения в секцию провайдера (сохраняя другие секции и ключи).
    pub fn set(&self, kind: ProviderKind, values: &[(String, String)]) -> Result<()> {
        let mut table = self.read_table()?;
        let entry = table
            .entry(kind.as_str().to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let sec = entry.as_table_mut().ok_or_else(|| {
            Error::Store(format!("section '{}' is not a table", kind.as_str()))
        })?;
        for (k, v) in values {
            sec.insert(k.clone(), toml::Value::String(v.clone()));
        }
        self.write_table(&table)
    }

    /// Удалить секцию провайдера (logout).
    pub fn remove(&self, kind: ProviderKind) -> Result<()> {
        let mut table = self.read_table()?;
        table.remove(kind.as_str());
        self.write_table(&table)
    }

    /// Провайдер авторизован (секция есть и не пуста).
    pub fn is_authenticated(&self, kind: ProviderKind) -> bool {
        self.section(kind).map(|s| !s.is_empty()).unwrap_or(false)
    }
}

impl Default for CredentialsStore {
    fn default() -> Self {
        Self::new()
    }
}
