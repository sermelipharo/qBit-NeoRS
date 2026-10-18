//! Reader for Qt's `QSettings` INI format as qBittorrent writes it
//! (`qBittorrent.ini`, `qBittorrent-data.ini`).
//!
//! Follows `QSettingsPrivate`: section and key names are percent-escaped
//! (`%XX`, `%UXXXX`) with `\` as the group separator, `[General]` holds the
//! top-level keys, values use C-like escapes (`\n`, `\xHH`, `\0`, octal),
//! double quotes and unquoted commas (a string list), and `@`-prefixed
//! values carry typed data: `@ByteArray(...)`, `@Variant(...)` (a
//! `QDataStream` in the `Qt_4_0` format), `@Invalid()`, `@Size(...)` and
//! friends. A value line ending in a backslash continues on the next line.

use std::collections::BTreeMap;

/// One decoded value.
#[derive(Clone, Debug, PartialEq)]
pub enum QValue {
    String(String),
    /// An unquoted comma made it a `QStringList` (elements decoded too).
    List(Vec<QValue>),
    ByteArray(Vec<u8>),
    Variant(Variant),
    /// `@Invalid()` (Qt writes an empty `QStringList` this way).
    Invalid,
    /// `@Size(..)`, `@Rect(..)`, `@Point(..)`, `@DateTime(..)` etc. kept raw.
    Other(String),
}

/// `QVariant` read from a `@Variant(...)` value.
#[derive(Clone, Debug, PartialEq)]
pub enum Variant {
    Invalid,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    StringList(Vec<String>),
    ByteArray(Vec<u8>),
    /// Milliseconds since midnight; `None` for a null `QTime`.
    Time(Option<u32>),
    List(Vec<Variant>),
    Map(BTreeMap<String, Variant>),
    /// A type this reader does not decode (its `QMetaType` id).
    Unsupported(u32),
}

/// A parsed INI file: full key path (`Section/Sub/Key`) to value. Keys
/// keep their order of appearance only through the `BTreeMap` order.
#[derive(Clone, Debug, Default)]
pub struct IniFile {
    pub values: BTreeMap<String, QValue>,
}

impl IniFile {
    pub fn parse(data: &[u8]) -> Self {
        let mut values = BTreeMap::new();
        let mut section = String::new();
        for line in logical_lines(data) {
            let line = trim_ascii(&line);
            if line.is_empty() || line[0] == b';' || line[0] == b'#' {
                continue;
            }
            if line[0] == b'[' {
                let end = line.iter().rposition(|&b| b == b']').unwrap_or(line.len());
                let raw = trim_ascii(&line[1..end.max(1)]);
                section = if raw.eq_ignore_ascii_case(b"general") {
                    String::new()
                } else if raw.eq_ignore_ascii_case(b"%general") {
                    "General".to_owned()
                } else {
                    unescape_key(raw)
                };
                continue;
            }
            let Some(eq) = line.iter().position(|&b| b == b'=') else { continue };
            let key = unescape_key(trim_ascii(&line[..eq]));
            if key.is_empty() {
                continue;
            }
            let full = if section.is_empty() { key } else { format!("{section}/{key}") };
            values.insert(full, parse_value(trim_ascii(&line[eq + 1..])));
        }
        Self { values }
    }

    pub fn get(&self, key: &str) -> Option<&QValue> {
        self.values.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// `QVariant::toString()` of the value (a one-element list gives its element).
    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key).map(QValue::to_string_value)
    }

    /// `QVariant::toStringList()`; a plain empty string is an empty list.
    pub fn string_list(&self, key: &str) -> Option<Vec<String>> {
        self.get(key).map(QValue::to_string_list)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(QValue::to_bool)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(QValue::to_int)
    }

    pub fn double(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(QValue::to_double)
    }

    pub fn byte_array(&self, key: &str) -> Option<Vec<u8>> {
        match self.get(key)? {
            QValue::ByteArray(b) => Some(b.clone()),
            QValue::String(s) => Some(s.as_bytes().to_vec()),
            QValue::Variant(Variant::ByteArray(b)) => Some(b.clone()),
            _ => None,
        }
    }
}

