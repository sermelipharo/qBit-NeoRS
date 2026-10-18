use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;

const DEST: &str = "AAAA-~AA";

/// Minimal SAM bridge: every connection says HELLO, then handles one or more
/// commands; STREAM CONNECT echoes, STREAM ACCEPT announces a peer.
async fn fake_sam() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut r = BufReader::new(s);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let reply = if line.starts_with("HELLO") {
                        "HELLO REPLY RESULT=OK VERSION=3.1\n".to_string()
                    } else if line.starts_with("SESSION CREATE") {
                        assert!(line.contains("inbound.quantity=2"));
                        "SESSION STATUS RESULT=OK DESTINATION=PRIVKEY\n".to_string()
                    } else if line.starts_with("NAMING LOOKUP NAME=ME") {
                        format!("NAMING REPLY RESULT=OK NAME=ME VALUE={DEST}\n")
                    } else if line.starts_with("NAMING LOOKUP") {
                        "NAMING REPLY RESULT=KEY_NOT_FOUND NAME=x MESSAGE=\"not found\"\n".to_string()
                    } else if line.starts_with("STREAM CONNECT") {
                        r.get_mut().write_all(b"STREAM STATUS RESULT=OK\n").await.unwrap();
                        let mut buf = [0u8; 5];
                        r.read_exact(&mut buf).await.unwrap();
                        r.get_mut().write_all(&buf).await.unwrap();
                        return;
                    } else if line.starts_with("STREAM ACCEPT") {
                        r.get_mut().write_all(b"STREAM STATUS RESULT=OK\n").await.unwrap();
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        r.get_mut().write_all(b"PEERDEST FROM_PORT=0 TO_PORT=0\n").await.unwrap();
                        return;
                    } else {
                        "ERROR\n".to_string()
                    };
                    r.get_mut().write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    port
}

#[test]
fn parses_replies_and_addresses() {
    let (w, p) = parse_reply("NAMING REPLY RESULT=KEY_NOT_FOUND NAME=foo.i2p MESSAGE=\"no such key\"");
    assert_eq!(w, vec!["NAMING", "REPLY"]);
    assert_eq!(get(&p, "RESULT"), Some("KEY_NOT_FOUND"));
    assert_eq!(get(&p, "MESSAGE"), Some("no such key"));
    assert_eq!(base32_lower(b"foobar"), "mzxw6ytboi");
    assert!(b32_address("AAAA-~AA").unwrap().ends_with(".b32.i2p"));
    assert!(b32_address("!!").is_none());
}

#[tokio::test]
async fn session_connect_accept_lookup() {
    let port = fake_sam().await;
    let opts = SamOptions { port, inbound_quantity: 2, ..Default::default() };
    let s = Session::create(opts, Duration::from_secs(5)).await.unwrap();
    assert_eq!(s.destination, DEST);
    assert_eq!(s.id().len(), 12);

    let mut stream = s.connect("peer.b32.i2p", Duration::from_secs(5)).await.unwrap();
    stream.write_all(b"hello").await.unwrap();
    let mut back = [0u8; 5];
    stream.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"hello");

    let (_stream, peer) = s.accept().await.unwrap();
    assert_eq!(peer, "PEERDEST");

    let err = s.lookup("nothere.i2p").await.unwrap_err();
    assert!(matches!(err, I2pError::Sam { ref result, .. } if result == "KEY_NOT_FOUND"), "{err}");
    assert!(s.connect("bad dest", Duration::from_secs(1)).await.is_err());
}
