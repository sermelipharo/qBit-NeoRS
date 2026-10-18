use std::io::Write as _;

use super::*;
use crate::net::testutil;

// ---- a tiny MaxMind DB writer (IPv4 tree, 24-bit records) -------------------

enum V {
    Str(&'static str),
    U16(u16),
    U32(u32),
    U64(u64),
    Map(Vec<(&'static str, V)>),
    Arr(Vec<V>),
}

fn control(out: &mut Vec<u8>, ty: u8, size: usize) {
    assert!(size < 29);
    if ty <= 7 {
        out.push((ty << 5) | size as u8);
    } else {
        out.push(size as u8);
        out.push(ty - 7);
    }
}

fn uint(out: &mut Vec<u8>, ty: u8, v: u64) {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count();
    control(out, ty, 8 - skip);
    out.extend_from_slice(&bytes[skip..]);
}

fn encode(out: &mut Vec<u8>, v: &V) {
    match v {
        V::Str(s) => {
            control(out, 2, s.len());
            out.extend_from_slice(s.as_bytes());
        }
        V::U16(n) => uint(out, 5, *n as u64),
        V::U32(n) => uint(out, 6, *n as u64),
        V::U64(n) => uint(out, 9, *n),
        V::Map(entries) => {
            control(out, 7, entries.len());
            for (k, v) in entries {
                encode(out, &V::Str(k));
                encode(out, v);
            }
        }
        V::Arr(items) => {
            control(out, 11, items.len());
            for i in items {
                encode(out, i);
            }
        }
    }
}

/// Builds a country database mapping each (first octet) /8 to a code.
fn build_mmdb(entries: &[(u8, &'static str)], build_epoch: u64, db_type: &'static str) -> Vec<u8> {
    // Data section.
    let mut data = Vec::new();
    let mut offsets = Vec::new();
    for (_, code) in entries {
        offsets.push(data.len());
        encode(&mut data, &V::Map(vec![("country", V::Map(vec![("iso_code", V::Str(code))]))]));
    }
    // Trie over the first 8 bits.
    #[derive(Clone, Copy)]
    enum Rec {
        Empty,
        Node(usize),
        Data(usize),
    }
    let mut nodes: Vec<[Rec; 2]> = vec![[Rec::Empty, Rec::Empty]];
    for (i, (octet, _)) in entries.iter().enumerate() {
        let mut n = 0;
        for bit in (0..8).rev() {
            let b = ((octet >> bit) & 1) as usize;
            if bit == 0 {
                nodes[n][b] = Rec::Data(offsets[i]);
            } else {
                n = match nodes[n][b] {
                    Rec::Node(next) => next,
                    _ => {
                        nodes.push([Rec::Empty, Rec::Empty]);
                        let next = nodes.len() - 1;
                        nodes[n][b] = Rec::Node(next);
                        next
                    }
                };
            }
        }
    }
    let count = nodes.len();
    let mut out = Vec::new();
    for node in &nodes {
        for rec in node {
            let v = match *rec {
                Rec::Empty => count,
                Rec::Node(i) => i,
                Rec::Data(off) => count + 16 + off,
            } as u32;
            out.extend_from_slice(&v.to_be_bytes()[1..]);
        }
    }
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&data);
    out.extend_from_slice(b"\xAB\xCD\xEFMaxMind.com");
    encode(
        &mut out,
        &V::Map(vec![
            ("binary_format_major_version", V::U16(2)),
            ("binary_format_minor_version", V::U16(0)),
            ("build_epoch", V::U64(build_epoch)),
            ("database_type", V::Str(db_type)),
            ("description", V::Map(vec![("en", V::Str("test"))])),
            ("ip_version", V::U16(4)),
            ("languages", V::Arr(vec![V::Str("en")])),
            ("node_count", V::U32(count as u32)),
            ("record_size", V::U16(24)),
        ]),
    );
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

const DB_TYPE: &str = "DBIP-Country-Lite";

#[test]
fn lookups_in_generated_database() {
    let db = build_mmdb(&[(1, "AU"), (2, "fr")], 1_700_000_000, DB_TYPE);
    let reader = Reader::from_source(db.as_slice()).unwrap();
    assert_eq!(lookup(&reader, "1.2.3.4".parse().unwrap()).as_deref(), Some("AU"));
    assert_eq!(lookup(&reader, "2.255.0.1".parse().unwrap()).as_deref(), Some("FR"));
    assert_eq!(lookup(&reader, "3.3.3.3".parse().unwrap()), None);
    // IPv4-mapped IPv6 peers resolve as IPv4.
    assert_eq!(lookup(&reader, "::ffff:1.2.3.4".parse().unwrap()).as_deref(), Some("AU"));
    assert_eq!(lookup(&reader, "2001:db8::1".parse().unwrap()), None);
    assert!(loaded_message(&reader).starts_with("IP geolocation database loaded. Type: DBIP-Country-Lite. Build time: Tue Nov 14"));
}

/// Unpacks into a temporary directory; returns the unpacked bytes.
fn validate_download(gz: &[u8], max_size: u64) -> Result<Vec<u8>, String> {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("db.mmdb");
    let result = unpack_download(gz, &out, max_size).map(|reader| {
        assert_eq!(reader.metadata().database_type, DB_TYPE);
        std::fs::read(&out).unwrap()
    });
    // A failed unpack leaves nothing behind.
    assert_eq!(out.exists(), result.is_ok());
    result
}

#[test]
fn validates_downloads() {
    let db = build_mmdb(&[(1, "AU")], 1_700_000_000, DB_TYPE);
    assert_eq!(validate_download(&gzip(&db), 1 << 20).unwrap(), db);
    // The output is created exclusively.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("db.mmdb");
    std::fs::write(&out, b"planted").unwrap();
    assert!(unpack_download(gzip(&db).as_slice(), &out, 1 << 20).is_err());
    // Decompressed size cap (zip bombs).
    assert!(validate_download(&gzip(&db), 100).unwrap_err().contains("decompress"));
    assert!(validate_download(&gzip(&vec![0u8; 4 << 20]), 1 << 20).is_err());
    // Not gzip / not a MaxMind DB / wrong database type.
    assert!(validate_download(b"plain", 1 << 20).is_err());
    assert!(validate_download(&gzip(b"not a database"), 1 << 20).is_err());
    let city = build_mmdb(&[(1, "AU")], 1_700_000_000, "DBIP-City-Lite");
    assert!(validate_download(&gzip(&city), 1 << 20).unwrap_err().contains("unexpected database type"));
    // Corrupted tree pointing outside the data section.
    let mut bad = db.clone();
    bad[0..3].copy_from_slice(&[0x7f, 0xff, 0xff]);
    assert!(validate_download(&gzip(&bad), 1 << 20).is_err());
}

#[test]
fn expiry_and_urls() {
    let ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    assert!(!is_expired(ts("2026-10-01T00:00:00Z"), ts("2026-10-14T00:00:00Z")));
    assert!(is_expired(ts("2026-09-01T00:00:00Z"), ts("2026-10-14T00:00:00Z")));
    // On the 1st the new file may not exist yet.
    assert!(!is_expired(ts("2026-09-01T00:00:00Z"), ts("2026-10-01T12:00:00Z")));
    assert!(is_expired(ts("2025-12-01T00:00:00Z"), ts("2026-01-05T00:00:00Z")));
    assert_eq!(
        database_urls(ts("2026-01-05T00:00:00Z")),
        [
            "https://download.db-ip.com/free/dbip-country-lite-2026-01.mmdb.gz",
            "https://download.db-ip.com/free/dbip-country-lite-2025-12.mmdb.gz"
        ]
    );
}

#[test]
fn country_names() {
    assert_eq!(country_name("US"), "United States");
    assert_eq!(country_name("de"), "Germany");
    assert_eq!(country_name("CI"), "Cote d'Ivoire");
    assert_eq!(country_name(""), "N/A");
    assert_eq!(country_name("ZZ"), "N/A");
    assert!(countries::COUNTRIES.windows(2).all(|w| w[0].0 < w[1].0));
}

#[test]
fn launch_loads_and_unloads() {
    let rt = testutil::runtime();
    let dir = tempfile::tempdir().unwrap();
    let ctx = testutil::context(&rt, dir.path());
    // A current database: no download is attempted.
    let now = chrono::Utc::now().timestamp() as u64;
    let db = build_mmdb(&[(1, "AU")], now, DB_TYPE);
    std::fs::create_dir_all(ctx.profile.geoip_dir()).unwrap();
    std::fs::write(ctx.profile.geoip_dir().join(DATABASE_FILE), db).unwrap();

    let geo = GeoIp::launch(&ctx);
    let wait = |want: bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while geo.is_loaded() != want && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(geo.is_loaded(), want);
    };
    wait(true);
    assert_eq!(geo.country_code("1.1.1.1".parse().unwrap()).as_deref(), Some("AU"));
    ctx.prefs.update(|p| p.advanced.resolve_peer_countries = false).unwrap();
    wait(false);
    assert_eq!(geo.country_code("1.1.1.1".parse().unwrap()), None);
}