impl QValue {
    pub fn to_string_value(&self) -> String {
        match self {
            QValue::String(s) | QValue::Other(s) => s.clone(),
            QValue::List(items) => items.iter().map(QValue::to_string_value).collect::<Vec<_>>().join(", "),
            QValue::ByteArray(b) => String::from_utf8_lossy(b).into_owned(),
            QValue::Variant(v) => v.to_string_value(),
            QValue::Invalid => String::new(),
        }
    }

    pub fn to_string_list(&self) -> Vec<String> {
        match self {
            QValue::List(items) => items.iter().map(QValue::to_string_value).collect(),
            QValue::String(s) if s.is_empty() => Vec::new(),
            QValue::Invalid => Vec::new(),
            QValue::Variant(Variant::StringList(l)) => l.clone(),
            QValue::Variant(Variant::List(l)) => l.iter().map(Variant::to_string_value).collect(),
            other => vec![other.to_string_value()],
        }
    }

    pub fn to_bool(&self) -> Option<bool> {
        match self {
            QValue::Variant(Variant::Bool(b)) => Some(*b),
            QValue::Variant(Variant::Int(i)) => Some(*i != 0),
            other => {
                let s = other.to_string_value();
                let t = s.trim();
                if t.eq_ignore_ascii_case("true") {
                    Some(true)
                } else if t.eq_ignore_ascii_case("false") || t.is_empty() {
                    Some(false)
                } else {
                    t.parse::<f64>().ok().map(|f| f != 0.0)
                }
            }
        }
    }

    pub fn to_int(&self) -> Option<i64> {
        match self {
            QValue::Variant(Variant::Int(i)) => Some(*i),
            QValue::Variant(Variant::Bool(b)) => Some(i64::from(*b)),
            QValue::Variant(Variant::Double(d)) => Some(d.round() as i64),
            other => {
                let s = other.to_string_value();
                let t = s.trim();
                t.parse::<i64>()
                    .ok()
                    .or_else(|| t.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| f.round() as i64))
                    .or(match t {
                        "true" => Some(1),
                        "false" => Some(0),
                        _ => None,
                    })
            }
        }
    }

    pub fn to_double(&self) -> Option<f64> {
        match self {
            QValue::Variant(Variant::Double(d)) => Some(*d),
            QValue::Variant(Variant::Int(i)) => Some(*i as f64),
            other => other.to_string_value().trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        }
    }
}

impl Variant {
    fn to_string_value(&self) -> String {
        match self {
            Variant::String(s) => s.clone(),
            Variant::Bool(b) => b.to_string(),
            Variant::Int(i) => i.to_string(),
            Variant::Double(d) => d.to_string(),
            Variant::ByteArray(b) => String::from_utf8_lossy(b).into_owned(),
            Variant::StringList(l) if l.len() == 1 => l[0].clone(),
            _ => String::new(),
        }
    }
}

// ---- lines and keys ------------------------------------------------------------

/// Splits into lines; a line ending in an odd number of backslashes
/// continues on the next one (the escaped line break is dropped later by
/// the value unescaper, like Qt does).
fn logical_lines(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == b'\n' || b == b'\r' {
            let trailing = cur.iter().rev().take_while(|&&c| c == b'\\').count();
            if trailing % 2 == 1 {
                // Continuation: keep the backslash + line break pair out.
                cur.pop();
                if b == b'\r' && data.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                i += 1;
                continue;
            }
            out.push(std::mem::take(&mut cur));
            if b == b'\r' && data.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
        } else {
            cur.push(b);
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !matches!(b, b' ' | b'\t')).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !matches!(b, b' ' | b'\t')).map(|p| p + 1).unwrap_or(start);
    &s[start..end.max(start)]
}

