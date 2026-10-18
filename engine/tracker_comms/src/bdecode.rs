//! A small bencode decoder for untrusted input (tracker responses).
//!
//! It never panics, bounds recursion depth and the number of decoded values,
//! limits integer and length prefixes, and ignores trailing data (some trackers
//! append a newline). Values borrow from the input buffer.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    Dict(Vec<(&'a [u8], Value<'a>)>),
}

impl<'a> Value<'a> {
    /// The value of `key` in a dictionary (first occurrence).
    pub fn get(&self, key: &[u8]) -> Option<&Value<'a>> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&[(&'a [u8], Value<'a>)]> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum nesting of lists/dictionaries.
    pub max_depth: usize,
    /// Maximum number of decoded values (integers, strings, lists, dicts).
    pub max_values: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_values: 100_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    UnexpectedEof,
    Invalid(&'static str),
    TooDeep,
    TooManyValues,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEof => f.write_str("unexpected end of bencoded data"),
            DecodeError::Invalid(what) => write!(f, "invalid bencode: {what}"),
            DecodeError::TooDeep => f.write_str("bencode nesting too deep"),
            DecodeError::TooManyValues => f.write_str("too many bencoded values"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode the first bencoded value in `buf`, ignoring any trailing bytes.
pub fn decode(buf: &[u8], limits: Limits) -> Result<Value<'_>, DecodeError> {
    let mut d = Decoder {
        buf,
        pos: 0,
        values: 0,
        limits,
    };
    d.value(0)
}

struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
    values: usize,
    limits: Limits,
}

impl<'a> Decoder<'a> {
    fn peek(&self) -> Result<u8, DecodeError> {
        self.buf
            .get(self.pos)
            .copied()
            .ok_or(DecodeError::UnexpectedEof)
    }

    fn count(&mut self) -> Result<(), DecodeError> {
        self.values += 1;
        if self.values > self.limits.max_values {
            return Err(DecodeError::TooManyValues);
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>, DecodeError> {
        self.count()?;
        match self.peek()? {
            b'i' => {
                self.pos += 1;
                let i = self.int_until(b'e')?;
                Ok(Value::Int(i))
            }
            b'l' => {
                if depth >= self.limits.max_depth {
                    return Err(DecodeError::TooDeep);
                }
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    if self.peek()? == b'e' {
                        self.pos += 1;
                        return Ok(Value::List(items));
                    }
                    items.push(self.value(depth + 1)?);
                }
            }
            b'd' => {
                if depth >= self.limits.max_depth {
                    return Err(DecodeError::TooDeep);
                }
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    if self.peek()? == b'e' {
                        self.pos += 1;
                        return Ok(Value::Dict(items));
                    }
                    if !self.peek()?.is_ascii_digit() {
                        return Err(DecodeError::Invalid("dictionary key is not a string"));
                    }
                    self.count()?;
                    let key = self.bytes()?;
                    let value = self.value(depth + 1)?;
                    items.push((key, value));
                }
            }
            b'0'..=b'9' => Ok(Value::Bytes(self.bytes()?)),
            _ => Err(DecodeError::Invalid("unexpected byte")),
        }
    }

