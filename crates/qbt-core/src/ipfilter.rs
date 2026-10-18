//! IP filter parsing (`filterparserthread.cpp`): eMule `.dat`, PeerGuardian
//! `.p2p` text and `.p2b` binary (v1/v2/v3), plus the manual ban list.
//!
//! Input is untrusted (downloaded block lists): parsing streams the file,
//! caps line length, rule count and file size, never panics, and keeps the
//! result as sorted, merged inclusive ranges (8 bytes per IPv4 range).

use std::io::{BufRead, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

/// Largest filter file accepted.
pub const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;
/// Upper bound on stored rules (memory guard).
pub const MAX_RULES: usize = 8_000_000;
const MAX_LINE: usize = 4096;
const MAX_LOGGED_ERRORS: usize = 5;

/// Blocked address ranges (inclusive), sorted and merged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpFilter {
    v4: Vec<(u32, u32)>,
    v6: Vec<(u128, u128)>,
}

impl IpFilter {
    pub fn add(&mut self, start: IpAddr, end: IpAddr) -> bool {
        if self.v4.len() + self.v6.len() >= MAX_RULES {
            return false;
        }
        match (start, end) {
            (IpAddr::V4(s), IpAddr::V4(e)) if s <= e => self.v4.push((s.into(), e.into())),
            (IpAddr::V6(s), IpAddr::V6(e)) if s <= e => self.v6.push((s.into(), e.into())),
            _ => return false,
        }
        true
    }

    pub fn add_ip(&mut self, ip: IpAddr) -> bool {
        // Ban IPv4-mapped IPv6 addresses as plain IPv4.
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
            v4 => v4,
        };
        self.add(ip, ip)
    }

    /// Sorts and merges overlapping/adjacent ranges.
    pub fn normalize(&mut self) {
        fn merge<T: Ord + Copy>(v: &mut Vec<(T, T)>, succ: impl Fn(T) -> Option<T>) {
            v.sort_unstable();
            let mut out: Vec<(T, T)> = Vec::with_capacity(v.len());
            for &(s, e) in v.iter() {
                if let Some(last) = out.last_mut() {
                    let touches = succ(last.1).is_none_or(|n| s <= n);
                    if touches {
                        if e > last.1 {
                            last.1 = e;
                        }
                        continue;
                    }
                }
                out.push((s, e));
            }
            out.shrink_to_fit();
            *v = out;
        }
        merge(&mut self.v4, |x: u32| x.checked_add(1));
        merge(&mut self.v6, |x: u128| x.checked_add(1));
    }

    pub fn merge_from(&mut self, other: &IpFilter) {
        self.v4.extend_from_slice(&other.v4);
        self.v6.extend_from_slice(&other.v6);
        self.normalize();
    }

    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Binary search; requires [`IpFilter::normalize`].
    pub fn contains(&self, ip: IpAddr) -> bool {
        fn find<T: Ord + Copy>(v: &[(T, T)], x: T) -> bool {
            let i = v.partition_point(|r| r.0 <= x);
            i > 0 && v[i - 1].1 >= x
        }
        match ip {
            IpAddr::V4(a) => find(&self.v4, u32::from(a)),
            IpAddr::V6(a) => match a.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, u32::from(v4)),
                None => find(&self.v6, u128::from(a)),
            },
        }
    }

    /// Writes the ranges in the engine's block-list text format
    /// (`name:start-end`, one per line).
    pub fn write_engine_list(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        for &(s, e) in &self.v4 {
            writeln!(out, "b:{}-{}", Ipv4Addr::from(s), Ipv4Addr::from(e))?;
        }
        for &(s, e) in &self.v6 {
            writeln!(out, "b:{}-{}", Ipv6Addr::from(s), Ipv6Addr::from(e))?;
        }
        Ok(())
    }
}

