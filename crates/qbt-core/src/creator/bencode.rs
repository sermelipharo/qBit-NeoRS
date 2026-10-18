//! Minimal bencode writer for generated .torrent files. Dictionaries are
//! `BTreeMap`s so keys are always emitted in raw byte order, as BEP 3
//! requires (and the info hash depends on).

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(BTreeMap<Vec<u8>, Value>),
}

impl Value {
    pub fn str(s: &str) -> Self {
        Value::Bytes(s.as_bytes().to_vec())
    }

    pub fn dict() -> Self {
        Value::Dict(BTreeMap::new())
    }

    /// Inserts `key` into a dictionary value (no-op for other kinds).
    pub fn insert(&mut self, key: &str, value: Value) {
        if let Value::Dict(d) = self {
            d.insert(key.as_bytes().to_vec(), value);
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Value::Int(i) => {
                out.push(b'i');
                out.extend_from_slice(i.to_string().as_bytes());
                out.push(b'e');
            }
            Value::Bytes(b) => encode_bytes(b, out),
            Value::List(l) => {
                out.push(b'l');
                for v in l {
                    v.encode_into(out);
                }
                out.push(b'e');
            }
            Value::Dict(d) => {
                out.push(b'd');
                for (k, v) in d {
                    encode_bytes(k, out);
                    v.encode_into(out);
                }
                out.push(b'e');
            }
        }
    }
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_sorted() {
        let mut d = Value::dict();
        d.insert("zeta", Value::Int(-3));
        d.insert("alpha", Value::List(vec![Value::str("x"), Value::Int(0)]));
        d.insert("", Value::Bytes(vec![0, 1]));
        assert_eq!(d.encode(), b"d0:2:\x00\x015:alphal1:xi0ee4:zetai-3ee".to_vec());
    }
}
