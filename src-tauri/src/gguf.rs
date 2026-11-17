//! Reads the GGUF header of a local model: what's needed to estimate the KV cache and memory use.

use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use serde::Serialize;
use tauri::AppHandle;

use crate::tasks::{err_str, storage_subdir};

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    architecture: Option<String>,
    /// Context the model was trained for (the most it can use).
    pub context_length: Option<u64>,
    block_count: Option<u64>,
    embedding_length: Option<u64>,
    head_count: Option<u64>,
    /// KV heads per layer; 0 for layers without attention (hybrid models). Empty if unknown.
    head_count_kv: Vec<u64>,
    key_length: Option<u64>,
    value_length: Option<u64>,
    /// Sliding-window attention: window size and, if the file says so, which layers use it.
    sliding_window: Option<u64>,
    sliding_window_pattern: Vec<bool>,
    /// DeepSeek-style compressed attention (MLA): the cache holds this many values per token.
    kv_lora_rank: Option<u64>,
    rope_dimension_count: Option<u64>,
    vocab_size: Option<u64>,
}

enum Value {
    Int(u64),
    Float(f64),
    Str(String),
    Ints(Vec<u64>),
    Other,
}

/// Numeric arrays longer than this (token scores etc.) are skipped, not read.
const MAX_ARRAY: u64 = 1 << 16;
const TYPE_STRING: u32 = 8;
const TYPE_ARRAY: u32 = 9;

struct Reader(BufReader<File>);