fn hex_val(b: u8) -> Option<u32> {
    (b as char).to_digit(16)
}

/// `QSettingsPrivate::iniUnescapedKey`: `%XX` is a byte (Latin-1),
/// `%UXXXX` a UTF-16 unit, `\` the group separator.
fn unescape_key(key: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(key.len());
    let mut i = 0;
    while i < key.len() {
        let b = key[i];
        if b == b'\\' {
            units.push(u16::from(b'/'));
            i += 1;
        } else if b == b'%' {
            if key.get(i + 1) == Some(&b'U') {
                let hex: Option<u32> = key.get(i + 2..i + 6).and_then(|h| {
                    h.iter().try_fold(0u32, |acc, &c| hex_val(c).map(|v| acc * 16 + v))
                });
                if let Some(v) = hex {
                    units.push(v as u16);
                    i += 6;
                    continue;
                }
            } else if let Some(v) = key.get(i + 1..i + 3).and_then(|h| h.iter().try_fold(0u32, |acc, &c| hex_val(c).map(|v| acc * 16 + v)))
            {
                units.push(v as u16);
                i += 3;
                continue;
            }
            units.push(u16::from(b'%'));
            i += 1;
        } else {
            // Raw bytes are UTF-8.
            let end = key[i..].iter().position(|&c| c == b'\\' || c == b'%').map(|p| i + p).unwrap_or(key.len());
            units.extend(String::from_utf8_lossy(&key[i..end]).encode_utf16());
            i = end;
        }
    }
    String::from_utf16_lossy(&units)
}

// ---- values --------------------------------------------------------------------

/// `QSettingsPrivate::iniUnescapedStringList`: returns the elements and
/// whether an unquoted comma made it a list. Strings are built as UTF-16
/// units because `\x` escapes produce units, not bytes.
fn unescape_string_list(s: &[u8]) -> (Vec<Vec<u16>>, bool) {
    let mut list: Vec<Vec<u16>> = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    let mut is_list = false;
    let mut in_quotes = false;
    let mut cur_quoted = false;
    let mut i = 0;
    // Trailing unquoted spaces are chopped, but never below this length
    // (spaces produced by escapes stay).
    let mut chop_limit = 0usize;

    let skip_spaces = |i: &mut usize| {
        while *i < s.len() && (s[*i] == b' ' || s[*i] == b'\t') {
            *i += 1;
        }
    };
    skip_spaces(&mut i);
    while i < s.len() {
        match s[i] {
            b'\\' => {
                i += 1;
                if i >= s.len() {
                    break;
                }
                let ch = s[i];
                i += 1;
                let simple = match ch {
                    b'a' => Some(0x07),
                    b'b' => Some(0x08),
                    b'f' => Some(0x0c),
                    b'n' => Some(u16::from(b'\n')),
                    b'r' => Some(u16::from(b'\r')),
                    b't' => Some(u16::from(b'\t')),
                    b'v' => Some(0x0b),
                    b'"' | b'?' | b'\'' | b'\\' => Some(u16::from(ch)),
                    _ => None,
                };
                if let Some(u) = simple {
                    cur.push(u);
                } else if ch == b'x' {
                    let mut v: u32 = 0;
                    let mut any = false;
                    while i < s.len() {
                        let Some(d) = hex_val(s[i]) else { break };
                        v = (v << 4).wrapping_add(d);
                        any = true;
                        i += 1;
                    }
                    if any {
                        cur.push(v as u16);
                    }
                } else if (b'0'..=b'7').contains(&ch) {
                    let mut v: u32 = u32::from(ch - b'0');
                    while i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                        v = (v << 3).wrapping_add(u32::from(s[i] - b'0'));
                        i += 1;
                    }
                    cur.push(v as u16);
                } else if (ch == b'\n' || ch == b'\r') && i < s.len() && (s[i] == b'\n' || s[i] == b'\r') && s[i] != ch {
                    i += 1;
                }
                // Any other escaped character is dropped (Qt skips it).
                chop_limit = cur.len();
            }
            b'"' => {
                i += 1;
                cur_quoted = true;
                in_quotes = !in_quotes;
                if !in_quotes {
                    skip_spaces(&mut i);
                }
            }
            b',' if !in_quotes => {
                if !cur_quoted {
                    chop_trailing_spaces(&mut cur, chop_limit);
                }
                is_list = true;
                list.push(std::mem::take(&mut cur));
                cur_quoted = false;
                chop_limit = 0;
                i += 1;
                skip_spaces(&mut i);
            }
            _ => {
                let mut j = i + 1;
                while j < s.len() && !matches!(s[j], b'\\' | b'"' | b',') {
                    j += 1;
                }
                cur.extend(String::from_utf8_lossy(&s[i..j]).encode_utf16());
                i = j;
            }
        }
    }
    if !cur_quoted {
        chop_trailing_spaces(&mut cur, chop_limit);
    }
    list.push(cur);
    (list, is_list)
}

