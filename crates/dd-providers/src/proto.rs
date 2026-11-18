//! Минимальный protobuf wire-format (для ConnectRPC Devin):
//! кодирование varint/len-delim/fixed + ленивое чтение полей без схемы.

/// Wire types.
pub const WT_VARINT: u8 = 0;
pub const WT_64BIT: u8 = 1;
pub const WT_LEN: u8 = 2;
pub const WT_32BIT: u8 = 5;

pub fn varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out
}

pub fn tag(field: u32, wt: u8) -> Vec<u8> {
    varint(((field as u64) << 3) | wt as u64)
}

/// Поле varint (int/bool/enum).
pub fn varint_field(field: u32, v: u64) -> Vec<u8> {
    let mut out = tag(field, WT_VARINT);
    out.extend(varint(v));
    out
}

/// Поле длины (str/bytes/msg/packed).
pub fn len_field(field: u32, data: &[u8]) -> Vec<u8> {
    let mut out = tag(field, WT_LEN);
    out.extend(varint(data.len() as u64));
    out.extend_from_slice(data);
    out
}

pub fn str_field(field: u32, s: &str) -> Vec<u8> {
    len_field(field, s.as_bytes())
}

pub fn f64_field(field: u32, v: f64) -> Vec<u8> {
    let mut out = tag(field, WT_64BIT);
    out.extend_from_slice(&v.to_le_bytes());
    out
}

/// Packed repeated varint (поле = len-delim из varint'ов).
pub fn packed_varints(field: u32, values: &[u64]) -> Vec<u8> {
    let mut inner = Vec::new();
    for v in values {
        inner.extend(varint(*v));
    }
    len_field(field, &inner)
}

// --- чтение -----------------------------------------------------------------

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift >= 70 {
            return None;
        }
    }
}

/// Одна запись из сообщения: (номер поля, wire type, сырое значение).
#[derive(Clone, Copy, Debug)]
pub struct Field<'a> {
    pub num: u32,
    pub wt: u8,
    pub value: Val<'a>,
}

#[derive(Clone, Copy, Debug)]
pub enum Val<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// Разобрать сообщение в список полей (без схемы).
pub fn fields(buf: &[u8]) -> Vec<Field<'_>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let Some(t) = read_varint(buf, &mut pos) else { break };
        let num = (t >> 3) as u32;
        let wt = (t & 7) as u8;
        let value = match wt {
            WT_VARINT => match read_varint(buf, &mut pos) {
                Some(v) => Val::Varint(v),
                None => break,
            },
            WT_64BIT => {
                if pos + 8 > buf.len() {
                    break;
                }
                let v = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap());
                pos += 8;
                Val::Fixed64(v)
            }
            WT_LEN => match read_varint(buf, &mut pos) {
                Some(len) => {
                    let len = len as usize;
                    if pos + len > buf.len() {
                        break;
                    }
                    let v = Val::Bytes(&buf[pos..pos + len]);
                    pos += len;
                    v
                }
                None => break,
            },
            WT_32BIT => {
                if pos + 4 > buf.len() {
                    break;
                }
                let v = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
                pos += 4;
                Val::Fixed32(v)
            }
            _ => break,
        };
        out.push(Field { num, wt, value });
    }
    out
}

/// Все len-delim значения поля `num` (для repeated msg/str).
pub fn msgs<'a>(buf: &'a [u8], num: u32) -> Vec<&'a [u8]> {
    fields(buf)
        .into_iter()
        .filter_map(|f| match (f.num, f.value) {
            (n, Val::Bytes(b)) if n == num => Some(b),
            _ => None,
        })
        .collect()
}

/// Первое строковое значение поля `num`.
pub fn str_val(buf: &[u8], num: u32) -> Option<String> {
    msgs(buf, num)
        .into_iter()
        .next()
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

/// Первое varint-значение поля `num`.
pub fn varint_val(buf: &[u8], num: u32) -> Option<u64> {
    fields(buf).into_iter().find_map(|f| match (f.num, f.value) {
        (n, Val::Varint(v)) if n == num => Some(v),
        _ => None,
    })
}

/// Первое 32-bit float-значение поля `num`.
pub fn f32_val(buf: &[u8], num: u32) -> Option<f32> {
    fields(buf).into_iter().find_map(|f| match (f.num, f.value) {
        (n, Val::Fixed32(v)) if n == num => Some(f32::from_bits(v)),
        _ => None,
    })
}

/// Первое 64-bit float-значение поля `num`.
pub fn f64_val(buf: &[u8], num: u32) -> Option<f64> {
    fields(buf).into_iter().find_map(|f| match (f.num, f.value) {
        (n, Val::Fixed64(v)) if n == num => Some(f64::from_bits(v)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut m = Vec::new();
        m.extend(str_field(1, "hello"));
        m.extend(varint_field(2, 128000));
        m.extend(f64_field(3, 0.95));
        let fs = fields(&m);
        assert_eq!(fs.len(), 3);
        assert_eq!(str_val(&m, 1).as_deref(), Some("hello"));
        assert_eq!(varint_val(&m, 2), Some(128000));
        assert_eq!(f64_val(&m, 3), Some(0.95));
    }
}
