//! HTTP-level helpers mirroring qBittorrent's `Http::RequestParser`:
//! query / form / multipart parameter parsing, cookie and Authorization
//! header parsing, gzip negotiation. Everything here is pure and bounded so
//! it can be fuzz-tested with hostile input.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;

use super::api::ApiError;

/// Maximum number of multipart parts accepted in one request.
pub const MAX_MULTIPART_PARTS: usize = 4096;
/// Maximum number of parameters accepted in a query string or form body.
pub const MAX_PARAMS: usize = 4096;

/// Request parameters (`GET`: query string, `POST`: body). Later duplicates
/// win, like qBittorrent's `QHash` insertion.
#[derive(Clone, Debug, Default)]
pub struct Params {
    map: HashMap<String, String>,
}

impl Params {
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut map = HashMap::new();
        for (k, v) in pairs.into_iter().take(MAX_PARAMS) {
            map.insert(k, v);
        }
        Self { map }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// Value or "" when absent (qBittorrent's `params()[key]`).
    pub fn str(&self, key: &str) -> &str {
        self.get(key).unwrap_or("")
    }

    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// `requireParams`: presence only; empty values satisfy it.
    pub fn require(&self, keys: &[&str]) -> Result<(), ApiError> {
        let missing: Vec<&str> = keys.iter().copied().filter(|k| !self.has(k)).collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ApiError::bad_params(format!("Missing required parameters: {}", missing.join(", "))))
        }
    }

    /// `Utils::String::parseBool`: only case-insensitive `true`/`false`.
    pub fn bool_opt(&self, key: &str) -> Option<bool> {
        parse_bool(self.str(key))
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.bool_opt(key).unwrap_or(default)
    }

    /// `Utils::String::parseInt` (32-bit).
    pub fn int_opt(&self, key: &str) -> Option<i64> {
        parse_int32(self.str(key))
    }

    /// `QString::toInt` (malformed -> 0).
    pub fn int(&self, key: &str) -> i64 {
        self.int_opt(key).unwrap_or(0)
    }

    /// `QString::toLongLong` (malformed -> 0).
    pub fn i64(&self, key: &str) -> i64 {
        parse_int_str(self.str(key)).unwrap_or(0)
    }

    /// `Utils::String::parseDouble` (finite values only).
    pub fn f64_opt(&self, key: &str) -> Option<f64> {
        parse_f64(self.str(key))
    }
}