/// Result of parsing a filter file.
#[derive(Debug, Default)]
pub struct ParseResult {
    pub filter: IpFilter,
    /// Rules accepted (before merging), as qBittorrent reports them.
    pub rule_count: usize,
    /// First few error messages (qBittorrent logs at most five).
    pub errors: Vec<String>,
    pub error_count: usize,
}

impl ParseResult {
    fn error(&mut self, msg: String) {
        self.error_count += 1;
        if self.errors.len() < MAX_LOGGED_ERRORS {
            self.errors.push(msg);
        }
    }

    fn rule(&mut self, start: IpAddr, end: IpAddr) {
        if self.filter.add(start, end) {
            self.rule_count += 1;
        }
    }

    fn finish(mut self) -> Self {
        self.filter.normalize();
        self
    }
}

/// IPv4 with leading zeros per octet (`001.002.003.004`), else IPv4/IPv6.
pub fn parse_ip(s: &str) -> Option<IpAddr> {
    let s = s.trim();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit())) {
        let mut o = [0u8; 4];
        for (i, p) in parts.iter().enumerate() {
            let v: u32 = p.parse().ok()?;
            o[i] = u8::try_from(v).ok()?;
        }
        return Some(IpAddr::V4(Ipv4Addr::from(o)));
    }
    s.parse().ok()
}

fn parse_range(range: &str, line_no: usize, out: &mut ParseResult) {
    let Some((a, b)) = range.split_once('-') else {
        out.error(format!("IP filter line {line_no} is malformed."));
        return;
    };
    let Some(start) = parse_ip(a) else {
        out.error(format!("IP filter line {line_no} is malformed. Start IP of the range is malformed."));
        return;
    };
    let Some(end) = parse_ip(b) else {
        out.error(format!("IP filter line {line_no} is malformed. End IP of the range is malformed."));
        return;
    };
    if start.is_ipv4() != end.is_ipv4() {
        out.error(format!("IP filter line {line_no} is malformed. One IP is IPv4 and the other is IPv6!"));
        return;
    }
    if start > end {
        out.error(format!("IP filter line {line_no} is malformed. End IP is lower than Start IP!"));
        return;
    }
    out.rule(start, end);
}

/// Iterates lines with a length cap (longer lines are reported as `None`).
fn for_each_line(reader: impl BufRead, mut f: impl FnMut(usize, Option<&str>)) {
    let mut reader = reader;
    let mut buf = Vec::with_capacity(256);
    let mut line_no = 0;
    loop {
        buf.clear();
        // Read up to MAX_LINE bytes or a newline.
        let mut too_long = false;
        loop {
            let available = match reader.fill_buf() {
                Ok(b) => b,
                Err(_) => return,
            };
            if available.is_empty() {
                break;
            }
            let (chunk, found) = match available.iter().position(|&b| b == b'\n') {
                Some(i) => (&available[..=i], true),
                None => (available, false),
            };
            let n = chunk.len();
            if buf.len() + n <= MAX_LINE {
                buf.extend_from_slice(chunk);
            } else {
                too_long = true;
            }
            reader.consume(n);
            if found {
                break;
            }
        }
        if buf.is_empty() && !too_long {
            return;
        }
        line_no += 1;
        if too_long {
            f(line_no, None);
            continue;
        }
        let text = String::from_utf8_lossy(&buf);
        f(line_no, Some(text.trim_end_matches(['\n', '\r'])));
    }
}

fn is_comment_or_blank(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty() || t.starts_with('#') || t.starts_with("//")
}

/// Leading integer like `strtol` (0 if none).
fn leading_int(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let n: String = digits.chars().take_while(char::is_ascii_digit).take(18).collect();
    let v: i64 = n.parse().unwrap_or(0);
    if neg { -v } else { v }
}

