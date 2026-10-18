// End-to-end web seed (BEP 19) downloads from a local HTTP server with Range
// support: a leecher without peers completes from the web seed, re-fetches a
// corrupted piece, and bans a seed that keeps serving bad data.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use bytes::Bytes;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

use crate::{
    AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions, WebSeedState,
    create_torrent,
    spawn_utils::BlockingSpawner,
    tests::test_util::{create_default_random_dir_with_torrents, setup_test_logging, wait_until},
};

const PIECE_LEN: u32 = 32768;

/// Decides whether to corrupt the response for (path, range start).
type CorruptFn = Arc<dyn Fn(&str, u64) -> bool + Send + Sync>;

/// A tiny HTTP/1.1 file server (one request per connection) rooted at `root`.
async fn serve(root: PathBuf, corrupt: CorruptFn) -> anyhow::Result<std::net::SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let root = root.clone();
            let corrupt = corrupt.clone();
            tokio::spawn(async move {
                let _ = handle(stream, root, corrupt).await;
            });
        }
    });
    Ok(addr)
}

async fn handle(
    stream: tokio::net::TcpStream,
    root: PathBuf,
    corrupt: CorruptFn,
) -> anyhow::Result<()> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let path = line
        .split_whitespace()
        .nth(1)
        .context("no path")?
        .to_owned();
    let mut range = None;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).await?;
        let h = h.trim();
        if h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
            let (a, b) = v.split_once('-').context("bad range")?;
            range = Some((a.parse::<u64>()?, b.parse::<u64>()?));
        }
    }
    let mut file = root.clone();
    for c in path.trim_start_matches('/').split('/') {
        file.push(urlencoding::decode(c)?.into_owned());
    }
    let mut stream = r.into_inner();
    let Ok(mut f) = tokio::fs::File::open(&file).await else {
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    };
    let mut data = Vec::new();
    f.read_to_end(&mut data).await?;
    let (start, end) = range.unwrap_or((0, data.len() as u64 - 1));
    let mut body = data[usize::try_from(start)?..=usize::try_from(end)?].to_vec();
    if corrupt(&path, start) {
        for b in body.iter_mut().take(64) {
            *b ^= 0xff;
        }
    }
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nConnection: close\r\n\r\n",
        body.len(),
        data.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn make_torrent() -> (tempfile::TempDir, Bytes) {
    // 3 files of 100 000 bytes, 32 KiB pieces crossing file boundaries.
    let dir = create_default_random_dir_with_torrents(3, 100_000, Some("rqbit_ws"));
    let torrent = create_torrent(
        dir.path(),
        crate::CreateTorrentOptions {
            piece_length: Some(PIECE_LEN),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    (dir, torrent.as_bytes().unwrap())
}

async fn start_leecher(
    torrent: Bytes,
    out: &std::path::Path,
    web_seed: String,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>)> {
    let session = Session::new_with_opts(
        out.to_owned(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .await?;
    let handle = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent),
            Some(AddTorrentOptions {
                output_folder: Some(out.to_str().unwrap().to_owned()),
                web_seeds: Some(vec![web_seed]),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    Ok((session, handle))
}

async fn wait_finished(handle: &ManagedTorrent) -> anyhow::Result<()> {
    wait_until(
        || {
            let s = handle.stats();
            if let Some(e) = s.error {
                bail!("torrent errored: {e}");
            }
            ensure!(s.live.is_some() && s.finished, "not finished");
            Ok(())
        },
        Duration::from_secs(60),
    )
    .await
}

fn assert_same_files(src: &std::path::Path, dst: &std::path::Path) {
    for i in 0..3 {
        let name = format!("{i}.data");
        let a = std::fs::read(src.join(&name)).unwrap();
        let b = std::fs::read(dst.join(&name)).unwrap();
        assert!(a == b, "{name} differs");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_webseed_download_and_refetch() {
    setup_test_logging();
    let (dir, torrent) = make_torrent().await;
    // Corrupt the second piece (inside 0.data) once.
    let hits = Arc::new(AtomicU32::new(0));
    let corrupt: CorruptFn = {
        let hits = hits.clone();
        Arc::new(move |path: &str, start: u64| {
            path.ends_with("/0.data")
                && start == u64::from(PIECE_LEN)
                && hits.fetch_add(1, Ordering::SeqCst) == 0
        })
    };
    let addr = serve(dir.path().parent().unwrap().to_owned(), corrupt)
        .await
        .unwrap();
    let out = tempfile::TempDir::with_prefix("rqbit_ws_client").unwrap();
    let (session, handle) = start_leecher(torrent, out.path(), format!("http://{addr}/"))
        .await
        .unwrap();

    wait_finished(&handle).await.unwrap();
    assert_same_files(dir.path(), out.path());

    let seeds = handle.web_seeds();
    assert_eq!(seeds.len(), 1);
    assert_eq!(seeds[0].hash_failures, 1);
    assert_eq!(seeds[0].downloaded, 300_000);
    assert!(hits.load(Ordering::SeqCst) >= 2, "piece was not re-fetched");
    assert!(handle.wasted_bytes() >= u64::from(PIECE_LEN));
    assert_eq!(handle.web_seed_urls(), vec![format!("http://{addr}/")]);

    // Runtime list editing.
    assert_eq!(handle.add_web_seeds(vec!["http://127.0.0.1:1/".into()]), 1);
    handle
        .edit_web_seed("http://127.0.0.1:1/", "http://127.0.0.1:2/")
        .unwrap();
    assert_eq!(handle.remove_web_seeds(&["http://127.0.0.1:2/".into()]), 1);
    assert_eq!(handle.web_seeds().len(), 1);
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_webseed_banned_after_hash_failures() {
    setup_test_logging();
    let (dir, torrent) = make_torrent().await;
    let corrupt: CorruptFn =
        Arc::new(|path: &str, start: u64| path.ends_with("/1.data") && start == 0);
    let addr = serve(dir.path().parent().unwrap().to_owned(), corrupt)
        .await
        .unwrap();
    let out = tempfile::TempDir::with_prefix("rqbit_ws_client").unwrap();
    let (session, handle) = start_leecher(torrent, out.path(), format!("http://{addr}/"))
        .await
        .unwrap();

    wait_until(
        || {
            let s = &handle.web_seeds()[0];
            ensure!(s.state == WebSeedState::Banned, "{s:?}");
            Ok(())
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    let s = &handle.web_seeds()[0];
    assert_eq!(s.hash_failures, 3);
    assert!(!handle.stats().finished);
    session.stop().await;
}
