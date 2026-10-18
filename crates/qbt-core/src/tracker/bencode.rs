//! Minimal bencode writer for tracker replies. Dictionary keys must be
//! supplied in sorted order (BEP 3); `Dict` sorts them on construction.

pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(&'static str, Value)>),
}

impl Value {
    pub fn dict(mut entries: Vec<(&'static str, Value)>) -> Self {
        entries.sort_by(|a, b| a.0.cmp(b.0));
        Value::Dict(entries)
    }

    pub fn str(s: &str) -> Self {
        Value::Bytes(s.as_bytes().to_vec())
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Value::Int(i) => {
                out.push(b'i');
                out.extend_from_slice(i.to_string().as_bytes());
                out.push(b'e');
            }
            Value::Bytes(b) => {
                out.extend_from_slice(b.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(b);
            }
            Value::List(l) => {
                out.push(b'l');
                for v in l {
                    v.encode(out);
                }
                out.push(b'e');
            }
            Value::Dict(d) => {
                out.push(b'd');
                for (k, v) in d {
                    Value::Bytes(k.as_bytes().to_vec()).encode(out);
                    v.encode(out);
                }
                out.push(b'e');
            }
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.encode(&mut v);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_sorted_dict() {
        let v = Value::dict(vec![("peers", Value::Bytes(vec![1, 2])), ("interval", Value::Int(1800))]);
        assert_eq!(v.to_bytes(), b"d8:intervali1800e5:peers2:\x01\x02e".to_vec());
    }
}