/// eMule DAT: `start - end , access , description`; access > 127 = allowed.
pub fn parse_dat(reader: impl BufRead) -> ParseResult {
    let mut out = ParseResult::default();
    for_each_line(reader, |no, line| {
        let Some(line) = line else {
            out.error(format!("IP filter line {no} is malformed."));
            return;
        };
        if is_comment_or_blank(line) {
            return;
        }
        let mut fields = line.splitn(3, ',');
        let range = fields.next().unwrap_or("");
        if let Some(access) = fields.next()
            && leading_int(access) > 127
        {
            return;
        }
        parse_range(range, no, &mut out);
    });
    out.finish()
}

/// PeerGuardian text: `description:start-end` (the description may contain
/// `:`; IPv6 ranges are recognised by trying each `:` from the right).
pub fn parse_p2p(reader: impl BufRead) -> ParseResult {
    let mut out = ParseResult::default();
    for_each_line(reader, |no, line| {
        let Some(line) = line else {
            out.error(format!("IP filter line {no} is malformed."));
            return;
        };
        if is_comment_or_blank(line) {
            return;
        }
        let candidates: Vec<usize> = line.match_indices(':').map(|(i, _)| i).collect();
        let Some(&last) = candidates.last() else {
            out.error(format!("IP filter line {no} is malformed."));
            return;
        };
        // Prefer the last ':' (qBittorrent); fall back to earlier ones for
        // IPv6 ranges whose addresses contain ':'.
        for &i in candidates.iter().rev() {
            let range = &line[i + 1..];
            if let Some((a, b)) = range.split_once('-')
                && parse_ip(a).is_some()
                && parse_ip(b).is_some()
            {
                parse_range(range, no, &mut out);
                return;
            }
        }
        parse_range(&line[last + 1..], no, &mut out);
    });
    out.finish()
}

const P2B_ERROR: &str = "Parsing Error: The filter file is not a valid PeerGuardian P2B file.";

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_be_bytes(b))
}

/// Reads a NUL-terminated name (bounded). None on EOF before any byte or on
/// an over-long name.
fn read_cstr(r: &mut impl BufRead) -> Option<()> {
    let mut len = 0usize;
    let mut any = false;
    loop {
        let buf = r.fill_buf().ok()?;
        if buf.is_empty() {
            return if any { Some(()) } else { None };
        }
        any = true;
        match buf.iter().position(|&b| b == 0) {
            Some(i) => {
                r.consume(i + 1);
                return Some(());
            }
            None => {
                len += buf.len();
                let n = buf.len();
                r.consume(n);
                if len > MAX_LINE {
                    return None;
                }
            }
        }
    }
}

/// PeerGuardian binary (P2B) v1, v2 and v3.
pub fn parse_p2b(reader: impl BufRead) -> Result<ParseResult, String> {
    let mut r = reader;
    let mut out = ParseResult::default();
    let mut header = [0u8; 8];
    r.read_exact(&mut header).map_err(|_| P2B_ERROR.to_string())?;
    if &header[..7] != b"\xFF\xFF\xFF\xFFP2B" {
        return Err(P2B_ERROR.into());
    }
    match header[7] {
        1 | 2 => {
            while read_cstr(&mut r).is_some() {
                let (Some(s), Some(e)) = (read_u32(&mut r), read_u32(&mut r)) else {
                    out.error(P2B_ERROR.into());
                    break;
                };
                if s <= e {
                    out.rule(IpAddr::V4(s.into()), IpAddr::V4(e.into()));
                }
                if out.filter.len() >= MAX_RULES {
                    break;
                }
            }
        }
        3 => {
            let names = read_u32(&mut r).ok_or(P2B_ERROR)?;
            for _ in 0..names {
                read_cstr(&mut r).ok_or(P2B_ERROR)?;
            }
            let ranges = read_u32(&mut r).ok_or(P2B_ERROR)?;
            for _ in 0..ranges {
                let (Some(_name), Some(s), Some(e)) = (read_u32(&mut r), read_u32(&mut r), read_u32(&mut r)) else {
                    out.error(P2B_ERROR.into());
                    break;
                };
                if s <= e {
                    out.rule(IpAddr::V4(s.into()), IpAddr::V4(e.into()));
                }
                if out.filter.len() >= MAX_RULES {
                    break;
                }
            }
        }
        _ => return Err(P2B_ERROR.into()),
    }
    Ok(out.finish())
}

