//! Minimal protobuf wire codec: just enough to encode requests to and decode replies from
//! the Connect endpoints the Devin provider talks to. Field numbers follow the extracted
//! `exa.*` schema; unknown fields are skipped on decode.

/// A protobuf message being built.
#[derive(Clone, Default)]
pub struct Msg(Vec<u8>);

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
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

impl Msg {
    pub fn new() -> Self {
        Self::default()
    }

    fn tag(&mut self, field: u32, wire: u64) {
        push_varint(&mut self.0, ((field as u64) << 3) | wire);
    }

    pub fn varint(&mut self, field: u32, value: u64) -> &mut Self {
        self.tag(field, 0);
        push_varint(&mut self.0, value);
        self
    }

    pub fn bytes(&mut self, field: u32, value: &[u8]) -> &mut Self {
        self.tag(field, 2);
        push_varint(&mut self.0, value.len() as u64);
        self.0.extend_from_slice(value);
        self
    }

    /// Empty strings are left out (proto3 would drop them anyway).
    pub fn string(&mut self, field: u32, value: &str) -> &mut Self {
        if !value.is_empty() {
            self.bytes(field, value.as_bytes());
        }
        self
    }

    pub fn msg(&mut self, field: u32, inner: &Msg) -> &mut Self {
        self.bytes(field, &inner.0)
    }

    pub fn f64(&mut self, field: u32, value: f64) -> &mut Self {
        self.tag(field, 1);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// A packed repeated varint field (enums and small ints).
    pub fn packed(&mut self, field: u32, varints: &[u64]) -> &mut Self {
        let mut inner = Vec::new();
        for value in varints {
            push_varint(&mut inner, *value);
        }
        self.bytes(field, &inner)
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

/// One decoded field value.
pub enum Val<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    F32(f32),
    F64(f64),
}

impl<'a> Val<'a> {
    pub fn as_u64(&self) -> u64 {
        match self {
            Val::Varint(v) => *v,
            Val::F32(v) => *v as u64,
            Val::F64(v) => *v as u64,
            Val::Bytes(_) => 0,
        }
    }

    pub fn as_str(&self) -> Option<&'a str> {
        match self {
            Val::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }
}

/// Iterates the fields of a protobuf message; malformed input just stops the iterator.
pub fn fields(data: &[u8]) -> Fields<'_> {
    Fields { data, pos: 0 }
}

pub struct Fields<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.data.get(self.pos)? as u64;
            self.pos += 1;
            value |= (byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u32, Val<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let tag = self.varint()?;
        let field = (tag >> 3) as u32;
        let value = match tag & 7 {
            0 => Val::Varint(self.varint()?),
            1 => {
                let bytes = self.data.get(self.pos..self.pos + 8)?;
                self.pos += 8;
                Val::F64(f64::from_le_bytes(bytes.try_into().ok()?))
            }
            2 => {
                let len = self.varint()? as usize;
                let bytes = self.data.get(self.pos..self.pos + len)?;
                self.pos += len;
                Val::Bytes(bytes)
            }
            5 => {
                let bytes = self.data.get(self.pos..self.pos + 4)?;
                self.pos += 4;
                Val::F32(f32::from_le_bytes(bytes.try_into().ok()?))
            }
            // Groups (3, 4) don't occur in this API; anything else is corrupt.
            _ => return None,
        };
        Some((field, value))
    }
}

/// Finds the first occurrence of a field, for messages read once.
pub fn get<'a>(data: &'a [u8], want: u32) -> Option<Val<'a>> {
    fields(data).find(|(field, _)| *field == want).map(|(_, value)| value)
}

/// Collects every occurrence of a field.
pub fn get_all<'a>(data: &'a [u8], want: u32) -> Vec<Val<'a>> {
    fields(data).filter(|(field, _)| *field == want).map(|(_, value)| value).collect()
}

/// Whether `data` parses as a message to the last byte.
fn is_message(data: &[u8]) -> bool {
    let mut it = fields(data);
    let mut count = 0;
    while it.next().is_some() {
        count += 1;
    }
    count > 0 && it.pos == data.len()
}

/// A schemaless dump of a message as JSON (`{"<field>": value or [values]}`): nested messages
/// where the bytes parse as one, text where they are UTF-8, otherwise the byte count.
/// Varints of `u64::MAX` (proto's -1, "unlimited") come out as -1.
pub fn to_json(data: &[u8], depth: usize) -> serde_json::Value {
    use serde_json::{Map, Value, json};
    let mut out = Map::new();
    for (field, value) in fields(data) {
        let value = match value {
            Val::Varint(v) if v == u64::MAX => json!(-1),
            Val::Varint(v) => json!(v),
            Val::F32(v) => json!(v),
            Val::F64(v) => json!(v),
            Val::Bytes(bytes) if depth > 0 && is_message(bytes) && std::str::from_utf8(bytes).is_err() => {
                to_json(bytes, depth - 1)
            }
            Val::Bytes(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) if depth > 0 && is_message(bytes) && text.chars().any(|c| c.is_control()) => {
                    to_json(bytes, depth - 1)
                }
                Ok(text) => json!(text),
                Err(_) => json!(format!("<{} bytes>", bytes.len())),
            },
        };
        match out.get_mut(&field.to_string()) {
            Some(Value::Array(list)) => list.push(value),
            Some(existing) => *existing = json!([existing.take(), value]),
            None => {
                out.insert(field.to_string(), value);
            }
        }
    }
    Value::Object(out)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut inner = Msg::new();
        inner.varint(1, 300).string(2, "hi");
        let mut msg = Msg::new();
        msg.string(1, "devin-cli")
            .varint(2, 5)
            .msg(3, &inner)
            .f64(4, 0.95)
            .packed(6, &[3, 4, 6, 7, 8])
            .string(7, "");
        let bytes = msg.into_vec();
        let got: Vec<_> = fields(&bytes).collect();
        assert_eq!(got.len(), 5);
        assert!(matches!(got[0], (1, Val::Bytes(b)) if b == b"devin-cli"));
        assert!(matches!(got[1], (2, Val::Varint(5))));
        assert!(matches!(got[3], (4, Val::F64(v)) if v == 0.95));
        assert!(matches!(got[4], (6, Val::Bytes(_))));
        if let (3, Val::Bytes(inner_bytes)) = &got[2] {
            assert!(matches!(get(inner_bytes, 1), Some(Val::Varint(300))));
            assert!(matches!(get(inner_bytes, 2).and_then(|v| v.as_str().map(str::to_owned)).as_deref(), Some("hi")));
        } else {
            panic!();
        }
    }

    #[test]
    fn overlong_and_truncated() {
        // Truncated input ends iteration instead of panicking.
        assert_eq!(fields(&[0x0a, 0x05, b'a']).count(), 0);
        assert_eq!(fields(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]).count(), 0);
    }
}
