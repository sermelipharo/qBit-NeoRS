//! Legacy rules export format (`*.rssrules`): a `QDataStream` (version
//! `Qt_4_5`, big-endian) serialization of `QVariantHash { ruleName ->
//! QVariantHash }` with the keys of `AutoDownloadRule::toLegacyDict`.
//! Only the `QVariant` types needed for that dictionary are supported; the
//! reader is bounds-checked and never trusts declared lengths.

use std::collections::BTreeMap;

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

use super::rule::{Rule, default_template};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Variant {
    Invalid,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    StringList(Vec<String>),
    ByteArray(Vec<u8>),
    /// Unix ms; `None` = null/invalid date-time.
    DateTime(Option<i64>),
    List(Vec<Variant>),
    Hash(BTreeMap<String, Variant>),
}

impl Variant {
    fn to_string_value(&self) -> String {
        match self {
            Variant::String(s) => s.clone(),
            Variant::Int(i) => i.to_string(),
            Variant::Bool(b) => b.to_string(),
            Variant::ByteArray(b) => String::from_utf8_lossy(b).into_owned(),
            _ => String::new(),
        }
    }

    fn to_bool(&self, default: bool) -> bool {
        match self {
            Variant::Bool(b) => *b,
            Variant::Int(i) => *i != 0,
            Variant::String(s) => !(s.is_empty() || s == "0" || s.eq_ignore_ascii_case("false")),
            _ => default,
        }
    }

    fn to_int(&self) -> i64 {
        match self {
            Variant::Int(i) => *i,
            Variant::Bool(b) => i64::from(*b),
            Variant::Double(d) => *d as i64,
            Variant::String(s) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn to_string_list(&self) -> Vec<String> {
        match self {
            Variant::StringList(l) => l.clone(),
            Variant::String(s) => vec![s.clone()],
            Variant::List(l) => l.iter().map(Variant::to_string_value).collect(),
            _ => Vec::new(),
        }
    }
}

// QMetaType ids (Qt 4 numbering, unchanged for these types).
const T_INVALID: u32 = 0;
const T_BOOL: u32 = 1;
const T_INT: u32 = 2;
const T_UINT: u32 = 3;
const T_LONGLONG: u32 = 4;
const T_ULONGLONG: u32 = 5;
const T_DOUBLE: u32 = 6;
const T_LIST: u32 = 9;
const T_STRING: u32 = 10;
const T_STRINGLIST: u32 = 11;
const T_BYTEARRAY: u32 = 12;
const T_DATETIME: u32 = 16;
const T_MAP: u32 = 8;
const T_HASH: u32 = 28;

/// Julian day of 1970-01-01.
const JD_EPOCH: i64 = 2_440_588;
const MAX_NESTING: usize = 16;

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

type R<T> = Result<T, ()>;

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(())?;
        let s = self.data.get(self.pos..end).ok_or(())?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u64(&mut self) -> R<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    /// Element counts can never exceed the remaining bytes.
    fn count(&mut self, min_item_size: usize) -> R<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item_size) > self.remaining() {
            return Err(());
        }
        Ok(n)
    }

    fn string(&mut self) -> R<String> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Ok(String::new());
        }
        if len % 2 != 0 {
            return Err(());
        }
        let bytes = self.take(len as usize)?;
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        Ok(String::from_utf16_lossy(&units))
    }

    fn byte_array(&mut self) -> R<Vec<u8>> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Ok(Vec::new());
        }
        Ok(self.take(len as usize)?.to_vec())
    }

    fn variant(&mut self, depth: usize) -> R<Variant> {
        if depth > MAX_NESTING {
            return Err(());
        }
        let ty = self.u32()?;
        let _is_null = self.u8()?;
        Ok(match ty {
            T_INVALID => Variant::Invalid,
            T_BOOL => Variant::Bool(self.u8()? != 0),
            T_INT => Variant::Int(i64::from(self.u32()? as i32)),
            T_UINT => Variant::Int(i64::from(self.u32()?)),
            T_LONGLONG => Variant::Int(self.u64()? as i64),
            T_ULONGLONG => Variant::Int(self.u64()? as i64),
            T_DOUBLE => Variant::Double(f64::from_bits(self.u64()?)),
            T_STRING => Variant::String(self.string()?),
            T_STRINGLIST => {
                let n = self.count(4)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.string()?);
                }
                Variant::StringList(v)
            }
            T_BYTEARRAY => Variant::ByteArray(self.byte_array()?),
            T_DATETIME => {
                let jd = self.u32()?;
                let ms = self.u32()?;
                let spec = self.u8()?;
                Variant::DateTime(qt4_datetime(jd, ms, spec))
            }
            T_LIST => {
                let n = self.count(5)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.variant(depth + 1)?);
                }
                Variant::List(v)
            }
            T_HASH | T_MAP => Variant::Hash(self.hash(depth + 1)?),
            _ => return Err(()),
        })
    }

    fn hash(&mut self, depth: usize) -> R<BTreeMap<String, Variant>> {
        let n = self.count(9)?;
        let mut m = BTreeMap::new();
        for _ in 0..n {
            let k = self.string()?;
            let v = self.variant(depth)?;
            m.insert(k, v);
        }
        Ok(m)
    }
}