pub fn parse_bool(s: &str) -> Option<bool> {
    if s.eq_ignore_ascii_case("true") {
        Some(true)
    } else if s.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

pub fn parse_int32(s: &str) -> Option<i64> {
    parse_int_str(s).filter(|v| i32::try_from(*v).is_ok())
}

/// Integer parsing like `QString::toLongLong` (surrounding whitespace and a
/// sign are accepted, nothing else).
pub fn parse_int_str(s: &str) -> Option<i64> {
    let t = s.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.strip_prefix('+').unwrap_or(t).parse().ok()
}

pub fn parse_f64(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.len() > 64 {
        return None;
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes `s` (invalid escapes are kept literally). With
/// `plus_as_space`, `+` is decoded to a space first (form encoding).
pub fn percent_decode(s: &[u8], plus_as_space: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'%'
            && i + 2 < s.len()
            && let (Some(h), Some(l)) = (hex_val(s[i + 1]), hex_val(s[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(if plus_as_space && b == b'+' { b' ' } else { b });
        i += 1;
    }
    out
}

/// Percent-decodes to a (lossy) UTF-8 string, without `+` handling.
pub fn percent_decode_str(s: &str) -> String {
    String::from_utf8_lossy(&percent_decode(s.as_bytes(), false)).into_owned()
}

/// Query string of a GET request. Parameters without a name (or without
/// `=`) are ignored, like qBittorrent.
pub fn parse_query(query: &str) -> Params {
    Params::from_pairs(query.split('&').filter_map(|param| {
        let eq = param.find('=')?;
        if eq == 0 {
            return None;
        }
        let name = String::from_utf8_lossy(&percent_decode(&param.as_bytes()[..eq], true)).into_owned();
        let value = String::from_utf8_lossy(&percent_decode(&param.as_bytes()[eq + 1..], true)).into_owned();
        Some((name, value))
    }))
}

/// `application/x-www-form-urlencoded` body.
pub fn parse_form(body: &[u8]) -> Params {
    Params::from_pairs(body.split(|b| *b == b'&').filter(|p| !p.is_empty()).map(|pair| {
        let (name, value) = match pair.iter().position(|b| *b == b'=') {
            Some(eq) => (&pair[..eq], &pair[eq + 1..]),
            None => (pair, &b""[..]),
        };
        (
            String::from_utf8_lossy(&percent_decode(name, true)).into_owned(),
            String::from_utf8_lossy(&percent_decode(value, true)).into_owned(),
        )
    }))
}

/// Uploaded files keyed by file name (qBittorrent's `DataMap`: duplicates
/// collide, the last one wins; iteration is sorted by name).
pub type Uploads = BTreeMap<String, Vec<u8>>;

#[derive(Debug, PartialEq, Eq)]
pub struct BadBody;

/// Parses a POST body according to its Content-Type. An empty body is valid
/// whatever the type; a non-empty body of an unknown type is rejected.
pub fn parse_body(content_type: &str, body: &[u8]) -> Result<(Params, Uploads), BadBody> {
    if body.is_empty() {
        return Ok((Params::default(), Uploads::new()));
    }
    let lower = content_type.trim().to_ascii_lowercase();
    if lower.starts_with("application/x-www-form-urlencoded") {
        return Ok((parse_form(body), Uploads::new()));
    }
    if lower.starts_with("multipart/form-data") {
        let boundary = header_param(content_type, "boundary").ok_or(BadBody)?;
        return parse_multipart(body, &boundary);
    }
    Err(BadBody)
}

/// Splits a header value into `;`-separated directives, honouring quotes.
fn split_directives(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&value[start..]);
    out
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        s.to_owned()
    }
}

/// Value of a `name=value` parameter inside a header (case-insensitive name).
pub fn header_param(header: &str, name: &str) -> Option<String> {
    split_directives(header).into_iter().skip(1).find_map(|d| {
        let (k, v) = d.split_once('=')?;
        k.trim().eq_ignore_ascii_case(name).then(|| unquote(v)).filter(|v| !v.is_empty())
    })
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    memchr::memmem::find(&hay[from..], needle).map(|p| p + from)
}

/// `multipart/form-data` (RFC 7578). Parts with a `filename` become uploads
/// keyed by file name; parts with only a `name` become string parameters.
pub fn parse_multipart(body: &[u8], boundary: &str) -> Result<(Params, Uploads), BadBody> {
    if boundary.is_empty() || boundary.len() > 200 {
        return Err(BadBody);
    }
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    let mut pos = find(body, delim, 0).ok_or(BadBody)? + delim.len();
    let mut params = Vec::new();
    let mut uploads = Uploads::new();
    let mut parts = 0;
    loop {
        if body[pos..].starts_with(b"--") {
            break; // closing delimiter
        }
        // Skip transport padding and the CRLF after the delimiter.
        while pos < body.len() && (body[pos] == b' ' || body[pos] == b'\t') {
            pos += 1;
        }
        if body[pos..].starts_with(b"\r\n") {
            pos += 2;
        } else if body[pos..].starts_with(b"\n") {
            pos += 1;
        } else {
            return Err(BadBody);
        }
        let next = find(body, delim, pos).ok_or(BadBody)?;
        let mut part = &body[pos..next];
        if part.ends_with(b"\r\n") {
            part = &part[..part.len() - 2];
        } else if part.ends_with(b"\n") {
            part = &part[..part.len() - 1];
        }
        parts += 1;
        if parts > MAX_MULTIPART_PARTS {
            return Err(BadBody);
        }
        parse_part(part, &mut params, &mut uploads)?;
        pos = next + delim.len();
        if pos >= body.len() {
            break; // tolerate a missing closing "--"
        }
    }
    Ok((Params::from_pairs(params), uploads))
}

fn parse_part(part: &[u8], params: &mut Vec<(String, String)>, uploads: &mut Uploads) -> Result<(), BadBody> {
    let (head_end, sep_len) = match find(part, b"\r\n\r\n", 0) {
        Some(p) => (p, 4),
        None => match find(part, b"\n\n", 0) {
            Some(p) => (p, 2),
            None => return Err(BadBody),
        },
    };
    let headers = String::from_utf8_lossy(&part[..head_end]);
    let payload = &part[head_end + sep_len..];
    let mut name = None;
    let mut filename = None;
    for line in headers.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let (hname, hvalue) = line.split_once(':').ok_or(BadBody)?;
        if !hname.trim().eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for d in split_directives(hvalue).into_iter().skip(1) {
            if let Some((k, v)) = d.split_once('=') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "name" => name = Some(unquote(v)),
                    "filename" => filename = Some(unquote(v)),
                    _ => {}
                }
            }
        }
    }
    if let Some(f) = filename {
        uploads.insert(f, payload.to_vec());
    } else if let Some(n) = name {
        params.push((n, String::from_utf8_lossy(payload).into_owned()));
    } else {
        return Err(BadBody);
    }
    Ok(())
}

