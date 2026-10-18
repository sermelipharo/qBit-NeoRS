//! Tracker responses (HTTP and UDP share these types) and the HTTP response
//! parsers. Every field coming from a tracker is treated as hostile: sizes are
//! capped, strings are sanitized and malformed values are ignored.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use librqbit_core::hash_id::Id20;

use crate::bdecode::{self, Value};
use crate::error::TrackerError;

/// Maximum number of peers accepted from a single response.
pub const MAX_PEERS_PER_RESPONSE: usize = 1000;
/// Maximum length (in bytes) of a message shown to the user.
pub const MAX_MESSAGE_LEN: usize = 512;
/// Maximum length of an opaque tracker id echoed back to the tracker.
pub const MAX_TRACKER_ID_LEN: usize = 128;

/// A successful announce response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnounceResponse {
    /// Seconds until the next regular announce.
    pub interval: Option<u32>,
    /// Seconds the tracker asks us to wait before announcing again.
    pub min_interval: Option<u32>,
    /// Opaque id to send back as `trackerid` on subsequent announces.
    pub tracker_id: Option<Vec<u8>>,
    /// Seeders in the swarm.
    pub complete: Option<u32>,
    /// Leechers in the swarm.
    pub incomplete: Option<u32>,
    /// Times the torrent was downloaded (non-standard HTTP extension).
    pub downloaded: Option<u32>,
    pub warning_message: Option<String>,
    pub peers: Vec<SocketAddr>,
    /// Our address as seen by the tracker (BEP 24).
    pub external_ip: Option<IpAddr>,
}

/// Scrape counters for one info hash.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrapeData {
    pub complete: Option<u32>,
    pub incomplete: Option<u32>,
    pub downloaded: Option<u32>,
}