/// Qt 4 `QDateTime` stream data: Julian day, ms since midnight, spec
/// (2 = UTC, otherwise local time).
fn qt4_datetime(jd: u32, ms: u32, spec: u8) -> Option<i64> {
    if jd == 0 || ms == u32::MAX || ms >= 86_400_000 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(1970, 1, 1)?.checked_add_signed(chrono::Duration::days(i64::from(jd) - JD_EPOCH))?;
    let time = NaiveTime::from_num_seconds_from_midnight_opt(ms / 1000, (ms % 1000) * 1_000_000)?;
    let naive = NaiveDateTime::new(date, time);
    if spec == 2 {
        Some(naive.and_utc().timestamp_millis())
    } else {
        Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp_millis())
    }
}

/// Parses a legacy rules file. Error text matches qBittorrent.
pub(crate) fn import(data: &[u8]) -> Result<Vec<Rule>, String> {
    let mut r = Reader { data, pos: 0 };
    let dict = r.hash(0).map_err(|_| "Invalid data format".to_owned())?;
    let mut rules = Vec::new();
    for value in dict.values() {
        let Variant::Hash(d) = value else {
            continue;
        };
        rules.push(rule_from_legacy_dict(d));
    }
    Ok(rules)
}

/// `AutoDownloadRule::fromLegacyDict`.
fn rule_from_legacy_dict(d: &BTreeMap<String, Variant>) -> Rule {
    let get = |k: &str| d.get(k).cloned().unwrap_or(Variant::Invalid);
    let mut params = default_template();
    params.save_path = get("save_path").to_string_value();
    params.category = get("category_assigned").to_string_value();
    params.stopped = match get("add_paused").to_int() {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    };
    if !params.save_path.is_empty() {
        params.auto_tmm = Some(false);
    }
    let mut rule = Rule::new(&get("name").to_string_value());
    rule.feed_urls = get("affected_feeds").to_string_list();
    rule.enabled = get("enabled").to_bool(false);
    rule.last_match = match get("last_match") {
        Variant::DateTime(dt) => dt,
        _ => None,
    };
    rule.ignore_days = get("ignore_days").to_int().clamp(0, i64::from(i32::MAX));
    rule.params = params;
    rule.set_filters(
        get("use_regex").to_bool(false),
        &get("must_contain").to_string_value(),
        &get("must_not_contain").to_string_value(),
        &get("episode_filter").to_string_value(),
    );
    rule
}

// ---- Writer -----------------------------------------------------------------

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    put_u32(out, (units.len() * 2) as u32);
    for u in units {
        out.extend_from_slice(&u.to_be_bytes());
    }
}