/// Parses a filter file, choosing the format by extension like qBittorrent
/// (unknown extensions yield 0 rules).
pub fn parse_file(path: &Path) -> Result<ParseResult, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("I/O Error: Could not open IP filter file in read mode. {e}"))?;
    if !meta.is_file() {
        return Err("I/O Error: Could not open IP filter file in read mode.".into());
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(format!("IP filter file is too large ({} bytes)", meta.len()));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("I/O Error: Could not open IP filter file in read mode. {e}"))?;
    let reader = std::io::BufReader::with_capacity(2 * 1024 * 1024, file.take(MAX_FILE_SIZE));
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "p2p" => Ok(parse_p2p(reader)),
        "p2b" => parse_p2b(reader),
        "dat" => Ok(parse_dat(reader)),
        _ => Ok(ParseResult::default()),
    }
}

/// Manual bans: returns the filter and the entries that are not valid IPs.
pub fn parse_banned(list: &[String]) -> (IpFilter, Vec<String>) {
    let mut f = IpFilter::default();
    let mut invalid = Vec::new();
    for entry in list {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        match e.parse::<IpAddr>() {
            Ok(ip) => {
                f.add_ip(ip);
            }
            Err(_) => invalid.push(e.to_string()),
        }
    }
    f.normalize();
    (f, invalid)
}