/// Make a tracker-provided string safe to show and log: lossy UTF-8, control
/// characters replaced by spaces, whitespace trimmed, length capped.
pub fn sanitize_message(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(s.len().min(MAX_MESSAGE_LEN));
    for c in s.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > MAX_MESSAGE_LEN {
            break;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

fn non_negative_u32(v: &Value<'_>) -> Option<u32> {
    let i = v.as_int()?;
    if i < 0 {
        return None;
    }
    Some(u32::try_from(i).unwrap_or(u32::MAX))
}

/// Whether a peer address is worth connecting to at all.
pub(crate) fn is_usable_peer(addr: &SocketAddr) -> bool {
    if addr.port() == 0 {
        return false;
    }
    match addr.ip().to_canonical() {
        IpAddr::V4(ip) => !(ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast()),
        IpAddr::V6(ip) => !(ip.is_unspecified() || ip.is_multicast()),
    }
}

fn push_peer(peers: &mut Vec<SocketAddr>, addr: SocketAddr) {
    if peers.len() < MAX_PEERS_PER_RESPONSE && is_usable_peer(&addr) {
        let addr = SocketAddr::new(addr.ip().to_canonical(), addr.port());
        peers.push(addr);
    }
}

fn parse_compact_v4(buf: &[u8], peers: &mut Vec<SocketAddr>) {
    let (chunks, _) = buf.as_chunks::<6>();
    for chunk in chunks {
        let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
        let port = u16::from_be_bytes([chunk[4], chunk[5]]);
        push_peer(peers, SocketAddr::new(ip.into(), port));
    }
}

fn parse_compact_v6(buf: &[u8], peers: &mut Vec<SocketAddr>) {
    let (chunks, _) = buf.as_chunks::<18>();
    for chunk in chunks {
        let mut ip = [0u8; 16];
        ip.copy_from_slice(&chunk[..16]);
        let port = u16::from_be_bytes([chunk[16], chunk[17]]);
        push_peer(peers, SocketAddr::new(Ipv6Addr::from(ip).into(), port));
    }
}

/// Dictionary-model peers (`[{ip, port, peer id}]`). Host names are ignored:
/// we never resolve names handed to us by a tracker.
fn parse_dict_peers(list: &[Value<'_>], peers: &mut Vec<SocketAddr>) {
    for item in list {
        let ip = item
            .get(b"ip")
            .and_then(|v| v.as_bytes())
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|s| s.trim().parse::<IpAddr>().ok());
        let port = item
            .get(b"port")
            .and_then(|v| v.as_int())
            .and_then(|p| u16::try_from(p).ok());
        if let (Some(ip), Some(port)) = (ip, port) {
            push_peer(peers, SocketAddr::new(ip, port));
        }
        if peers.len() >= MAX_PEERS_PER_RESPONSE {
            break;
        }
    }
}

fn parse_peers_value(v: &Value<'_>, v6: bool, peers: &mut Vec<SocketAddr>) {
    match v {
        Value::Bytes(b) if v6 => parse_compact_v6(b, peers),
        Value::Bytes(b) => parse_compact_v4(b, peers),
        Value::List(l) => parse_dict_peers(l, peers),
        _ => {}
    }
}

fn parse_failure(dict: &Value<'_>) -> Option<TrackerError> {
    let reason = dict.get(b"failure reason")?;
    let msg = reason
        .as_bytes()
        .map(sanitize_message)
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "tracker returned a failure".to_owned());
    let mut err = TrackerError::tracker_failure(msg);
    // BEP 31: "retry in" is minutes, or the string "never".
    match dict.get(b"retry in") {
        Some(Value::Int(min)) if *min > 0 => {
            let secs = u64::try_from(*min).unwrap_or(0).saturating_mul(60);
            err.retry_in = Some(Duration::from_secs(secs.min(7 * 24 * 3600)));
        }
        Some(Value::Bytes(b"never")) => err.never_retry = true,
        _ => {}
    }
    Some(err)
}

fn decode_dict(body: &[u8]) -> Result<Value<'_>, TrackerError> {
    let v = bdecode::decode(body, bdecode::Limits::default())
        .map_err(|e| TrackerError::other(format!("invalid tracker response: {e}")))?;
    if v.as_dict().is_none() {
        return Err(TrackerError::other(
            "invalid tracker response: not a dictionary",
        ));
    }
    Ok(v)
}

/// Parse a bencoded HTTP announce response.
pub fn parse_announce_response(body: &[u8]) -> Result<AnnounceResponse, TrackerError> {
    let dict = decode_dict(body)?;
    if let Some(err) = parse_failure(&dict) {
        return Err(err);
    }
    let mut resp = AnnounceResponse {
        interval: dict.get(b"interval").and_then(non_negative_u32),
        min_interval: dict.get(b"min interval").and_then(non_negative_u32),
        tracker_id: dict
            .get(b"tracker id")
            .and_then(|v| v.as_bytes())
            .filter(|b| !b.is_empty() && b.len() <= MAX_TRACKER_ID_LEN)
            .map(|b| b.to_vec()),
        complete: dict.get(b"complete").and_then(non_negative_u32),
        incomplete: dict.get(b"incomplete").and_then(non_negative_u32),
        downloaded: dict.get(b"downloaded").and_then(non_negative_u32),
        warning_message: dict
            .get(b"warning message")
            .and_then(|v| v.as_bytes())
            .map(sanitize_message)
            .filter(|m| !m.is_empty()),
        peers: Vec::new(),
        external_ip: dict
            .get(b"external ip")
            .and_then(|v| v.as_bytes())
            .and_then(|b| match b.len() {
                4 => Some(IpAddr::from(<[u8; 4]>::try_from(b).ok()?)),
                16 => Some(IpAddr::from(<[u8; 16]>::try_from(b).ok()?)),
                _ => None,
            }),
    };
    if let Some(p) = dict.get(b"peers") {
        parse_peers_value(p, false, &mut resp.peers);
    }
    if let Some(p) = dict.get(b"peers6") {
        parse_peers_value(p, true, &mut resp.peers);
    }
    Ok(resp)
}

