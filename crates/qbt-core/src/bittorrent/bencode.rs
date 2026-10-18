//! Minimal, allocation-bounded bencode reader/writer.
//!
//! Torrent files and resume data come from untrusted sources, so decoding is
//! iterative (no recursion, no stack overflow on deeply nested input) and
//! bounded by a nesting depth and a token count, like libtorrent's
//! `bdecode` limits (`bdecode_depth_limit`, `bdecode_token_limit`).

use std::collections::BTreeMap;

/// A decoded bencode value. Byte strings are kept as raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(BTreeMap<Vec<u8>, Value>),
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<String> {
        self.as_bytes().map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Value>> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_dict().and_then(|d| d.get(key.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(m: &str) -> Result<T, DecodeError> {
    Err(DecodeError(m.to_string()))
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_depth: usize,
    pub max_tokens: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_depth: 100, max_tokens: 10_000_000 }
    }
}

/// Validates the structure of `data` (one complete value, nothing trailing)
/// without building a tree. Used as a cheap guard before handing bytes to the
/// engine's recursive decoder.
pub fn validate(data: &[u8], limits: Limits) -> Result<(), DecodeError> {
    let end = scan_value(data, 0, limits)?;
    if end != data.len() {
        return err("trailing data after bencoded value");
    }
    Ok(())
}

/// Returns the end offset of the value starting at `start`.
pub fn scan_value(data: &[u8], start: usize, limits: Limits) -> Result<usize, DecodeError> {
    // Stack of open containers: true = dict expecting a key next.
    let mut stack: Vec<(bool, bool)> = Vec::new(); // (is_dict, expecting_key)
    let mut pos = start;
    let mut tokens = 0usize;
    loop {
        tokens += 1;
        if tokens > limits.max_tokens {
            return err("bencode token limit exceeded");
        }
        let Some(&c) = data.get(pos) else {
            return err("unexpected end of input");
        };
        if c == b'e' {
            if stack.pop().is_none() {
                return err("unexpected end marker");
            }
            pos += 1;
        } else {
            if let Some(top) = stack.last_mut()
                && top.0
                && top.1
                && !c.is_ascii_digit()
            {
                return err("dictionary key must be a string");
            }
            match c {
                b'i' => pos = scan_int(data, pos)?.1,
                b'0'..=b'9' => pos = scan_bytes(data, pos)?.1,
                b'l' | b'd' => {
                    if stack.len() >= limits.max_depth {
                        return err("bencode depth limit exceeded");
                    }
                    // The container itself counts as a value of its parent.
                    if let Some(top) = stack.last_mut()
                        && top.0
                    {
                        top.1 = !top.1;
                    }
                    stack.push((c == b'd', true));
                    pos += 1;
                    continue;
                }
                _ => return err("invalid bencode token"),
            }
            if let Some(top) = stack.last_mut()
                && top.0
            {
                top.1 = !top.1;
            }
        }
        if stack.is_empty() {
            return Ok(pos);
        }
        // A dict must not end while waiting for a value.
        if let Some(&(true, false)) = stack.last()
            && data.get(pos) == Some(&b'e')
        {
            return err("dictionary key without value");
        }
    }
}

fn scan_int(data: &[u8], pos: usize) -> Result<(i64, usize), DecodeError> {
    let rest = &data[pos + 1..];
    let Some(end) = rest.iter().position(|&b| b == b'e') else {
        return err("unterminated integer");
    };
    if end == 0 || end > 20 {
        return err("invalid integer");
    }
    let s = std::str::from_utf8(&rest[..end]).map_err(|_| DecodeError("invalid integer".into()))?;
    let v: i64 = s.parse().map_err(|_| DecodeError("invalid integer".into()))?;
    Ok((v, pos + 1 + end + 1))
}

fn scan_bytes(data: &[u8], pos: usize) -> Result<(usize, usize), DecodeError> {
    let rest = &data[pos..];
    let Some(colon) = rest.iter().take(21).position(|&b| b == b':') else {
        return err("invalid string length");
    };
    let s = std::str::from_utf8(&rest[..colon]).map_err(|_| DecodeError("invalid string length".into()))?;
    let len: usize = s.parse().map_err(|_| DecodeError("invalid string length".into()))?;
    let start = pos + colon + 1;
    let end = start.checked_add(len).ok_or_else(|| DecodeError("string too long".into()))?;
    if end > data.len() {
        return err("string exceeds input");
    }
    Ok((start, end))
}

/// Decodes one complete value (trailing bytes are an error).
pub fn decode(data: &[u8], limits: Limits) -> Result<Value, DecodeError> {
    validate(data, limits)?;
    let (v, _) = decode_at(data, 0)?;
    Ok(v)
}

/// Decodes a pre-validated value at `pos`. Iterative.
fn decode_at(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    enum Frame {
        List(Vec<Value>),
        Dict(BTreeMap<Vec<u8>, Value>, Option<Vec<u8>>),
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut pos = pos;
    loop {
        let c = *data.get(pos).ok_or_else(|| DecodeError("unexpected end".into()))?;
        let value = match c {
            b'l' => {
                stack.push(Frame::List(Vec::new()));
                pos += 1;
                continue;
            }
            b'd' => {
                stack.push(Frame::Dict(BTreeMap::new(), None));
                pos += 1;
                continue;
            }
            b'e' => {
                pos += 1;
                match stack.pop() {
                    Some(Frame::List(l)) => Value::List(l),
                    Some(Frame::Dict(d, _)) => Value::Dict(d),
                    None => return err("unexpected end marker"),
                }
            }
            b'i' => {
                let (v, end) = scan_int(data, pos)?;
                pos = end;
                Value::Int(v)
            }
            _ => {
                let (s, e) = scan_bytes(data, pos)?;
                pos = e;
                Value::Bytes(data[s..e].to_vec())
            }
        };
        match stack.last_mut() {
            None => return Ok((value, pos)),
            Some(Frame::List(l)) => l.push(value),
            Some(Frame::Dict(d, key)) => match key.take() {
                None => match value {
                    Value::Bytes(k) => *key = Some(k),
                    _ => return err("dictionary key must be a string"),
                },
                Some(k) => {
                    d.insert(k, value);
                }
            },
        }
    }
}

/// Finds the raw byte span of `key` in the top-level dictionary of `data`.
pub fn find_top_level_span(data: &[u8], key: &[u8], limits: Limits) -> Option<(usize, usize)> {
    if data.first() != Some(&b'd') {
        return None;
    }
    let mut pos = 1;
    while data.get(pos) != Some(&b'e') {
        let (ks, ke) = scan_bytes(data, pos).ok()?;
        let vstart = ke;
        let vend = scan_value(data, vstart, limits).ok()?;
        if &data[ks..ke] == key {
            return Some((vstart, vend));
        }
        pos = vend;
    }
    None
}

/// Bencode writer.
pub fn encode(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Int(i) => {
            out.push(b'i');
            out.extend_from_slice(i.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(b) => encode_bytes(b, out),
        Value::List(l) => {
            out.push(b'l');
            for x in l {
                encode(x, out);
            }
            out.push(b'e');
        }
        Value::Dict(d) => {
            out.push(b'd');
            for (k, x) in d {
                encode_bytes(k, out);
                encode(x, out);
            }
            out.push(b'e');
        }
    }
}

pub fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

/// A dictionary entry whose value is already bencoded (e.g. the raw `info`
/// dictionary, which must be preserved byte for byte).
pub enum RawOrValue<'a> {
    Raw(&'a [u8]),
    Value(Value),
}

/// Writes a dictionary with keys sorted as bencode requires.
pub fn encode_dict(entries: Vec<(&str, RawOrValue<'_>)>, out: &mut Vec<u8>) {
    let mut entries = entries;
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    out.push(b'd');
    for (k, v) in entries {
        encode_bytes(k.as_bytes(), out);
        match v {
            RawOrValue::Raw(r) => out.extend_from_slice(r),
            RawOrValue::Value(v) => encode(&v, out),
        }
    }
    out.push(b'e');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let data = b"d3:fooi42e4:listl1:a1:bee";
        let v = decode(data, Limits::default()).unwrap();
        assert_eq!(v.get("foo").and_then(Value::as_int), Some(42));
        let mut out = Vec::new();
        encode(&v, &mut out);
        assert_eq!(out, data);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [&b"d3:foo"[..], b"i12", b"l", b"5:ab", b"di1ei2ee", b"d3:fooe", b"x", b"i1ei2e"] {
            assert!(decode(bad, Limits::default()).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn depth_and_token_limits() {
        let deep: Vec<u8> = std::iter::repeat_n(b'l', 200).chain(std::iter::repeat_n(b'e', 200)).collect();
        assert!(decode(&deep, Limits::default()).is_err());
        assert!(decode(&deep, Limits { max_depth: 300, max_tokens: 1000 }).is_ok());
        let many = b"li1ei2ei3ei4ee";
        assert!(decode(many, Limits { max_depth: 10, max_tokens: 3 }).is_err());
        // Huge declared string length must not allocate or panic.
        assert!(decode(b"99999999999999999999:x", Limits::default()).is_err());
    }

    #[test]
    fn finds_raw_span() {
        let data = b"d8:announce3:abc4:infod4:name1:xee";
        let (s, e) = find_top_level_span(data, b"info", Limits::default()).unwrap();
        assert_eq!(&data[s..e], b"d4:name1:xe");
    }
}