fn chop_trailing_spaces(v: &mut Vec<u16>, limit: usize) {
    while v.len() > limit && matches!(v.last(), Some(&0x20) | Some(&0x09)) {
        v.pop();
    }
}

fn parse_value(raw: &[u8]) -> QValue {
    let (items, is_list) = unescape_string_list(raw);
    if is_list {
        QValue::List(items.into_iter().map(|u| typed_value(&u)).collect())
    } else {
        typed_value(items.first().map(Vec::as_slice).unwrap_or(&[]))
    }
}

/// `QSettingsPrivate::stringToVariant` on the unescaped UTF-16 string.
fn typed_value(units: &[u16]) -> QValue {
    let s = String::from_utf16_lossy(units);
    if !s.starts_with('@') {
        return QValue::String(s);
    }
    if let Some(rest) = s.strip_prefix("@@") {
        return QValue::String(format!("@{rest}"));
    }
    if !s.ends_with(')') {
        return QValue::String(s);
    }
    // Payload as Latin-1 bytes (`QString::toLatin1`, units above 0xFF
    // become '?').
    let payload = |prefix: usize| -> Vec<u8> {
        units[prefix..units.len() - 1].iter().map(|&u| if u <= 0xff { u as u8 } else { b'?' }).collect()
    };
    if s.starts_with("@ByteArray(") {
        return QValue::ByteArray(payload("@ByteArray(".len()));
    }
    if s.starts_with("@String(") {
        return QValue::String(String::from_utf16_lossy(&units["@String(".len()..units.len() - 1]));
    }
    if s.starts_with("@Variant(") {
        let data = payload("@Variant(".len());
        return QValue::Variant(decode_variant(&data).unwrap_or(Variant::Invalid));
    }
    if s == "@Invalid()" {
        return QValue::Invalid;
    }
    if s.starts_with("@Size(") || s.starts_with("@Rect(") || s.starts_with("@Point(") || s.starts_with("@DateTime(") {
        return QValue::Other(s);
    }
    QValue::String(s)
}

// ---- QDataStream (Qt_4_0) -----------------------------------------------------

const MAX_NESTING: usize = 16;

