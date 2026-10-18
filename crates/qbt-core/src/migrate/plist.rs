//! Reader of Apple property lists (XML and binary `bplist00`) and of
//! `NSKeyedArchiver` archives inside them, enough for the files of the
//! Transmission app (`Transfers.plist`, its defaults domain, the archived
//! `GroupDicts`). Read only; bounded in depth and size.

use std::collections::BTreeMap;

use base64::Engine as _;
use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    Bool(bool),
    Int(i64),
    Real(f64),
    /// Seconds since 2001-01-01 (binary) or the ISO 8601 text (XML) as
    /// Unix seconds.
    Date(f64),
    Data(Vec<u8>),
    String(String),
    Array(Vec<Plist>),
    Dict(BTreeMap<String, Plist>),
    /// `NSKeyedArchiver` object reference.
    Uid(u64),
    /// `$null` of an archive.
    Null,
}

const MAX_DEPTH: usize = 64;
/// Seconds between 1970-01-01 and 2001-01-01.
const APPLE_EPOCH: f64 = 978_307_200.0;

impl Plist {
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(d) => d.get(key),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<String, Plist>> {
        match self {
            Plist::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Plist]> {
        match self {
            Plist::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::String(s) => Some(s),
            _ => None,
        }
    }

    /// Booleans; also integers and "YES"/"1"-like strings, which
    /// `NSUserDefaults boolForKey:` accepts too.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Plist::Bool(b) => Some(*b),
            Plist::Int(i) => Some(*i != 0),
            Plist::Real(r) => Some(*r != 0.0),
            Plist::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "yes" | "true" | "1" => Some(true),
                "no" | "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Plist::Int(i) => Some(*i),
            Plist::Real(r) if r.is_finite() => Some(*r as i64),
            Plist::Bool(b) => Some(i64::from(*b)),
            Plist::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Plist::Real(r) => Some(*r),
            Plist::Int(i) => Some(*i as f64),
            Plist::Date(d) => Some(*d),
            Plist::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_data(&self) -> Option<&[u8]> {
        match self {
            Plist::Data(d) => Some(d),
            _ => None,
        }
    }
}

/// Parses an XML or binary property list.
pub fn parse(data: &[u8]) -> Result<Plist, String> {
    if data.starts_with(b"bplist00") {
        Binary::new(data)?.root()
    } else {
        parse_xml(data)
    }
}

// ---------------------------------------------------------------- binary

struct Binary<'a> {
    data: &'a [u8],
    offsets: Vec<usize>,
    ref_size: usize,
    top: usize,
}

fn be_uint(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, x| (acc << 8) | u64::from(*x))
}