fn put_variant(out: &mut Vec<u8>, v: &Variant) {
    let (ty, null) = match v {
        Variant::Invalid => (T_INVALID, 1),
        Variant::Bool(_) => (T_BOOL, 0),
        Variant::Int(_) => (T_INT, 0),
        Variant::Double(_) => (T_DOUBLE, 0),
        Variant::String(_) => (T_STRING, 0),
        Variant::StringList(_) => (T_STRINGLIST, 0),
        Variant::ByteArray(_) => (T_BYTEARRAY, 0),
        Variant::DateTime(dt) => (T_DATETIME, u8::from(dt.is_none())),
        Variant::List(_) => (T_LIST, 0),
        Variant::Hash(_) => (T_HASH, 0),
    };
    put_u32(out, ty);
    out.push(null);
    match v {
        Variant::Invalid => {}
        Variant::Bool(b) => out.push(u8::from(*b)),
        Variant::Int(i) => put_u32(out, *i as i32 as u32),
        Variant::Double(d) => out.extend_from_slice(&d.to_bits().to_be_bytes()),
        Variant::String(s) => put_string(out, s),
        Variant::StringList(l) => {
            put_u32(out, l.len() as u32);
            for s in l {
                put_string(out, s);
            }
        }
        Variant::ByteArray(b) => {
            put_u32(out, b.len() as u32);
            out.extend_from_slice(b);
        }
        Variant::DateTime(dt) => {
            let (jd, ms) = match dt.and_then(chrono::DateTime::from_timestamp_millis) {
                Some(d) => {
                    let days = d.timestamp().div_euclid(86_400);
                    let msecs = (d.timestamp_millis().rem_euclid(86_400_000)) as u32;
                    ((days + JD_EPOCH).clamp(0, i64::from(u32::MAX)) as u32, msecs)
                }
                None => (0, u32::MAX),
            };
            put_u32(out, jd);
            put_u32(out, ms);
            out.push(2); // UTC
        }
        Variant::List(l) => {
            put_u32(out, l.len() as u32);
            for x in l {
                put_variant(out, x);
            }
        }
        Variant::Hash(h) => put_hash(out, h),
    }
}

fn put_hash(out: &mut Vec<u8>, h: &BTreeMap<String, Variant>) {
    put_u32(out, h.len() as u32);
    for (k, v) in h {
        put_string(out, k);
        put_variant(out, v);
    }
}