/// Decodes a `QVariant` written by `QSettings` (`QDataStream::Qt_4_0`:
/// type id, no null flag, then the value).
pub fn decode_variant(data: &[u8]) -> Option<Variant> {
    let mut r = Reader { data, pos: 0 };
    r.variant(0)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn count(&mut self, min_item: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.saturating_mul(min_item) <= self.data.len() - self.pos).then_some(n)
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Some(String::new());
        }
        if len % 2 != 0 {
            return None;
        }
        let bytes = self.take(len as usize)?;
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        Some(String::from_utf16_lossy(&units))
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Some(Vec::new());
        }
        Some(self.take(len as usize)?.to_vec())
    }

    fn variant(&mut self, depth: usize) -> Option<Variant> {
        if depth > MAX_NESTING {
            return None;
        }
        let ty = self.u32()?;
        Some(match ty {
            0 => Variant::Invalid,
            1 => Variant::Bool(self.u8()? != 0),
            2 => Variant::Int(i64::from(self.u32()? as i32)),
            3 => Variant::Int(i64::from(self.u32()?)),
            4 => Variant::Int(self.u64()? as i64),
            5 => Variant::Int(self.u64()? as i64),
            6 => Variant::Double(f64::from_bits(self.u64()?)),
            8 | 28 => {
                let n = self.count(8)?;
                let mut m = BTreeMap::new();
                for _ in 0..n {
                    let k = self.string()?;
                    let v = self.variant(depth + 1)?;
                    m.insert(k, v);
                }
                Variant::Map(m)
            }
            9 => {
                let n = self.count(4)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.variant(depth + 1)?);
                }
                Variant::List(v)
            }
            10 => Variant::String(self.string()?),
            11 => {
                let n = self.count(4)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.string()?);
                }
                Variant::StringList(v)
            }
            12 => Variant::ByteArray(self.bytes()?),
            15 => {
                let ms = self.u32()?;
                Variant::Time((ms != u32::MAX && ms < 86_400_000).then_some(ms))
            }
            other => Variant::Unsupported(other),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini(text: &str) -> IniFile {
        IniFile::parse(text.as_bytes())
    }

    #[test]
    fn sections_keys_and_general() {
        let f = ini("[General]\nfoo=1\n[%General]\nbar=2\n[BitTorrent]\nSession\\Port=52739\n; comment\n# other\n[Pref%20s]\na%2Fb=x\nk%U00E9y=v\n");
        assert_eq!(f.int("foo"), Some(1));
        assert_eq!(f.int("General/bar"), Some(2));
        assert_eq!(f.int("BitTorrent/Session/Port"), Some(52739));
        assert_eq!(f.string("Pref s/a/b").as_deref(), Some("x"));
        assert_eq!(f.string("Pref s/k\u{e9}y").as_deref(), Some("v"));
        assert!(!f.contains("comment"));
    }

    #[test]
    fn escapes_quotes_and_lists() {
        let f = ini(concat!(
            "[A]\n",
            "plain=  hello world  \n",
            "quoted=\" spaced, with comma \"\n",
            "esc=a\\tb\\nc\\\\d\\\"e\\x41\\101\\0\n",
            "list=a, b ,c\n",
            "qlist=s(\\\\d+)e(\\\\d+), \"(\\\\d{1,2})\", x\n",
            "empty=\n",
            "single=@Invalid()\n",
            "utf8=\u{77e2}\u{6ca2}, Movie\n",
            "unicode=\\x77e2\\x6ca2\n",
            "at=@@literal\n",
            "cont=one\\\ntwo\n",
        ));
        assert_eq!(f.string("A/plain").as_deref(), Some("hello world"));
        assert_eq!(f.string("A/quoted").as_deref(), Some(" spaced, with comma "));
        assert_eq!(f.string("A/esc").as_deref(), Some("a\tb\nc\\d\"eAA\0"));
        assert_eq!(f.string_list("A/list").unwrap(), vec!["a", "b", "c"]);
        assert_eq!(f.string_list("A/qlist").unwrap(), vec!["s(\\d+)e(\\d+)", "(\\d{1,2})", "x"]);
        assert_eq!(f.string_list("A/empty").unwrap(), Vec::<String>::new());
        assert_eq!(f.get("A/single"), Some(&QValue::Invalid));
        assert_eq!(f.string_list("A/single").unwrap(), Vec::<String>::new());
        assert_eq!(f.string_list("A/utf8").unwrap(), vec!["\u{77e2}\u{6ca2}", "Movie"]);
        assert_eq!(f.string("A/unicode").as_deref(), Some("\u{77e2}\u{6ca2}"));
        assert_eq!(f.string("A/at").as_deref(), Some("@literal"));
        assert_eq!(f.string("A/cont").as_deref(), Some("onetwo"));
    }

    #[test]
    fn byte_array_with_escapes() {
        // qBittorrent's PBKDF2 value and a binary geometry blob.
        let f = ini(concat!(
            "[Preferences]\n",
            "WebUI\\Password_PBKDF2=\"@ByteArray(c2FsdA==:aGFzaA==)\"\n",
            "[MainWindow]\n",
            "geometry=@ByteArray(\\x1\\xd9\\xd0\\xcb\\0\\x3\\0\\0)\n",
        ));
        assert_eq!(f.byte_array("Preferences/WebUI/Password_PBKDF2").unwrap(), b"c2FsdA==:aGFzaA==");
        assert_eq!(f.byte_array("MainWindow/geometry").unwrap(), vec![1, 0xd9, 0xd0, 0xcb, 0, 3, 0, 0]);
    }

    #[test]
    fn variant_hash_of_longlongs() {
        // qBittorrent-data.ini: QVariantHash{AlltimeUL, AlltimeDL} (qlonglong).
        let f = ini(concat!(
            "[Stats]\n",
            "AllStats=@Variant(\\0\\0\\0\\x1c\\0\\0\\0\\x2\\0\\0\\0\\x12\\0\\x41\\0l\\0l\\0t\\0i\\0m\\0\\x65\\0U\\0L",
            "\\0\\0\\0\\x4\\0\\0\\0\\x91S\\xa3\\x41\\x5\\0\\0\\0\\x12\\0\\x41\\0l\\0l\\0t\\0i\\0m\\0\\x65\\0\\x44\\0L",
            "\\0\\0\\0\\x4\\0\\0\\x1\\x4LB\\x8a\\x8d)\n",
        ));
        let Some(QValue::Variant(Variant::Map(m))) = f.get("Stats/AllStats") else { panic!("{:?}", f.get("Stats/AllStats")) };
        assert_eq!(m.get("AlltimeUL"), Some(&Variant::Int(0x91_53a3_4105)));
        assert_eq!(m.get("AlltimeDL"), Some(&Variant::Int(0x0104_4c42_8a8d)));
    }

    #[test]
    fn variant_time_and_others() {
        // QTime 08:00 = 28 800 000 ms = 0x01B77400.
        let f = ini("[Preferences]\nScheduler\\start_time=@Variant(\\0\\0\\0\\xf\\x1\\xb7t\\0)\nsz=@Size(545 504)\n");
        assert_eq!(f.get("Preferences/Scheduler/start_time"), Some(&QValue::Variant(Variant::Time(Some(28_800_000)))));
        assert!(matches!(f.get("Preferences/sz"), Some(QValue::Other(_))));
        // Truncated or hostile streams never panic.
        assert_eq!(decode_variant(&[0, 0, 0, 10, 0, 0, 0, 9]), None);
        assert_eq!(decode_variant(&[0, 0, 0, 11, 0xff, 0xff, 0xff, 0xf0]), None);
        assert_eq!(decode_variant(&[0, 0, 0, 2, 0xff, 0xff, 0xff, 0xfe]), Some(Variant::Int(-2)));
    }

    #[test]
    fn typed_accessors() {
        let f = ini("[A]\nt=true\nf=false\nn=-2\nd=1.5\nlist=x\n");
        assert_eq!(f.bool("A/t"), Some(true));
        assert_eq!(f.bool("A/f"), Some(false));
        assert_eq!(f.bool("A/n"), Some(true));
        assert_eq!(f.int("A/n"), Some(-2));
        assert_eq!(f.int("A/t"), Some(1));
        assert_eq!(f.double("A/d"), Some(1.5));
        assert_eq!(f.int("A/d"), Some(2));
        assert_eq!(f.string_list("A/list").unwrap(), vec!["x"]);
        assert_eq!(f.int("A/missing"), None);
    }
}
