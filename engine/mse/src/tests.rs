use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

use super::*;

const HASH: [u8; 20] = *b"0123456789abcdefghij";

fn lookup(req2: &[u8; 20]) -> Option<[u8; 20]> {
    (req2_hash(&HASH) == *req2).then_some(HASH)
}

#[tokio::test]
async fn encrypted_round_trip() {
    for _ in 0..20 {
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            match accept(b, EncryptionPolicy::Forced, lookup).await.unwrap() {
                Accepted::Encrypted { mut stream, info_hash } => {
                    assert_eq!(info_hash, HASH);
                    assert!(stream.is_encrypted());
                    let mut buf = [0u8; 68];
                    stream.read_exact(&mut buf).await.unwrap();
                    assert_eq!(&buf[..20], BT_PROTOCOL_PREFIX);
                    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
                    stream.write_all(&big).await.unwrap();
                    stream.flush().await.unwrap();
                }
                Accepted::Plaintext(_) => panic!("expected encrypted"),
            }
        });
        let mut s = initiate(a, &HASH).await.unwrap();
        let mut hs = BT_PROTOCOL_PREFIX.to_vec();
        hs.extend_from_slice(&[0u8; 48]);
        s.write_all(&hs).await.unwrap();
        s.flush().await.unwrap();
        let mut got = vec![0u8; 300_000];
        s.read_exact(&mut got).await.unwrap();
        assert!(got.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn plaintext_detected_and_replayed() {
    let (mut a, b) = duplex(4096);
    let server = tokio::spawn(async move {
        match accept(b, EncryptionPolicy::Enabled, lookup).await.unwrap() {
            Accepted::Plaintext(mut s) => {
                let mut buf = [0u8; 25];
                s.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf[..20], BT_PROTOCOL_PREFIX);
                assert_eq!(&buf[20..], b"hello");
                assert!(!s.is_encrypted());
            }
            Accepted::Encrypted { .. } => panic!("expected plaintext"),
        }
    });
    a.write_all(BT_PROTOCOL_PREFIX).await.unwrap();
    a.write_all(b"hello").await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn policies_are_enforced() {
    let (mut a, b) = duplex(4096);
    let server = tokio::spawn(async move { accept(b, EncryptionPolicy::Forced, lookup).await });
    a.write_all(BT_PROTOCOL_PREFIX).await.unwrap();
    assert!(matches!(server.await.unwrap(), Err(MseError::PlaintextRefused)));

    let (a, b) = duplex(4096);
    let server = tokio::spawn(async move { accept(b, EncryptionPolicy::Disabled, lookup).await });
    let client = tokio::spawn(async move { initiate(a, &HASH).await });
    assert!(matches!(server.await.unwrap(), Err(MseError::EncryptedRefused)));
    drop(client);
}

#[tokio::test]
async fn unknown_torrent_is_rejected() {
    let (a, b) = duplex(4096);
    let server = tokio::spawn(async move { accept(b, EncryptionPolicy::Enabled, |_| None).await });
    let _client = tokio::spawn(async move { initiate(a, &HASH).await });
    assert!(matches!(server.await.unwrap(), Err(MseError::UnknownTorrent)));
}

#[tokio::test]
async fn garbage_does_not_panic() {
    for len in [20usize, 96, 200, 700, 2000] {
        let (mut a, b) = duplex(8192);
        let server = tokio::spawn(async move { accept(b, EncryptionPolicy::Enabled, lookup).await });
        let junk: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
        a.write_all(&junk).await.unwrap();
        drop(a);
        assert!(server.await.unwrap().is_err());
    }
}

#[test]
fn degenerate_public_keys_rejected() {
    let k = KeyPair::generate();
    assert!(k.shared_secret(&[0u8; 96]).is_err());
    let mut one = [0u8; 96];
    one[95] = 1;
    assert!(k.shared_secret(&one).is_err());
    assert!(k.shared_secret(&[0xffu8; 96]).is_err());
}