/// Parse a bencoded HTTP scrape response and return the counters for `info_hash`.
pub fn parse_scrape_response(body: &[u8], info_hash: &Id20) -> Result<ScrapeData, TrackerError> {
    let dict = decode_dict(body)?;
    if let Some(err) = parse_failure(&dict) {
        return Err(err);
    }
    let files = dict
        .get(b"files")
        .and_then(|f| f.as_dict())
        .ok_or_else(|| TrackerError::other("invalid scrape response: no files"))?;
    let entry = files
        .iter()
        .find(|(k, _)| *k == &info_hash.0[..])
        .map(|(_, v)| v)
        .ok_or_else(|| TrackerError::other("torrent not found in scrape response"))?;
    Ok(ScrapeData {
        complete: entry.get(b"complete").and_then(non_negative_u32),
        incomplete: entry.get(b"incomplete").and_then(non_negative_u32),
        downloaded: entry.get(b"downloaded").and_then(non_negative_u32),
    })
}

/// Longest I2P destination or name accepted from a tracker.
const MAX_I2P_PEER_LEN: usize = 1024;

fn base32_lower(data: &[u8]) -> String {
    const ALPHA: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(data.len() * 8 / 5 + 1);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(ALPHA[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHA[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn i2p_peer_from_ip(ip: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(ip).ok()?;
    if s.len() > MAX_I2P_PEER_LEN || !s.ends_with(".i2p") {
        return None;
    }
    let ok = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'~' | b'=' | b'.');
    if !s.bytes().all(ok) {
        return None;
    }
    let bare = &s[..s.len() - 4];
    // `<base64 destination>.i2p` (destinations are hundreds of characters)
    // or a host name such as `<b32>.b32.i2p`.
    if bare.len() >= 387 && !bare.contains('.') {
        Some(bare.to_owned())
    } else {
        Some(s.to_owned())
    }
}

/// I2P peers of an announce response from an `.i2p` tracker: compact
/// `peers` (32-byte destination hashes, returned as `<b32>.b32.i2p` names) or
/// a list of dictionaries whose `ip` is `<base64 destination>.i2p` (returned
/// as the destination) or a name. At most [`MAX_PEERS_PER_RESPONSE`];
/// anything else is ignored.
pub fn parse_i2p_peers(body: &[u8]) -> Vec<String> {
    let Ok(dict) = decode_dict(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match dict.get(b"peers") {
        Some(Value::Bytes(b)) => {
            for chunk in b.as_chunks::<32>().0.iter().take(MAX_PEERS_PER_RESPONSE) {
                out.push(format!("{}.b32.i2p", base32_lower(chunk)));
            }
        }
        Some(Value::List(l)) => {
            for p in l.iter() {
                if out.len() >= MAX_PEERS_PER_RESPONSE {
                    break;
                }
                if let Some(peer) = p
                    .get(b"ip")
                    .and_then(|v| v.as_bytes())
                    .and_then(i2p_peer_from_ip)
                {
                    out.push(peer);
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TrackerErrorKind;

    #[test]
    fn compact_and_ipv6_peers() {
        let data = b"d8:intervali1800e5:peers6:iiiipp6:peers618:iiiiiiiiiiiiiiiippe";
        let r = parse_announce_response(data).unwrap();
        assert_eq!(r.interval, Some(1800));
        assert_eq!(
            r.peers,
            vec![
                "105.105.105.105:28784".parse().unwrap(),
                "[6969:6969:6969:6969:6969:6969:6969:6969]:28784"
                    .parse()
                    .unwrap()
            ]
        );
    }

    #[test]
    fn dict_peers() {
        let buf = b"d8:intervali60e5:peersld2:ip9:127.0.0.14:porti100eed2:ip39:6969:6969:6969:6969:6969:6969:6969:69694:porti101eed2:ip11:example.com4:porti5eed2:ip7:1.2.3.44:porti0eeee";
        let r = parse_announce_response(buf).unwrap();
        assert_eq!(
            r.peers,
            vec![
                "127.0.0.1:100".parse().unwrap(),
                "[6969:6969:6969:6969:6969:6969:6969:6969]:101"
                    .parse()
                    .unwrap()
            ]
        );
    }

    #[test]
    fn all_fields() {
        let buf = b"d8:completei12e10:downloadedi99e10:incompletei3e8:intervali900e12:min intervali60e10:tracker id3:abc15:warning message9:be nice\x07!5:peers0:11:external ip4:\x01\x02\x03\x04e";
        let r = parse_announce_response(buf).unwrap();
        assert_eq!(r.complete, Some(12));
        assert_eq!(r.incomplete, Some(3));
        assert_eq!(r.downloaded, Some(99));
        assert_eq!(r.interval, Some(900));
        assert_eq!(r.min_interval, Some(60));
        assert_eq!(r.tracker_id.as_deref(), Some(&b"abc"[..]));
        assert_eq!(r.warning_message.as_deref(), Some("be nice !"));
        assert_eq!(r.external_ip, Some("1.2.3.4".parse().unwrap()));
        assert!(r.peers.is_empty());
    }

    #[test]
    fn missing_and_negative_fields() {
        let r = parse_announce_response(b"d8:completei-1e5:peers0:e").unwrap();
        assert_eq!(r.interval, None);
        assert_eq!(r.complete, None);
        let r = parse_announce_response(b"d8:intervali99999999999ee").unwrap();
        assert_eq!(r.interval, Some(u32::MAX));
    }

    #[test]
    fn failure_reason() {
        let e = parse_announce_response(b"d14:failure reason17:torrent not founde").unwrap_err();
        assert_eq!(e.kind, TrackerErrorKind::TrackerFailure);
        assert_eq!(e.message, "torrent not found");
        assert!(!e.never_retry);

        let e = parse_announce_response(b"d14:failure reason3:bad8:retry in5:nevere").unwrap_err();
        assert!(e.never_retry);
        let e = parse_announce_response(b"d14:failure reason3:bad8:retry ini10ee").unwrap_err();
        assert_eq!(e.retry_in, Some(Duration::from_secs(600)));
    }

    #[test]
    fn not_a_dict() {
        let e = parse_announce_response(b"<html>oops</html>").unwrap_err();
        assert_eq!(e.kind, TrackerErrorKind::Other);
        assert!(parse_announce_response(b"li1ee").is_err());
    }

    #[test]
    fn truncated_compact_peers_are_ignored() {
        let r = parse_announce_response(b"d5:peers8:aaaaaabbe").unwrap();
        assert_eq!(r.peers.len(), 1);
        let r = parse_announce_response(b"d6:peers620:aaaaaaaaaaaaaaaaaabbe").unwrap();
        assert_eq!(r.peers.len(), 1);
    }

    #[test]
    fn peer_cap() {
        let mut body = b"d5:peers".to_vec();
        let n = MAX_PEERS_PER_RESPONSE + 500;
        body.extend_from_slice(format!("{}:", n * 6).as_bytes());
        for i in 0..n {
            body.extend_from_slice(&[10, 0, (i >> 8) as u8, i as u8, 0x1a, 0xe1]);
        }
        body.push(b'e');
        let r = parse_announce_response(&body).unwrap();
        assert_eq!(r.peers.len(), MAX_PEERS_PER_RESPONSE);
    }

    #[test]
    fn message_sanitization() {
        let long = vec![b'a'; 10_000];
        assert_eq!(sanitize_message(&long).len(), MAX_MESSAGE_LEN);
        assert_eq!(sanitize_message(b"a\nb\x1b[31mc"), "a b [31mc");
        assert_eq!(sanitize_message(b"\xff\xfeok"), "\u{fffd}\u{fffd}ok");
    }

    #[test]
    fn scrape() {
        let hash = Id20::new(*b"aaaaaaaaaaaaaaaaaaaa");
        let body =
            b"d5:filesd20:aaaaaaaaaaaaaaaaaaaad8:completei5e10:downloadedi50e10:incompletei10eeee";
        let s = parse_scrape_response(body, &hash).unwrap();
        assert_eq!(
            s,
            ScrapeData {
                complete: Some(5),
                incomplete: Some(10),
                downloaded: Some(50)
            }
        );
        let other = Id20::new(*b"bbbbbbbbbbbbbbbbbbbb");
        assert!(parse_scrape_response(body, &other).is_err());
        assert!(parse_scrape_response(b"d5:filesi1ee", &hash).is_err());
        let e = parse_scrape_response(b"d14:failure reason4:nopee", &hash).unwrap_err();
        assert_eq!(e.kind, TrackerErrorKind::TrackerFailure);
    }

    #[test]
    fn fuzz_parsers_never_panic() {
        let hash = Id20::new(*b"aaaaaaaaaaaaaaaaaaaa");
        let seeds: [&[u8]; 4] = [
            b"d8:completei12e10:downloadedi99e10:incompletei3e8:intervali900e12:min intervali60e10:tracker id3:abc5:peers6:iiiipp6:peers618:iiiiiiiiiiiiiiiippe",
            b"d14:failure reason3:bad8:retry ini10ee",
            b"d5:filesd20:aaaaaaaaaaaaaaaaaaaad8:completei5e10:downloadedi50e10:incompletei10eeee",
            b"d5:peersld2:ip9:127.0.0.14:porti100eed2:ip3:::14:porti1eeee",
        ];
        let mut state: u64 = 0x2545f4914f6cdd1d;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for seed in seeds {
            for cut in 0..=seed.len() {
                let _ = parse_announce_response(&seed[..cut]);
                let _ = parse_scrape_response(&seed[..cut], &hash);
            }
            for _ in 0..5_000 {
                let mut buf = seed.to_vec();
                for _ in 0..(next() % 6 + 1) {
                    let r = next();
                    if buf.is_empty() {
                        break;
                    }
                    let pos = (r as usize) % buf.len();
                    match r % 3 {
                        0 => buf[pos] = (r >> 8) as u8,
                        1 => buf.insert(pos, b"deil:0123456789-"[(r >> 16) as usize % 16]),
                        _ => {
                            buf.remove(pos);
                        }
                    }
                }
                if let Ok(r) = parse_announce_response(&buf) {
                    assert!(r.peers.len() <= MAX_PEERS_PER_RESPONSE);
                    assert!(r.peers.iter().all(|p| p.port() != 0));
                }
                let _ = parse_scrape_response(&buf, &hash);
            }
        }
    }

    #[test]
    fn i2p_peers() {
        let dest = "A".repeat(516) + "AAAA";
        let body = format!(
            "d8:intervali1800e5:peersld2:ip{}:{dest}.i2p4:porti6881eed2:ip9:x.b32.i2pe\
             d2:ip7:1.2.3.4ed2:ip12:bad\nname.i2peee",
            dest.len() + 4
        );
        let peers = parse_i2p_peers(body.as_bytes());
        assert_eq!(peers, vec![dest, "x.b32.i2p".to_owned()]);

        let mut compact = b"d5:peers64:".to_vec();
        compact.extend_from_slice(&[0u8; 32]);
        compact.extend_from_slice(&[0xffu8; 32]);
        compact.extend_from_slice(b"e");
        let peers = parse_i2p_peers(&compact);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0], format!("{}.b32.i2p", "a".repeat(52)));
        assert!(peers[1].ends_with(".b32.i2p") && peers[1].len() == 52 + 8);
        assert!(parse_i2p_peers(b"garbage").is_empty());
        assert!(parse_i2p_peers(b"d5:peersi3ee").is_empty());
    }
}