/// Value of the cookie `name` from a `Cookie` header (RFC 6265 4.2.1).
pub fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|c| {
        let (k, v) = c.split_once('=')?;
        (k.trim() == name).then(|| unquote(v))
    })
}

/// `Authorization: <scheme> <credentials>`: scheme case-insensitive,
/// exactly one space, credentials trimmed and non-empty.
pub fn authorization<'a>(header: &'a str, scheme: &str) -> Option<&'a str> {
    if header.len() > scheme.len() + 1
        && header.as_bytes()[..scheme.len()].eq_ignore_ascii_case(scheme.as_bytes())
        && header.as_bytes()[scheme.len()] == b' '
    {
        let v = header[scheme.len() + 1..].trim();
        (!v.is_empty()).then_some(v)
    } else {
        None
    }
}

/// Whether `Accept-Encoding` allows gzip (`gzip` or `*` with q > 0).
pub fn accepts_gzip(header: &str) -> bool {
    header.split(',').any(|item| {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or("").trim();
        if !(coding.eq_ignore_ascii_case("gzip") || coding == "*") {
            return false;
        }
        for p in parts {
            if let Some((k, v)) = p.split_once('=')
                && k.trim().eq_ignore_ascii_case("q")
            {
                return v.trim().parse::<f32>().map(|q| q > 0.0).unwrap_or(false);
            }
        }
        true
    })
}

pub fn gzip(data: &[u8], level: u32) -> Option<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::with_capacity(data.len() / 3), flate2::Compression::new(level));
    enc.write_all(data).ok()?;
    enc.finish().ok()
}

pub fn gunzip(data: &[u8], max: usize) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut out = Vec::new();
    let dec = flate2::read::GzDecoder::new(data);
    dec.take(max as u64 + 1).read_to_end(&mut out).ok()?;
    (out.len() <= max).then_some(out)
}