    /// Parse a decimal integer terminated by `end`. Leading zeros are tolerated.
    fn int_until(&mut self, end: u8) -> Result<i64, DecodeError> {
        let start = self.pos;
        let rest = &self.buf[start..];
        let len = rest
            .iter()
            .position(|&b| b == end)
            .ok_or(DecodeError::UnexpectedEof)?;
        let digits = &rest[..len];
        self.pos = start + len + 1;
        let (negative, digits) = match digits.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, digits),
        };
        if digits.is_empty() || digits.len() > 19 || !digits.iter().all(u8::is_ascii_digit) {
            return Err(DecodeError::Invalid("malformed integer"));
        }
        let mut value: i64 = 0;
        for &d in digits {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(i64::from(d - b'0')))
                .ok_or(DecodeError::Invalid("integer overflow"))?;
        }
        Ok(if negative { -value } else { value })
    }

    fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.int_until(b':')?;
        let len = usize::try_from(len).map_err(|_| DecodeError::Invalid("negative length"))?;
        let end = self
            .pos
            .checked_add(len)
            .ok_or(DecodeError::Invalid("length overflow"))?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(DecodeError::UnexpectedEof)?;
        self.pos = end;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(buf: &[u8]) -> Result<Value<'_>, DecodeError> {
        decode(buf, Limits::default())
    }

    #[test]
    fn decodes_basic_values() {
        assert_eq!(d(b"i42e").unwrap(), Value::Int(42));
        assert_eq!(d(b"i-7e").unwrap(), Value::Int(-7));
        assert_eq!(d(b"4:spam").unwrap(), Value::Bytes(b"spam"));
        assert_eq!(d(b"0:").unwrap(), Value::Bytes(b""));
        assert_eq!(
            d(b"l4:spami1ee").unwrap(),
            Value::List(vec![Value::Bytes(b"spam"), Value::Int(1)])
        );
        let v = d(b"d3:cow3:moo4:spaml1:a1:bee").unwrap();
        assert_eq!(v.get(b"cow"), Some(&Value::Bytes(b"moo")));
        assert_eq!(
            v.get(b"spam").and_then(|v| v.as_list()).map(|l| l.len()),
            Some(2)
        );
        assert_eq!(v.get(b"missing"), None);
    }

    #[test]
    fn ignores_trailing_data() {
        assert_eq!(d(b"i1e\n\r garbage").unwrap(), Value::Int(1));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            &b""[..],
            b"i",
            b"ie",
            b"i-e",
            b"i12",
            b"i1x2e",
            b"5:abc",
            b"-1:a",
            b"l",
            b"li1e",
            b"d",
            b"di1ei2ee",
            b"d1:a",
            b"x",
            b"i99999999999999999999e",
            b"99999999999999999999:a",
            b"18446744073709551615:a",
        ] {
            assert!(
                d(bad).is_err(),
                "{:?} should fail",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn depth_limit() {
        let mut deep = vec![b'l'; 10_000];
        deep.extend(vec![b'e'; 10_000]);
        assert_eq!(d(&deep), Err(DecodeError::TooDeep));

        let mut ok = vec![b'l'; 16];
        ok.extend(vec![b'e'; 16]);
        assert!(d(&ok).is_ok());
        let mut too_deep = vec![b'l'; 17];
        too_deep.extend(vec![b'e'; 17]);
        assert_eq!(d(&too_deep), Err(DecodeError::TooDeep));

        let mut deep_dicts = Vec::new();
        for _ in 0..1000 {
            deep_dicts.extend_from_slice(b"d1:a");
        }
        assert!(d(&deep_dicts).is_err());
    }

    #[test]
    fn value_limit() {
        let mut many = b"l".to_vec();
        for _ in 0..200_000 {
            many.extend_from_slice(b"i0e");
        }
        many.push(b'e');
        assert_eq!(d(&many), Err(DecodeError::TooManyValues));
    }

    /// Deterministic pseudo-random fuzzing: the decoder must never panic, and
    /// must decode every prefix/mutation without overflowing the stack.
    #[test]
    fn fuzz_never_panics() {
        let seeds: [&[u8]; 4] = [
            b"d8:intervali1800e5:peers6:iiiipp6:peers618:iiiiiiiiiiiiiiiippe",
            b"d14:failure reason4:nope8:retry in5:nevere",
            b"d5:filesd20:aaaaaaaaaaaaaaaaaaaad8:completei5e10:downloadedi50e10:incompletei10eeee",
            b"d5:peersld2:ip9:127.0.0.14:porti100eed2:ip3:::14:porti1eeee",
        ];
        let mut state: u64 = 0x9e3779b97f4a7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for seed in seeds {
            for cut in 0..=seed.len() {
                let _ = d(&seed[..cut]);
            }
            for _ in 0..5_000 {
                let mut buf = seed.to_vec();
                let mutations = (next() % 4 + 1) as usize;
                for _ in 0..mutations {
                    let r = next();
                    let pos = (r as usize) % buf.len().max(1);
                    match r % 3 {
                        0 if !buf.is_empty() => buf[pos] = (r >> 8) as u8,
                        1 => buf.insert(
                            pos.min(buf.len()),
                            b"ield0123456789:-"[(r >> 16) as usize % 16],
                        ),
                        _ if !buf.is_empty() => {
                            buf.remove(pos);
                        }
                        _ => {}
                    }
                }
                let _ = d(&buf);
            }
        }
    }
}