impl<'a> Binary<'a> {
    fn new(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < 8 + 32 {
            return Err("binary plist too short".into());
        }
        let t = &data[data.len() - 32..];
        let offset_size = t[6] as usize;
        let ref_size = t[7] as usize;
        let count = be_uint(&t[8..16]) as usize;
        let top = be_uint(&t[16..24]) as usize;
        let table = be_uint(&t[24..32]) as usize;
        if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) || top >= count {
            return Err("binary plist: invalid trailer".into());
        }
        let end = count.checked_mul(offset_size).and_then(|n| n.checked_add(table)).ok_or("binary plist: invalid offset table")?;
        if end > data.len() - 32 {
            return Err("binary plist: offset table out of range".into());
        }
        let offsets = (0..count).map(|i| be_uint(&data[table + i * offset_size..table + (i + 1) * offset_size]) as usize).collect();
        Ok(Self { data, offsets, ref_size, top })
    }

    fn root(&self) -> Result<Plist, String> {
        self.object(self.top, 0)
    }

    fn bytes(&self, at: usize, len: usize) -> Result<&'a [u8], String> {
        at.checked_add(len).filter(|e| *e <= self.data.len()).map(|e| &self.data[at..e]).ok_or_else(|| "binary plist: object out of range".into())
    }

    /// Length of a container/string: low nibble, or a following int object.
    fn length(&self, marker: u8, at: usize) -> Result<(usize, usize), String> {
        let low = (marker & 0x0f) as usize;
        if low != 0x0f {
            return Ok((low, at + 1));
        }
        let m = *self.bytes(at + 1, 1)?.first().unwrap_or(&0);
        if m >> 4 != 0x1 {
            return Err("binary plist: invalid length".into());
        }
        let n = 1usize << (m & 0x0f);
        let v = be_uint(self.bytes(at + 2, n.min(8))?) as usize;
        Ok((v, at + 2 + n))
    }

    fn reference(&self, at: usize, i: usize) -> Result<usize, String> {
        Ok(be_uint(self.bytes(at + i * self.ref_size, self.ref_size)?) as usize)
    }

    fn object(&self, index: usize, depth: usize) -> Result<Plist, String> {
        if depth > MAX_DEPTH {
            return Err("binary plist nested too deeply".into());
        }
        let at = *self.offsets.get(index).ok_or("binary plist: invalid object reference")?;
        let marker = *self.bytes(at, 1)?.first().unwrap_or(&0);
        let low = marker & 0x0f;
        Ok(match marker >> 4 {
            0x0 => match marker {
                0x08 => Plist::Bool(false),
                0x09 => Plist::Bool(true),
                _ => Plist::Null,
            },
            0x1 => {
                let n = 1usize << low;
                let b = self.bytes(at + 1, n)?;
                match n {
                    1 | 2 | 4 => Plist::Int(be_uint(b) as i64),
                    8 => Plist::Int(be_uint(b) as i64),
                    // 16-byte integers hold 64-bit values in the low half.
                    _ => Plist::Int(be_uint(&b[n - 8..]) as i64),
                }
            }
            0x2 => {
                let n = 1usize << low;
                let b = self.bytes(at + 1, n)?;
                match n {
                    4 => Plist::Real(f64::from(f32::from_bits(be_uint(b) as u32))),
                    8 => Plist::Real(f64::from_bits(be_uint(b))),
                    _ => return Err("binary plist: invalid real".into()),
                }
            }
            0x3 => Plist::Date(f64::from_bits(be_uint(self.bytes(at + 1, 8)?)) + APPLE_EPOCH),
            0x4 => {
                let (n, start) = self.length(marker, at)?;
                Plist::Data(self.bytes(start, n)?.to_vec())
            }
            0x5 => {
                let (n, start) = self.length(marker, at)?;
                Plist::String(String::from_utf8_lossy(self.bytes(start, n)?).into_owned())
            }
            0x6 => {
                let (n, start) = self.length(marker, at)?;
                let b = self.bytes(start, n.checked_mul(2).ok_or("binary plist: invalid string")?)?;
                let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
                Plist::String(String::from_utf16_lossy(&units))
            }
            0x8 => Plist::Uid(be_uint(self.bytes(at + 1, low as usize + 1)?)),
            0xA | 0xC => {
                let (n, start) = self.length(marker, at)?;
                self.bytes(start, n.checked_mul(self.ref_size).ok_or("binary plist: invalid array")?)?;
                let mut out = Vec::with_capacity(n.min(4096));
                for i in 0..n {
                    out.push(self.object(self.reference(start, i)?, depth + 1)?);
                }
                Plist::Array(out)
            }
            0xD => {
                let (n, start) = self.length(marker, at)?;
                self.bytes(start, n.checked_mul(2 * self.ref_size).ok_or("binary plist: invalid dictionary")?)?;
                let mut out = BTreeMap::new();
                for i in 0..n {
                    let key = match self.object(self.reference(start, i)?, depth + 1)? {
                        Plist::String(s) => s,
                        _ => return Err("binary plist: dictionary key is not a string".into()),
                    };
                    out.insert(key, self.object(self.reference(start, n + i)?, depth + 1)?);
                }
                Plist::Dict(out)
            }
            _ => return Err(format!("binary plist: unknown object type {marker:#04x}")),
        })
    }
}

// ---------------------------------------------------------------- XML

enum Frame {
    Array(Vec<Plist>),
    Dict(BTreeMap<String, Plist>, Option<String>),
}

fn iso_date(s: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(s.trim()).map(|d| d.timestamp() as f64).unwrap_or(0.0)
}