/// `AutoDownloadRule::toLegacyDict` for every rule, serialized.
pub(crate) fn export(rules: &[Rule]) -> Vec<u8> {
    let mut dict = BTreeMap::new();
    for rule in rules {
        let p = &rule.params;
        let mut d = BTreeMap::new();
        d.insert("name".to_owned(), Variant::String(rule.name.clone()));
        d.insert("must_contain".to_owned(), Variant::String(rule.must_contain()));
        d.insert("must_not_contain".to_owned(), Variant::String(rule.must_not_contain()));
        d.insert("save_path".to_owned(), Variant::String(p.save_path.clone()));
        d.insert("affected_feeds".to_owned(), Variant::StringList(rule.feed_urls.clone()));
        d.insert("enabled".to_owned(), Variant::Bool(rule.enabled));
        d.insert("category_assigned".to_owned(), Variant::String(p.category.clone()));
        d.insert("use_regex".to_owned(), Variant::Bool(rule.use_regex));
        d.insert(
            "add_paused".to_owned(),
            Variant::Int(match p.stopped {
                None => 0,
                Some(true) => 1,
                Some(false) => 2,
            }),
        );
        d.insert("episode_filter".to_owned(), Variant::String(rule.episode_filter.clone()));
        d.insert("last_match".to_owned(), Variant::DateTime(rule.last_match));
        d.insert("ignore_days".to_owned(), Variant::Int(rule.ignore_days));
        dict.insert(rule.name.clone(), Variant::Hash(d));
    }
    let mut out = Vec::new();
    put_hash(&mut out, &dict);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut a = Rule::new("Show A");
        a.set_filters(false, "show a|show b", "cam", "1x2-;");
        a.feed_urls = vec!["https://a/rss".into(), "https://b/rss".into()];
        a.last_match = Some(1_791_000_306_000);
        a.ignore_days = 3;
        a.params.save_path = "/tv".into();
        a.params.category = "TV".into();
        a.params.stopped = Some(false);
        let mut b = Rule::new("Ünïcödé");
        b.enabled = false;
        b.set_filters(true, "^x", "", "");

        let data = export(&[a.clone(), b.clone()]);
        let mut rules = import(&data).unwrap();
        rules.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(rules.len(), 2);
        let ra = &rules[0];
        assert_eq!(ra.name, "Show A");
        assert!(ra.enabled);
        assert_eq!(ra.must_contain(), "show a|show b");
        assert_eq!(ra.must_not_contain(), "cam");
        assert_eq!(ra.episode_filter, "1x2-;");
        assert_eq!(ra.feed_urls, a.feed_urls);
        assert_eq!(ra.last_match, a.last_match);
        assert_eq!(ra.ignore_days, 3);
        assert_eq!(ra.params.save_path, "/tv");
        assert_eq!(ra.params.auto_tmm, Some(false));
        assert_eq!(ra.params.category, "TV");
        assert_eq!(ra.params.stopped, Some(false));
        let rb = &rules[1];
        assert_eq!(rb.name, "Ünïcödé");
        assert!(!rb.enabled);
        assert!(rb.use_regex);
        assert_eq!(rb.last_match, None);
        assert_eq!(rb.params.stopped, None);
    }

    /// A file written by Qt 6 (`QDataStream` version `Qt_4_5`) with the
    /// same dictionary layout as qBittorrent's legacy export.
    #[test]
    fn reads_qt_written_file() {
        let mut rules = import(include_bytes!("testdata/qt_rules.rssrules")).unwrap();
        rules.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(rules.len(), 2);
        let a = &rules[0];
        assert_eq!(a.name, "Show A");
        assert!(a.enabled);
        assert!(!a.use_regex);
        assert_eq!(a.must_contain(), "show*720p|other");
        assert_eq!(a.must_not_contain(), "cam");
        assert_eq!(a.episode_filter, "1x2-;");
        assert_eq!(a.feed_urls, ["https://a/rss", "https://b/rss"]);
        assert_eq!(a.last_match, Some(1_791_000_306_000));
        assert_eq!(a.ignore_days, 3);
        assert_eq!(a.params.save_path, "/tv");
        assert_eq!(a.params.category, "TV");
        assert_eq!(a.params.stopped, Some(false));
        let b = &rules[1];
        assert_eq!(b.name, "Ünïcödé");
        assert!(!b.enabled);
        assert!(b.use_regex);
        assert_eq!(b.must_contain(), "^x");
        assert_eq!(b.last_match, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(import(b"").is_err());
        assert!(import(&[0xFF, 0xFF, 0xFF, 0xFF]).is_err(), "absurd count");
        assert!(import(&[0, 0, 0, 1, 0, 0, 0, 3, 0, 0]).is_err(), "odd string length");
        // Deeply nested lists are bounded.
        let mut deep = vec![0, 0, 0, 1];
        put_string(&mut deep, "k");
        for _ in 0..100 {
            put_u32(&mut deep, T_LIST);
            deep.push(0);
            put_u32(&mut deep, 1);
        }
        assert!(import(&deep).is_err());
        // Pseudo-random data never panics.
        let mut seed = 12345u64;
        for len in 0..400 {
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                buf.push((seed >> 56) as u8);
            }
            let _ = import(&buf);
        }
        // An empty hash is valid.
        assert_eq!(import(&[0, 0, 0, 0]).unwrap().len(), 0);
    }
}
