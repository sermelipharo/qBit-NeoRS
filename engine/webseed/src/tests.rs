use super::*;

fn multi() -> Layout {
    Layout {
        name: "My Album".into(),
        piece_length: 10,
        single_file: false,
        files: vec![
            LayoutFile { path: vec!["01 intro.mp3".into()], length: 7, is_pad: false },
            LayoutFile { path: vec![".pad".into(), "3".into()], length: 3, is_pad: true },
            LayoutFile { path: vec!["cd 2".into(), "02.mp3".into()], length: 25, is_pad: false },
        ],
    }
}

#[test]
fn urls_follow_bep19() {
    let l = multi();
    assert_eq!(file_url("http://h/seed", &l, 0).unwrap(), "http://h/seed/My%20Album/01%20intro.mp3");
    assert_eq!(file_url("http://h/seed/", &l, 2).unwrap(), "http://h/seed/My%20Album/cd%202/02.mp3");
    let single = Layout {
        name: "a b.iso".into(),
        piece_length: 4,
        single_file: true,
        files: vec![LayoutFile { path: vec![], length: 10, is_pad: false }],
    };
    assert_eq!(file_url("http://h/dir/", &single, 0).unwrap(), "http://h/dir/a%20b.iso");
    assert_eq!(file_url("http://h/file.iso", &single, 0).unwrap(), "http://h/file.iso");
    assert!(file_url("ftp://h/", &single, 0).is_err());
    // Dot segments from the metadata can't climb out of the seed directory.
    let dots = Layout {
        name: "..".into(),
        piece_length: 4,
        single_file: false,
        files: vec![LayoutFile { path: vec!["..".into(), ".".into(), "x".into()], length: 4, is_pad: false }],
    };
    assert!(file_url("http://h/seed/", &dots, 0).is_err());
    let dots = Layout { name: "ok".into(), files: vec![LayoutFile { path: vec!["a".into(), "..".into()], length: 4, is_pad: false }], ..dots };
    assert!(file_url("http://h/seed/", &dots, 0).is_err());
    assert!(file_url("file:///etc/passwd", &single, 0).is_err());
}

#[test]
fn ranges_span_files_and_skip_pads() {
    let l = multi();
    assert_eq!(l.piece_count(), 4);
    let r = requests_for_piece("http://h/", &l, 0).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].start, r[0].end, r[0].piece_offset), (0, 6, 0));
    let r = requests_for_piece("http://h/", &l, 1).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].start, r[0].end, r[0].piece_offset), (0, 9, 0));
    let r = requests_for_piece("http://h/", &l, 3).unwrap();
    assert_eq!((r[0].start, r[0].end), (20, 24));
    assert_eq!(l.piece_size(3), 5);
    assert!(requests_for_piece("http://h/", &l, 4).is_err());
}

#[tokio::test]
async fn fetches_pieces_with_ranges() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let content: Vec<u8> = (0..25u8).collect();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = content.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let served = served.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let range = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes=").or_else(|| l.strip_prefix("range: bytes=")))
                    .unwrap();
                let (a, b) = range.split_once('-').unwrap();
                let (a, b): (usize, usize) = (a.trim().parse().unwrap(), b.trim().parse().unwrap());
                let body = &served[a..=b];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                s.write_all(head.as_bytes()).await.unwrap();
                s.write_all(body).await.unwrap();
            });
        }
    });
    let l = Layout {
        name: "f".into(),
        piece_length: 10,
        single_file: true,
        files: vec![LayoutFile { path: vec![], length: 25, is_pad: false }],
    };
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/f.bin");
    let p1 = fetch_piece(&client, &base, &l, 1).await.unwrap();
    assert_eq!(p1, content[10..20]);
    let p2 = fetch_piece(&client, &base, &l, 2).await.unwrap();
    assert_eq!(p2, content[20..25]);
}

/// A server ignoring `Range` sends the whole file: only our part is kept.
#[tokio::test]
async fn full_replies_keep_only_the_range() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let content: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = content.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let served = served.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = s.read(&mut buf).await.unwrap();
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len());
                s.write_all(head.as_bytes()).await.unwrap();
                let _ = s.write_all(&served).await;
            });
        }
    });
    let l = Layout {
        name: "f".into(),
        piece_length: 16_384,
        single_file: true,
        files: vec![LayoutFile { path: vec![], length: 100_000, is_pad: false }],
    };
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/f.bin");
    let p = fetch_piece(&client, &base, &l, 3).await.unwrap();
    assert_eq!(p, content[3 * 16_384..4 * 16_384]);
    let last = fetch_piece(&client, &base, &l, 6).await.unwrap();
    assert_eq!(last, content[6 * 16_384..]);
}