fn parse_xml(data: &[u8]) -> Result<Plist, String> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Frame> = Vec::new();
    let mut text = String::new();
    let mut root: Option<Plist> = None;
    let mut buf = Vec::new();

    fn push(stack: &mut [Frame], root: &mut Option<Plist>, v: Plist) -> Result<(), String> {
        match stack.last_mut() {
            Some(Frame::Array(a)) => a.push(v),
            Some(Frame::Dict(d, key)) => match key.take() {
                Some(k) => {
                    d.insert(k, v);
                }
                None => return Err("plist: value without a key".into()),
            },
            None => *root = Some(v),
        }
        Ok(())
    }

    loop {
        let ev = reader.read_event_into(&mut buf).map_err(|e| format!("plist: {e}"))?;
        match ev {
            Event::Start(e) => {
                text.clear();
                match e.name().as_ref() {
                    b"array" => stack.push(Frame::Array(Vec::new())),
                    b"dict" => stack.push(Frame::Dict(BTreeMap::new(), None)),
                    _ => {}
                }
                if stack.len() > MAX_DEPTH {
                    return Err("plist nested too deeply".into());
                }
            }
            Event::Empty(e) => {
                let v = match e.name().as_ref() {
                    b"true" => Plist::Bool(true),
                    b"false" => Plist::Bool(false),
                    b"array" => Plist::Array(Vec::new()),
                    b"dict" => Plist::Dict(BTreeMap::new()),
                    b"string" => Plist::String(String::new()),
                    b"data" => Plist::Data(Vec::new()),
                    b"plist" => continue,
                    other => return Err(format!("plist: unexpected <{}/>", String::from_utf8_lossy(other))),
                };
                push(&mut stack, &mut root, v)?;
            }
            Event::Text(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::GeneralRef(r) => match r.resolve_char_ref() {
                Ok(Some(c)) => text.push(c),
                _ => {
                    // Predefined entities only; anything else is dropped, never expanded.
                    let name = String::from_utf8_lossy(&r).into_owned();
                    if let Some(v) = quick_xml::escape::resolve_predefined_entity(&name) {
                        text.push_str(v);
                    }
                }
            },
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::End(e) => {
                let v = match e.name().as_ref() {
                    b"key" => {
                        match stack.last_mut() {
                            Some(Frame::Dict(_, key)) => *key = Some(std::mem::take(&mut text)),
                            _ => return Err("plist: <key> outside a dictionary".into()),
                        }
                        continue;
                    }
                    b"string" => Plist::String(std::mem::take(&mut text)),
                    b"integer" => Plist::Int(text.trim().parse().map_err(|_| format!("plist: invalid integer {:?}", text.trim()))?),
                    b"real" => Plist::Real(text.trim().parse().map_err(|_| format!("plist: invalid real {:?}", text.trim()))?),
                    b"date" => Plist::Date(iso_date(&text)),
                    b"data" => {
                        let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                        Plist::Data(base64::engine::general_purpose::STANDARD.decode(clean).map_err(|e| format!("plist: invalid data: {e}"))?)
                    }
                    b"array" => match stack.pop() {
                        Some(Frame::Array(a)) => Plist::Array(a),
                        _ => return Err("plist: unbalanced </array>".into()),
                    },
                    b"dict" => match stack.pop() {
                        Some(Frame::Dict(d, _)) => Plist::Dict(d),
                        _ => return Err("plist: unbalanced </dict>".into()),
                    },
                    _ => continue,
                };
                text.clear();
                push(&mut stack, &mut root, v)?;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    root.ok_or_else(|| "plist: no value".into())
}

// ---------------------------------------------------------------- NSKeyedArchiver

/// Decodes an `NSKeyedArchiver` archive into plain values: dictionaries
/// (`NS.keys`/`NS.objects`), arrays, strings, numbers and data. Objects of
/// other classes become dictionaries of their decoded fields plus
/// `$class` (the class name).
pub fn unarchive(data: &[u8]) -> Result<Plist, String> {
    let archive = parse(data)?;
    let objects = archive.get("$objects").and_then(Plist::as_array).ok_or("archive without $objects")?;
    let root = match archive.get("$top").and_then(|t| t.get("root")) {
        Some(Plist::Uid(u)) => *u as usize,
        _ => return Err("archive without a root object".into()),
    };
    resolve(objects, root, 0)
}

fn resolve(objects: &[Plist], index: usize, depth: usize) -> Result<Plist, String> {
    if depth > MAX_DEPTH {
        return Err("archive nested too deeply".into());
    }
    let obj = objects.get(index).ok_or("archive: invalid object reference")?;
    let value = |v: &Plist| -> Result<Plist, String> {
        match v {
            Plist::Uid(u) => resolve(objects, *u as usize, depth + 1),
            Plist::Array(a) => a.iter().map(|x| if let Plist::Uid(u) = x { resolve(objects, *u as usize, depth + 1) } else { Ok(x.clone()) }).collect::<Result<Vec<_>, _>>().map(Plist::Array),
            other => Ok(other.clone()),
        }
    };
    match obj {
        Plist::String(s) if s == "$null" => Ok(Plist::Null),
        Plist::Dict(d) => {
            if let (Some(Plist::Array(keys)), Some(Plist::Array(vals))) = (d.get("NS.keys"), d.get("NS.objects")) {
                let mut out = BTreeMap::new();
                for (k, v) in keys.iter().zip(vals) {
                    if let Plist::String(k) = value(k)? {
                        out.insert(k, value(v)?);
                    }
                }
                return Ok(Plist::Dict(out));
            }
            if let Some(Plist::Array(_)) = d.get("NS.objects") {
                return value(&d["NS.objects"]);
            }
            if let Some(s) = d.get("NS.string") {
                return value(s);
            }
            if let Some(b) = d.get("NS.bytes").or_else(|| d.get("NS.data")) {
                return value(b);
            }
            let mut out = BTreeMap::new();
            for (k, v) in d {
                if k == "$class" {
                    if let Ok(Plist::Dict(c)) = value(v)
                        && let Some(Plist::String(name)) = c.get("$classname")
                    {
                        out.insert("$class".into(), Plist::String(name.clone()));
                    }
                    continue;
                }
                out.insert(k.clone(), value(v)?);
            }
            Ok(Plist::Dict(out))
        }
        other => Ok(other.clone()),
    }
}

// ---------------------------------------------------------------- writing (tests)

/// Binary plist writer (`bplist00`), used to build fixtures and archives
/// in tests.
#[cfg(test)]
pub(crate) fn to_binary(root: &Plist) -> Vec<u8> {
    fn flatten(v: &Plist, objs: &mut Vec<Vec<u8>>) -> usize {
        let idx = objs.len();
        objs.push(Vec::new());
        let mut out = Vec::new();
        let len_marker = |kind: u8, n: usize, out: &mut Vec<u8>| {
            if n < 15 {
                out.push((kind << 4) | n as u8);
            } else {
                out.push((kind << 4) | 0x0f);
                out.push(0x13);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        };
        match v {
            Plist::Null => out.push(0x00),
            Plist::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
            Plist::Int(i) => {
                out.push(0x13);
                out.extend_from_slice(&i.to_be_bytes());
            }
            Plist::Real(r) => {
                out.push(0x23);
                out.extend_from_slice(&r.to_bits().to_be_bytes());
            }
            Plist::Date(d) => {
                out.push(0x33);
                out.extend_from_slice(&(d - APPLE_EPOCH).to_bits().to_be_bytes());
            }
            Plist::Data(d) => {
                len_marker(0x4, d.len(), &mut out);
                out.extend_from_slice(d);
            }
            Plist::String(s) if s.is_ascii() => {
                len_marker(0x5, s.len(), &mut out);
                out.extend_from_slice(s.as_bytes());
            }
            Plist::String(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                len_marker(0x6, units.len(), &mut out);
                for u in units {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
            Plist::Uid(u) => {
                out.push(0x87);
                out.extend_from_slice(&u.to_be_bytes());
            }
            Plist::Array(a) => {
                let refs: Vec<usize> = a.iter().map(|x| flatten(x, objs)).collect();
                len_marker(0xA, refs.len(), &mut out);
                for r in refs {
                    out.extend_from_slice(&(r as u32).to_be_bytes());
                }
            }
            Plist::Dict(d) => {
                let keys: Vec<usize> = d.keys().map(|k| flatten(&Plist::String(k.clone()), objs)).collect();
                let vals: Vec<usize> = d.values().map(|x| flatten(x, objs)).collect();
                len_marker(0xD, keys.len(), &mut out);
                for r in keys.into_iter().chain(vals) {
                    out.extend_from_slice(&(r as u32).to_be_bytes());
                }
            }
        }
        objs[idx] = out;
        idx
    }
    let mut objs = Vec::new();
    flatten(root, &mut objs);
    let mut data = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for o in &objs {
        offsets.push(data.len() as u64);
        data.extend_from_slice(o);
    }
    let table = data.len() as u64;
    for o in &offsets {
        data.extend_from_slice(&o.to_be_bytes());
    }
    data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 8, 4]);
    data.extend_from_slice(&(objs.len() as u64).to_be_bytes());
    data.extend_from_slice(&0u64.to_be_bytes());
    data.extend_from_slice(&table.to_be_bytes());
    data
}

/// An `NSKeyedArchiver` archive of plain values (dictionaries, arrays,
/// strings, numbers), as `-[NSKeyedArchiver archivedDataWithRootObject:]`
/// lays them out.
#[cfg(test)]
pub(crate) fn archive(root: &Plist) -> Vec<u8> {
    fn add(v: &Plist, objects: &mut Vec<Plist>, classes: &mut BTreeMap<&'static str, u64>) -> Plist {
        let idx = objects.len() as u64;
        objects.push(Plist::Null);
        fn class(name: &'static str, objects: &mut Vec<Plist>, classes: &mut BTreeMap<&'static str, u64>) -> Plist {
            let id = *classes.entry(name).or_insert_with(|| {
                objects.push(Plist::Dict(BTreeMap::from([
                    ("$classname".to_string(), Plist::String(name.into())),
                    ("$classes".to_string(), Plist::Array(vec![Plist::String(name.into()), Plist::String("NSObject".into())])),
                ])));
                objects.len() as u64 - 1
            });
            Plist::Uid(id)
        }
        let obj = match v {
            Plist::Array(a) => {
                let refs: Vec<Plist> = a.iter().map(|x| add(x, objects, classes)).collect();
                let c = class("NSMutableArray", objects, classes);
                Plist::Dict(BTreeMap::from([("NS.objects".to_string(), Plist::Array(refs)), ("$class".to_string(), c)]))
            }
            Plist::Dict(d) => {
                let keys: Vec<Plist> = d.keys().map(|k| add(&Plist::String(k.clone()), objects, classes)).collect();
                let vals: Vec<Plist> = d.values().map(|x| add(x, objects, classes)).collect();
                let c = class("NSMutableDictionary", objects, classes);
                Plist::Dict(BTreeMap::from([
                    ("NS.keys".to_string(), Plist::Array(keys)),
                    ("NS.objects".to_string(), Plist::Array(vals)),
                    ("$class".to_string(), c),
                ]))
            }
            Plist::Null => Plist::String("$null".into()),
            other => other.clone(),
        };
        objects[idx as usize] = obj;
        Plist::Uid(idx)
    }
    let mut objects = vec![Plist::String("$null".into())];
    let mut classes = BTreeMap::new();
    let root_ref = add(root, &mut objects, &mut classes);
    to_binary(&Plist::Dict(BTreeMap::from([
        ("$archiver".to_string(), Plist::String("NSKeyedArchiver".into())),
        ("$version".to_string(), Plist::Int(100_000)),
        ("$objects".to_string(), Plist::Array(objects)),
        ("$top".to_string(), Plist::Dict(BTreeMap::from([("root".to_string(), root_ref)]))),
    ])))
}

/// XML property list text of `v` (fixtures).
#[cfg(test)]
pub(crate) fn to_xml(v: &Plist) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }
    fn w(v: &Plist, out: &mut String) {
        match v {
            Plist::Bool(true) => out.push_str("<true/>"),
            Plist::Bool(false) => out.push_str("<false/>"),
            Plist::Int(i) => out.push_str(&format!("<integer>{i}</integer>")),
            Plist::Real(r) => out.push_str(&format!("<real>{r}</real>")),
            Plist::Date(d) => out.push_str(&format!(
                "<date>{}</date>",
                chrono::DateTime::from_timestamp(*d as i64, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ")
            )),
            Plist::Data(d) => out.push_str(&format!("<data>{}</data>", base64::engine::general_purpose::STANDARD.encode(d))),
            Plist::String(s) => out.push_str(&format!("<string>{}</string>", esc(s))),
            Plist::Array(a) => {
                out.push_str("<array>");
                a.iter().for_each(|x| w(x, out));
                out.push_str("</array>");
            }
            Plist::Dict(d) => {
                out.push_str("<dict>");
                for (k, x) in d {
                    out.push_str(&format!("<key>{}</key>", esc(k)));
                    w(x, out);
                }
                out.push_str("</dict>");
            }
            Plist::Uid(_) | Plist::Null => {}
        }
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    w(v, &mut out);
    out.push_str("\n</plist>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Plist {
        Plist::Dict(BTreeMap::from([
            ("Bool".to_string(), Plist::Bool(true)),
            ("Int".to_string(), Plist::Int(-42)),
            ("Big".to_string(), Plist::Int(1 << 40)),
            ("Real".to_string(), Plist::Real(2.5)),
            ("Date".to_string(), Plist::Date(1_700_000_000.0)),
            ("Data".to_string(), Plist::Data(vec![0, 1, 2, 255])),
            ("Text".to_string(), Plist::String("a < b & \u{442}\u{435}\u{441}\u{442} with a long enough text".into())),
            ("List".to_string(), Plist::Array(vec![Plist::Int(1), Plist::String("x".into()), Plist::Array(vec![])])),
            ("Empty".to_string(), Plist::Dict(BTreeMap::new())),
        ]))
    }

    #[test]
    fn reads_xml_and_binary() {
        let v = sample();
        assert_eq!(parse(to_xml(&v).as_bytes()).unwrap(), v);
        assert_eq!(parse(&to_binary(&v)).unwrap(), v);
        let v = parse(to_xml(&v).as_bytes()).unwrap();
        assert_eq!(v.get("Bool").and_then(Plist::as_bool), Some(true));
        assert_eq!(v.get("Int").and_then(Plist::as_i64), Some(-42));
        assert_eq!(v.get("Real").and_then(Plist::as_f64), Some(2.5));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"bplist00").is_err());
        let mut bad = to_binary(&sample());
        let n = bad.len();
        bad[n - 8..].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(parse(&bad).is_err());
        assert!(parse(b"<plist><dict><string>x</string></dict></plist>").is_err(), "value without a key");
        assert!(parse(b"").is_err());
        // A self-referencing array is cut off by the depth limit.
        let mut data = b"bplist00".to_vec();
        data.extend_from_slice(&[0xA1, 0x00]);
        data.push(8); // offset table at 10: object 0 at 8
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        data.extend_from_slice(&1u64.to_be_bytes());
        data.extend_from_slice(&0u64.to_be_bytes());
        data.extend_from_slice(&10u64.to_be_bytes());
        assert!(parse(&data).is_err());
    }

    #[test]
    fn unarchives_keyed_archives() {
        let groups = Plist::Array(vec![Plist::Dict(BTreeMap::from([
            ("Name".to_string(), Plist::String("Films".into())),
            ("Index".to_string(), Plist::Int(3)),
            ("UsesCustomDownloadLocation".to_string(), Plist::Bool(true)),
            ("CustomDownloadLocation".to_string(), Plist::String("/Volumes/Media/Films".into())),
        ]))]);
        assert_eq!(unarchive(&archive(&groups)).unwrap(), groups);
        assert!(unarchive(&to_binary(&Plist::Int(1))).is_err());
    }
}