impl Reader {
    fn bytes<const N: usize>(&mut self) -> std::io::Result<[u8; N]> {
        let mut buf = [0; N];
        self.0.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> std::io::Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    fn skip(&mut self, n: u64) -> std::io::Result<()> {
        self.0.seek_relative(n as i64)
    }
    fn string(&mut self) -> std::io::Result<String> {
        let len = self.u64()?;
        if len > 1 << 20 {
            return Err(std::io::Error::other("string too long"));
        }
        let mut buf = vec![0; len as usize];
        self.0.read_exact(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Size of a fixed-size scalar type, None for strings / arrays.
    fn scalar_size(kind: u32) -> Option<u64> {
        match kind {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        }
    }

    fn int(&mut self, kind: u32) -> std::io::Result<Option<u64>> {
        Ok(match kind {
            0 | 1 | 7 => Some(self.bytes::<1>()?[0] as u64),
            2 | 3 => Some(u16::from_le_bytes(self.bytes()?) as u64),
            4 => Some(self.u32()? as u64),
            5 => Some(i32::from_le_bytes(self.bytes()?).max(0) as u64),
            10 => Some(self.u64()?),
            11 => Some(i64::from_le_bytes(self.bytes()?).max(0) as u64),
            6 | 12 => {
                self.skip(Self::scalar_size(kind).unwrap_or(0))?;
                None
            }
            _ => return Err(std::io::Error::other(format!("unknown GGUF type {kind}"))),
        })
    }

    fn value(&mut self, kind: u32) -> std::io::Result<Value> {
        match kind {
            TYPE_STRING => Ok(Value::Str(self.string()?)),
            TYPE_ARRAY => {
                let item = self.u32()?;
                let n = self.u64()?;
                if item == TYPE_STRING {
                    for _ in 0..n {
                        let len = self.u64()?;
                        self.skip(len)?;
                    }
                    // Only the length matters (tokenizer vocabulary size).
                    return Ok(Value::Ints(vec![n]));
                }
                let size = Self::scalar_size(item).ok_or_else(|| std::io::Error::other("nested array"))?;
                if n > MAX_ARRAY || matches!(item, 6 | 12) {
                    self.skip(n * size)?;
                    return Ok(Value::Other);
                }
                let mut out = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    out.push(self.int(item)?.unwrap_or(0));
                }
                Ok(Value::Ints(out))
            }
            // Rounded to what an f32 actually carries, so 0.7 doesn't come out as 0.699999988.
            6 => Ok(Value::Float(((f32::from_le_bytes(self.bytes()?) as f64) * 1e6).round() / 1e6)),
            12 => Ok(Value::Float(f64::from_le_bytes(self.bytes()?))),
            _ => Ok(self.int(kind)?.map_or(Value::Other, Value::Int)),
        }
    }
}

fn read_kv(path: &Path) -> std::io::Result<HashMap<String, Value>> {
    let mut r = Reader(BufReader::with_capacity(1 << 16, File::open(path)?));
    if &r.bytes::<4>()? != b"GGUF" {
        return Err(std::io::Error::other("not a GGUF file"));
    }
    let version = r.u32()?;
    if version < 2 {
        return Err(std::io::Error::other("GGUF v1 is not supported"));
    }
    let _tensors = r.u64()?;
    let count = r.u64()?;
    let mut kv = HashMap::new();
    for _ in 0..count {
        let key = r.string()?;
        let kind = r.u32()?;
        kv.insert(key, r.value(kind)?);
    }
    Ok(kv)
}

fn read_info(path: &Path) -> std::io::Result<ModelInfo> {
    Ok(info_from(&read_kv(path)?))
}

fn info_from(kv: &HashMap<String, Value>) -> ModelInfo {

    let arch = match kv.get("general.architecture") {
        Some(Value::Str(arch)) => arch.clone(),
        _ => String::new(),
    };
    let get = |name: &str| kv.get(&format!("{arch}.{name}"));
    let int = |name: &str| match get(name) {
        Some(Value::Int(v)) => Some(*v),
        Some(Value::Ints(v)) => v.iter().copied().max(),
        _ => None,
    };
    let ints = |name: &str| match get(name) {
        Some(Value::Int(v)) => vec![*v],
        Some(Value::Ints(v)) => v.clone(),
        _ => Vec::new(),
    };

    let block_count = int("block_count");
    let mut head_count_kv = ints("attention.head_count_kv");
    if head_count_kv.is_empty() {
        head_count_kv = ints("attention.head_count");
    }
    // One value means "the same for every layer".
    if let (1, Some(n)) = (head_count_kv.len(), block_count) {
        head_count_kv = vec![head_count_kv[0]; n as usize];
    }
    ModelInfo {
        context_length: int("context_length"),
        block_count,
        embedding_length: int("embedding_length"),
        head_count: int("attention.head_count"),
        head_count_kv,
        key_length: int("attention.key_length"),
        value_length: int("attention.value_length"),
        sliding_window: int("attention.sliding_window"),
        sliding_window_pattern: ints("attention.sliding_window_pattern").into_iter().map(|v| v != 0).collect(),
        kv_lora_rank: int("attention.kv_lora_rank"),
        rope_dimension_count: int("rope.dimension_count"),
        vocab_size: int("vocab_size").or(match kv.get("tokenizer.ggml.tokens") {
            Some(Value::Ints(v)) => v.first().copied(),
            _ => None,
        }),
        architecture: (!arch.is_empty()).then_some(arch),
    }
}

/// Sampling defaults the model's author recommends (`general.sampling.*`); absent in most files.
#[derive(Serialize, Default, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sampling {
    pub temperature: Option<f64>,
    pub top_k: Option<u64>,
    pub top_p: Option<f64>,
    pub min_p: Option<f64>,
    pub repeat_penalty: Option<f64>,
    pub repeat_last_n: Option<u64>,
}

/// Everything the app needs to know about a model file without running it.
pub struct Meta {
    pub info: ModelInfo,
    pub chat_template: Option<String>,
    pub sampling: Sampling,
}

#[cfg(test)]
impl Meta {
    pub fn info_arch(&self) -> Option<&str> {
        self.info.architecture.as_deref()
    }
}

pub fn read_meta(path: &Path) -> std::io::Result<Meta> {
    let kv = read_kv(path)?;
    let float = |name: &str| match kv.get(&format!("general.sampling.{name}")) {
        Some(Value::Float(v)) => Some(*v),
        Some(Value::Int(v)) => Some(*v as f64),
        _ => None,
    };
    let int = |name: &str| match kv.get(&format!("general.sampling.{name}")) {
        Some(Value::Int(v)) => Some(*v),
        _ => None,
    };
    Ok(Meta {
        info: info_from(&kv),
        chat_template: match kv.get("tokenizer.chat_template") {
            Some(Value::Str(t)) if !t.is_empty() => Some(t.clone()),
            _ => None,
        },
        sampling: Sampling {
            temperature: float("temp"),
            top_k: int("top_k"),
            top_p: float("top_p"),
            min_p: float("min_p"),
            repeat_penalty: float("penalty_repeat"),
            repeat_last_n: int("penalty_last_n"),
        },
    })
}

/// Metadata of a local GGUF model (`name` is a file in the models folder; for a split model,
/// pass its first part).
#[tauri::command]
pub async fn model_info(app: AppHandle, name: String) -> Result<ModelInfo, String> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name == ".." {
        return Err(format!("invalid model file name: {name:?}"));
    }
    let path = storage_subdir(&app, "models")?.join(name);
    tauri::async_runtime::spawn_blocking(move || read_info(&path))
        .await
        .map_err(err_str)?
        .map_err(err_str)
}