/// Cookie / HTTP date: `Wed, 21 Oct 2015 07:28:00 GMT`.
pub fn http_date(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing() {
        let p = parse_query("a=1&b=x%20y&c=a+b&=skip&noeq&d=%zz&a=2&e=%E2%9C%93&f=");
        assert_eq!(p.get("a"), Some("2"));
        assert_eq!(p.get("b"), Some("x y"));
        assert_eq!(p.get("c"), Some("a b"));
        assert_eq!(p.get("d"), Some("%zz"));
        assert_eq!(p.get("e"), Some("✓"));
        assert_eq!(p.get("f"), Some(""));
        assert!(!p.has("noeq"));
        assert!(!p.has(""));
        // An encoded plus stays a plus.
        assert_eq!(parse_query("q=1%2B1").get("q"), Some("1+1"));
    }

    #[test]
    fn form_parsing() {
        let p = parse_form(b"username=admin&password=p%40ss+word&flag&&x=%");
        assert_eq!(p.get("username"), Some("admin"));
        assert_eq!(p.get("password"), Some("p@ss word"));
        assert_eq!(p.get("flag"), Some(""));
        assert_eq!(p.get("x"), Some("%"));
    }

    #[test]
    fn typed_params() {
        let p = parse_query("t=TRUE&f=false&x=1&i=+42&big=99999999999&neg=-7&d=1.5&nan=nan&inf=inf");
        assert_eq!(p.bool_opt("t"), Some(true));
        assert_eq!(p.bool_opt("f"), Some(false));
        assert_eq!(p.bool_opt("x"), None);
        assert_eq!(p.bool_opt("missing"), None);
        assert_eq!(p.int("i"), 42);
        assert_eq!(p.int("big"), 0);
        assert_eq!(p.i64("big"), 99_999_999_999);
        assert_eq!(p.int("neg"), -7);
        assert_eq!(p.int_opt("d"), None);
        assert_eq!(p.f64_opt("d"), Some(1.5));
        assert_eq!(p.f64_opt("nan"), None);
        assert_eq!(p.f64_opt("inf"), None);
        let e = p.require(&["t", "zz", "yy"]).unwrap_err();
        assert_eq!(e.message, "Missing required parameters: zz, yy");
        assert!(p.require(&["t", "f"]).is_ok());
    }

    fn multipart_body(boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut b = Vec::new();
        for (name, filename, data) in parts {
            b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            match filename {
                Some(f) => b.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: application/x-bittorrent\r\n\r\n").as_bytes(),
                ),
                None => b.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes()),
            }
            b.extend_from_slice(data);
            b.extend_from_slice(b"\r\n");
        }
        b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        b
    }

    #[test]
    fn multipart_parsing() {
        let body = multipart_body(
            "XyZ",
            &[
                ("urls", None, b"magnet:?xt=urn:btih:abc\nhttp://x/y.torrent"),
                ("torrents", Some("a.torrent"), b"d4:infod\r\n\r\nee"),
                ("torrents", Some("b;c.torrent"), b"BBB"),
                ("torrents", Some("a.torrent"), b"second"),
                ("savepath", None, b"/tmp/x"),
            ],
        );
        let (params, files) = parse_body("multipart/form-data; boundary=XyZ", &body).unwrap();
        assert_eq!(params.get("urls"), Some("magnet:?xt=urn:btih:abc\nhttp://x/y.torrent"));
        assert_eq!(params.get("savepath"), Some("/tmp/x"));
        assert_eq!(files.len(), 2);
        assert_eq!(files["a.torrent"], b"second");
        assert_eq!(files["b;c.torrent"], b"BBB");
        // quoted boundary
        let (p2, _) = parse_body("multipart/form-data; boundary=\"XyZ\"", &body).unwrap();
        assert_eq!(p2.get("savepath"), Some("/tmp/x"));
    }

    #[test]
    fn body_errors() {
        assert!(parse_body("text/plain", b"x").is_err());
        assert!(parse_body("text/plain", b"").is_ok());
        assert!(parse_body("multipart/form-data", b"--x\r\n").is_err());
        assert!(parse_body("multipart/form-data; boundary=", b"--x\r\n").is_err());
        assert!(parse_body("multipart/form-data; boundary=b", b"no delimiter here").is_err());
        assert!(parse_body("multipart/form-data; boundary=b", b"--b\r\nX: y\r\n\r\ndata\r\n--b--").is_err());
        assert!(parse_body("multipart/form-data; boundary=b", b"--b\r\nContent-Disposition: form-data\r\n\r\nx\r\n--b--").is_err());
        assert!(parse_body("multipart/form-data; boundary=b", b"--b\r\nno headers end").is_err());
    }

    /// Deterministic pseudo-random fuzzing of every parser: they must never
    /// panic on hostile input.
    #[test]
    fn fuzz_parsers_never_panic() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet: &[u8] = b"-=&%+;:\"\\\r\n \tab01Fz\xff\xfe\x00\xe2\x9c\x93/[]";
        for round in 0..4000 {
            let len = (next() % 300) as usize;
            let mut data: Vec<u8> = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            if round % 3 == 0 {
                data.splice(0..0, b"--b\r\nContent-Disposition: form-data; name=\"x\"; filename=\"".iter().copied());
            }
            let s = String::from_utf8_lossy(&data).into_owned();
            let _ = parse_query(&s);
            let _ = parse_form(&data);
            let _ = parse_body("multipart/form-data; boundary=b", &data);
            let _ = parse_body("application/x-www-form-urlencoded", &data);
            let _ = cookie_value(&s, "QBT_SID_8080");
            let _ = authorization(&s, "Bearer");
            let _ = accepts_gzip(&s);
            let _ = header_param(&s, "boundary");
            let _ = percent_decode(&data, true);
            let _ = parse_f64(&s);
            let _ = parse_int32(&s);
            let _ = gunzip(&data, 1000);
        }
    }

    #[test]
    fn cookies_and_auth_headers() {
        assert_eq!(cookie_value("a=1; QBT_SID_8080=abc+/=; b=2", "QBT_SID_8080"), Some("abc+/=".into()));
        assert_eq!(cookie_value("QBT_SID_8080=\"q\"", "QBT_SID_8080"), Some("q".into()));
        assert_eq!(cookie_value("QBT_SID_8081=x", "QBT_SID_8080"), None);
        assert_eq!(authorization("Bearer  qbt_x ", "Bearer"), Some("qbt_x"));
        assert_eq!(authorization("bearer qbt_x", "Bearer"), Some("qbt_x"));
        assert_eq!(authorization("Bearerqbt_x", "Bearer"), None);
        assert_eq!(authorization("Bearer ", "Bearer"), None);
        assert_eq!(authorization("Basic YTpi", "Bearer"), None);
        assert_eq!(authorization("Bé", "Bearer"), None);
    }

    #[test]
    fn gzip_negotiation() {
        assert!(accepts_gzip("gzip, deflate, br"));
        assert!(accepts_gzip("br;q=1.0, GZIP;q=0.5"));
        assert!(accepts_gzip("*"));
        assert!(!accepts_gzip("gzip;q=0"));
        assert!(!accepts_gzip("identity"));
        assert!(!accepts_gzip(""));
        let data = vec![b'a'; 5000];
        let z = gzip(&data, 6).unwrap();
        assert!(z.len() < 100);
        assert_eq!(gunzip(&z, 10_000).unwrap(), data);
        assert!(gunzip(&z, 100).is_none());
    }
}