/// Normalises an address for the ban list (RFC 5952 for IPv6).
pub fn normalize_ip(s: &str) -> Option<String> {
    let s = s.trim();
    let ip: IpAddr = s
        .parse()
        .ok()
        .or_else(|| s.parse::<std::net::SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| s.rsplit_once(':').and_then(|(h, _)| h.parse().ok()))?;
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    Some(ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn dat_format() {
        let data = "# comment\n// also\n\n001.009.096.105 - 001.009.096.110 , 000 , Some org\n10.0.0.0 - 10.0.0.255 , 200 , allowed\n1.2.3.4 - 1.2.3.1 , 0 , reversed\nbad line\n192.168.0.1-192.168.0.2\n2001:db8::1 - 2001:db8::ff , 0 , v6\n1.1.1.1 - ::1 , 0 , mixed\n";
        let r = parse_dat(Cursor::new(data));
        assert_eq!(r.rule_count, 3);
        assert!(r.filter.contains(ip("1.9.96.107")));
        assert!(!r.filter.contains(ip("10.0.0.1")));
        assert!(r.filter.contains(ip("192.168.0.2")));
        assert!(r.filter.contains(ip("2001:db8::10")));
        assert!(!r.filter.contains(ip("2001:db8::1:0")));
        assert_eq!(r.error_count, 3);
        assert!(r.errors[0].contains("End IP is lower than Start IP"));
        assert!(r.errors[2].contains("One IP is IPv4 and the other is IPv6"));
    }

    #[test]
    fn p2p_format() {
        let data = "Org: with colon:1.0.0.0-1.255.255.255\nv6 range:2001:db8::1-2001:db8::2\r\nno range here\n";
        let r = parse_p2p(Cursor::new(data));
        assert_eq!(r.rule_count, 2);
        assert!(r.filter.contains(ip("1.2.3.4")));
        assert!(r.filter.contains(ip("2001:db8::2")));
        assert_eq!(r.error_count, 1);
    }

    #[test]
    fn p2b_versions() {
        let mut v2 = b"\xFF\xFF\xFF\xFFP2B\x02".to_vec();
        v2.extend_from_slice(b"name\0");
        v2.extend_from_slice(&u32::from(Ipv4Addr::new(5, 0, 0, 0)).to_be_bytes());
        v2.extend_from_slice(&u32::from(Ipv4Addr::new(5, 0, 0, 255)).to_be_bytes());
        let r = parse_p2b(Cursor::new(v2)).unwrap();
        assert_eq!(r.rule_count, 1);
        assert!(r.filter.contains(ip("5.0.0.7")));

        let mut v3 = b"\xFF\xFF\xFF\xFFP2B\x03".to_vec();
        v3.extend_from_slice(&2u32.to_be_bytes());
        v3.extend_from_slice(b"a\0b\0");
        v3.extend_from_slice(&1u32.to_be_bytes());
        v3.extend_from_slice(&1u32.to_be_bytes());
        v3.extend_from_slice(&u32::from(Ipv4Addr::new(6, 0, 0, 0)).to_be_bytes());
        v3.extend_from_slice(&u32::from(Ipv4Addr::new(6, 0, 0, 9)).to_be_bytes());
        let r = parse_p2b(Cursor::new(v3)).unwrap();
        assert!(r.filter.contains(ip("6.0.0.9")));

        assert!(parse_p2b(Cursor::new(b"junk".to_vec())).is_err());
        assert!(parse_p2b(Cursor::new(b"\xFF\xFF\xFF\xFFP2B\x09".to_vec())).is_err());
        // Truncated v3 with a huge declared count must not allocate or hang.
        let mut trunc = b"\xFF\xFF\xFF\xFFP2B\x03".to_vec();
        trunc.extend_from_slice(&0u32.to_be_bytes());
        trunc.extend_from_slice(&u32::MAX.to_be_bytes());
        let r = parse_p2b(Cursor::new(trunc)).unwrap();
        assert_eq!(r.rule_count, 0);
        assert_eq!(r.error_count, 1);
    }

    #[test]
    fn merging_and_lookup() {
        let mut f = IpFilter::default();
        f.add(ip("1.0.0.0"), ip("1.0.0.10"));
        f.add(ip("1.0.0.11"), ip("1.0.0.20"));
        f.add(ip("1.0.0.5"), ip("1.0.0.6"));
        f.add(ip("255.255.255.255"), ip("255.255.255.255"));
        f.add_ip(ip("::ffff:9.9.9.9"));
        f.normalize();
        assert_eq!(f.len(), 3);
        assert!(f.contains(ip("1.0.0.15")));
        assert!(f.contains(ip("9.9.9.9")));
        assert!(f.contains(ip("::ffff:1.0.0.1")));
        assert!(f.contains(ip("255.255.255.255")));
        assert!(!f.contains(ip("1.0.0.21")));
        let mut text = Vec::new();
        f.write_engine_list(&mut text).unwrap();
        assert!(String::from_utf8(text).unwrap().contains("b:1.0.0.0-1.0.0.20\n"));
    }

    #[test]
    fn hostile_input() {
        let long = format!("{}\nx:1.1.1.1-1.1.1.2\n", "x".repeat(100_000));
        let r = parse_p2p(Cursor::new(long));
        assert_eq!(r.rule_count, 1);
        assert_eq!(r.error_count, 1);
        let r = parse_dat(Cursor::new(vec![0xffu8; 10_000]));
        assert_eq!(r.rule_count, 0);
        assert_eq!(parse_ip("256.1.1.1"), None);
        assert_eq!(parse_ip("001.002.003.004"), Some(ip("1.2.3.4")));
    }

    #[test]
    fn banned_list() {
        let (f, invalid) = parse_banned(&["1.2.3.4".into(), "nope".into(), " ::1 ".into()]);
        assert!(f.contains(ip("1.2.3.4")));
        assert!(f.contains(ip("::1")));
        assert_eq!(invalid, vec!["nope".to_string()]);
        assert_eq!(normalize_ip("1.2.3.4:6881").as_deref(), Some("1.2.3.4"));
        assert_eq!(normalize_ip("[2001:DB8::1]:80").as_deref(), Some("2001:db8::1"));
        assert_eq!(normalize_ip("x"), None);
    }
}
