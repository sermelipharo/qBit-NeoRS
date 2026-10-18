use std::net::SocketAddr;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::http::{parse_head, route};
use super::state::{MAX_PEERS_PER_TORRENT, TrackerState};

const HASH: &str = "%01%02%03%04%05%06%07%08%09%0a%0b%0c%0d%0e%0f%10%11%12%13%14";

fn get(state: &Mutex<TrackerState>, from: &str, query: &str) -> Vec<u8> {
    let head = format!("GET /announce?{query} HTTP/1.1\r\nHost: x\r\n\r\n");
    let req = parse_head(head.as_bytes()).unwrap();
    let peer: SocketAddr = from.parse().unwrap();
    let resp = route(&req, peer, state);
    resp.body().to_vec()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn announce_registers_and_returns_compact_peers() {
    let st = Mutex::new(TrackerState::default());
    let a = get(&st, "10.0.0.1:5000", &format!("info_hash={HASH}&peer_id=AAAAAAAAAAAAAAAAAAAA&port=6881&left=0"));
    assert!(contains(&a, b"8:completei1e"), "{}", String::from_utf8_lossy(&a));
    let b = get(&st, "10.0.0.2:5000", &format!("info_hash={HASH}&peer_id=BBBBBBBBBBBBBBBBBBBB&port=6882&left=10"));
    assert!(contains(&b, b"10:incompletei1e"));
    // Both peers in the compact list: 2 * 6 bytes.
    assert!(contains(&b, b"5:peers12:"));
    assert!(contains(&b, &[10, 0, 0, 1, 0x1a, 0xe1]));

    let c = get(&st, "10.0.0.2:5000", &format!("info_hash={HASH}&peer_id=BBBBBBBBBBBBBBBBBBBB&port=6882&event=stopped"));
    assert!(contains(&c, b"5:peers0:"));
    let d = get(&st, "10.0.0.3:5000", &format!("info_hash={HASH}&peer_id=CCCCCCCCCCCCCCCCCCCC&port=7000"));
    assert!(contains(&d, b"8:completei1e"));
    assert!(contains(&d, b"10:incompletei1e"));
}

#[test]
fn rejects_bad_parameters() {
    let st = Mutex::new(TrackerState::default());
    let r = get(&st, "10.0.0.1:1", "peer_id=x&port=1");
    assert!(contains(&r, b"failure reason"));
    assert!(contains(&r, b"Missing \"info_hash\""));
    let r = get(&st, "10.0.0.1:1", "info_hash=short&peer_id=x&port=1");
    assert!(contains(&r, b"Invalid \"info_hash\""));
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=x&port=0"));
    assert!(contains(&r, b"Invalid \"port\""));
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=x&port=99999"));
    assert!(contains(&r, b"Invalid \"port\""));
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=x&port=1&numwant=-1"));
    assert!(contains(&r, b"Invalid \"numwant\""));
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=x&port=1&event=bogus"));
    assert!(contains(&r, b"Invalid \"event\""));
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=123456789012345678901&port=1"));
    assert!(contains(&r, b"Invalid \"peer_id\""));
    assert_eq!(st.lock().torrent_count(), 0);
}

#[test]
fn claimed_ip_only_trusted_from_local_networks() {
    let st = Mutex::new(TrackerState::default());
    // A public client claiming to be 1.2.3.4 is recorded with its real address.
    get(&st, "203.0.113.7:1", &format!("info_hash={HASH}&peer_id=x&port=1000&ip=1.2.3.4"));
    let r = get(&st, "10.0.0.9:1", &format!("info_hash={HASH}&peer_id=y&port=2000&ip=192.168.1.50"));
    assert!(contains(&r, &[203, 0, 113, 7, 0x03, 0xe8]));
    assert!(!contains(&r, &[1, 2, 3, 4]));
    assert!(contains(&r, &[192, 168, 1, 50, 0x07, 0xd0]));
}

#[test]
fn peer_table_is_capped() {
    let st = Mutex::new(TrackerState::default());
    for i in 0..(MAX_PEERS_PER_TORRENT + 50) {
        let from = format!("10.1.{}.{}:1", i / 250, i % 250 + 1);
        get(&st, &from, &format!("info_hash={HASH}&peer_id=p&port=1000"));
    }
    let g = st.lock();
    let t = g.torrent(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]).unwrap();
    assert_eq!(t.peers().len(), MAX_PEERS_PER_TORRENT);
}

#[test]
fn non_compact_reply_and_method_checks() {
    let st = Mutex::new(TrackerState::default());
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=ABC&port=1000&compact=0"));
    assert!(contains(&r, b"7:peer id3:ABC"));
    let r = get(&st, "10.0.0.1:1", &format!("info_hash={HASH}&peer_id=ABC&port=1000&compact=0&no_peer_id=1"));
    assert!(!contains(&r, b"peer id"));

    let req = parse_head(b"POST /announce HTTP/1.1\r\n\r\n").unwrap();
    assert!(String::from_utf8_lossy(&route(&req, "10.0.0.1:1".parse().unwrap(), &st).into_bytes()).starts_with("HTTP/1.1 405"));
    let req = parse_head(b"GET /scrape HTTP/1.1\r\n\r\n").unwrap();
    assert!(String::from_utf8_lossy(&route(&req, "10.0.0.1:1".parse().unwrap(), &st).into_bytes()).starts_with("HTTP/1.1 404"));
    assert!(parse_head(b"garbage\r\n\r\n").is_err());
}

#[tokio::test]
async fn serves_over_tcp_and_rejects_huge_heads() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(Mutex::new(TrackerState::default()));
    let server = tokio::spawn(super::serve(listener, state.clone()));

    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET /announce?info_hash={HASH}&peer_id=x&port=6881 HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("8:intervali1800e"));
    assert_eq!(state.lock().torrent_count(), 1);

    let mut s = TcpStream::connect(addr).await.unwrap();
    let big = format!("GET /announce?x={} HTTP/1.1\r\n", "a".repeat(20_000));
    let _ = s.write_all(big.as_bytes()).await;
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 413"));
    server.abort();
}
