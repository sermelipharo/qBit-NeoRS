//! BitTorrent v2 (BEP 52) over localhost: pure v2 and hybrid torrents made
//! by the core's creator, transferred between two engine sessions and
//! between two cores.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerOptions, ManagedTorrent, ManagedTorrentState, Session,
    SessionOptions,
};
type ManagedTorrentHandle = Arc<ManagedTorrent>;

use qbt_core::model::{AddTorrentParams, TorrentFormat, TorrentSource};
use qbt_e2e::*;

/// Files covering the v2 corner cases at a 64 KiB piece size: a multi-piece
/// file with a short last piece, a file of exactly one piece, files smaller
/// than a piece (one and several blocks), an empty file and nested folders.
const FILES: &[(&str, usize)] = &[
    ("a/big.bin", 300_000),
    ("a/small.bin", 20_000),
    ("b/c/tiny.txt", 100),
    ("b/empty", 0),
    ("one_piece.bin", 65_536),
    ("two_pieces_and_a_byte.bin", 131_073),
];
const PIECE: u32 = 64 * 1024;

fn session_opts(listen: bool) -> SessionOptions {
    SessionOptions {
        dht: None,
        persistence: None,
        disable_local_service_discovery: true,
        listen: listen.then(|| ListenerOptions {
            listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn wait_finished(h: &ManagedTorrentHandle) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let done = h.with_state(|s| match s {
                ManagedTorrentState::Live(_) => h.stats().finished,
                ManagedTorrentState::Error(e) => panic!("torrent errored: {e:#}"),
                _ => false,
            });
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("timed out waiting for the torrent to finish")
}

async fn add(session: &Arc<Session>, torrent: &[u8], out: &Path, peers: Option<Vec<SocketAddr>>) -> ManagedTorrentHandle {
    session
        .add_torrent(
            AddTorrent::from_bytes(torrent.to_vec()),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(out.to_string_lossy().into_owned()),
                initial_peers: peers,
                ..Default::default()
            }),
        )
        .await
        .expect("add_torrent")
        .into_handle()
        .expect("handle")
}

async fn engine_transfer(format: TorrentFormat) {
    let tmp = tempfile::tempdir().unwrap();
    let content = make_content(tmp.path(), "V2 Collection", FILES);
    let torrent = make_torrent_format(&content, PIECE, format);

    let seeder = Session::new_with_opts(tmp.path().join("s"), session_opts(true)).await.unwrap();
    let sh = add(&seeder, &torrent, &content, None).await;
    // The initial check verified every piece against the merkle trees.
    wait_finished(&sh).await;
    let addr = seeder.listen_addr().unwrap();

    let out = tmp.path().join("leech");
    let leecher = Session::new_with_opts(tmp.path().join("l"), session_opts(false)).await.unwrap();
    let lh = add(&leecher, &torrent, &out, Some(vec![addr])).await;
    assert_eq!(lh.info_hash(), sh.info_hash());
    wait_finished(&lh).await;
    assert!(same_tree(&content, &out), "downloaded tree differs");

    // A corrupted copy fails the check: only the damaged piece is missing.
    let corrupt = tmp.path().join("corrupt");
    std::fs::create_dir_all(&corrupt).unwrap();
    copy_tree(&content, &corrupt);
    let big = corrupt.join("a/big.bin");
    let mut data = std::fs::read(&big).unwrap();
    data[200_000] ^= 0xff;
    std::fs::write(&big, data).unwrap();
    let checker = Session::new_with_opts(tmp.path().join("c"), session_opts(false)).await.unwrap();
    let ch = add(&checker, &torrent, &corrupt, None).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let live = ch.with_state(|s| matches!(s, ManagedTorrentState::Live(_)));
            if live {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let stats = ch.stats();
    let total = stats.total_bytes;
    assert!(!stats.finished);
    assert_eq!(total - stats.progress_bytes, u64::from(PIECE), "exactly one piece should be missing");

    seeder.stop().await;
    leecher.stop().await;
    checker.stop().await;
}

fn copy_tree(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let dst = to.join(e.file_name());
        if e.path().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_sessions_transfer_pure_v2_multi_file() {
    engine_transfer(TorrentFormat::V2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_sessions_transfer_hybrid_multi_file() {
    engine_transfer(TorrentFormat::Hybrid).await;
}

/// A `btmh` magnet: the metadata is checked with SHA-256 and the piece
/// layers come from the seeder with BEP 52 hash requests (a file of more
/// than 512 pieces needs several requests with merkle proofs).
#[tokio::test(flavor = "multi_thread")]
async fn engine_sessions_transfer_pure_v2_by_magnet() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = FILES.to_vec();
    files.push(("large.bin", 611 * 16 * 1024 + 5));
    let content = make_content(tmp.path(), "V2 Magnet", &files);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::V2);
    let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
    let v2 = parsed.info_hash_v2.unwrap().as_string();

    let seeder = Session::new_with_opts(tmp.path().join("s"), session_opts(true)).await.unwrap();
    let sh = add(&seeder, &torrent, &content, None).await;
    wait_finished(&sh).await;
    let addr = seeder.listen_addr().unwrap();

    let out = tmp.path().join("leech");
    let leecher = Session::new_with_opts(tmp.path().join("l"), session_opts(false)).await.unwrap();
    let magnet = format!("magnet:?xt=urn:btmh:1220{v2}&dn=V2%20Magnet");
    let lh = leecher
        .add_torrent(
            AddTorrent::from_url(magnet.as_str()),
            Some(AddTorrentOptions {
                output_folder: Some(out.to_string_lossy().into_owned()),
                initial_peers: Some(vec![addr]),
                ..Default::default()
            }),
        )
        .await
        .expect("add magnet")
        .into_handle()
        .unwrap();
    assert_eq!(lh.info_hash(), sh.info_hash());
    wait_finished(&lh).await;
    assert!(same_tree(&content, &out), "downloaded tree differs");
    assert_eq!(leecher.fetch_budget_used(), 0, "metadata and layer memory returned");
    // The metadata the leecher built carries the piece layers: it parses
    // like the original .torrent.
    let bytes = lh.with_metadata(|m| m.torrent_bytes.clone()).unwrap();
    let again = librqbit::torrent_from_bytes(&bytes).unwrap();
    let layers = again.piece_layers.as_ref().map(|l| l.0.as_ref());
    assert!(layers.is_some());
    again.info.data.clone().validate().unwrap().with_piece_layers(layers).unwrap();

    seeder.stop().await;
    leecher.stop().await;
}

/// The entries (root, hashes) of a `piece layers` dictionary.
fn layer_entries(raw: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    fn bytes(raw: &[u8], at: &mut usize) -> Vec<u8> {
        let colon = *at + raw[*at..].iter().position(|&c| c == b':').unwrap();
        let len: usize = std::str::from_utf8(&raw[*at..colon]).unwrap().parse().unwrap();
        *at = colon + 1 + len;
        raw[colon + 1..*at].to_vec()
    }
    assert_eq!(raw[0], b'd');
    let mut at = 1;
    let mut out = Vec::new();
    while raw[at] != b'e' {
        let k = bytes(raw, &mut at);
        let v = bytes(raw, &mut at);
        out.push((k, v));
    }
    out
}

/// The .torrent with only the piece layers whose root `keep` accepts.
fn strip_piece_layers(torrent: &[u8], keep: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let parsed = librqbit::torrent_from_bytes(torrent).unwrap();
    let raw = parsed.piece_layers.as_ref().unwrap().0.as_ref();
    let mut layers = b"d".to_vec();
    for (k, v) in layer_entries(raw).into_iter().filter(|(k, _)| keep(k)) {
        layers.extend_from_slice(format!("{}:", k.len()).as_bytes());
        layers.extend(k);
        layers.extend_from_slice(format!("{}:", v.len()).as_bytes());
        layers.extend(v);
    }
    layers.push(b'e');
    librqbit::torrent_with_piece_layers(torrent, &layers).unwrap()
}

/// A pure v2 .torrent without its piece layers (or with only some of
/// them): like libtorrent, the engine starts it at once and fetches the
/// missing layers from the seeder with BEP 52 hash requests, checks them
/// against each file's `pieces root` and downloads each file once its
/// layer is known; the .torrent it hands out has every layer.
#[tokio::test(flavor = "multi_thread")]
async fn engine_fetches_missing_piece_layers_of_a_pure_v2_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = FILES.to_vec();
    files.push(("large.bin", 611 * 16 * 1024 + 5));
    let content = make_content(tmp.path(), "V2 No Layers", &files);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::V2);
    let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
    let full_layers = parsed.piece_layers.as_ref().unwrap().0.as_ref().to_vec();
    let entries = layer_entries(&full_layers);
    assert!(entries.len() >= 2);
    let one_root = entries[0].0.clone();

    let seeder = Session::new_with_opts(tmp.path().join("s"), session_opts(true)).await.unwrap();
    let sh = add(&seeder, &torrent, &content, None).await;
    wait_finished(&sh).await;
    let addr = seeder.listen_addr().unwrap();

    for (label, stripped) in [
        ("none", strip_piece_layers(&torrent, |_| false)),
        ("some", strip_piece_layers(&torrent, |k| k == one_root.as_slice())),
    ] {
        let info = librqbit::torrent_from_bytes(&stripped).unwrap();
        let l = info.piece_layers.as_ref().map(|l| l.0.as_ref());
        assert!(info.info.data.clone().validate().unwrap().with_piece_layers(l).is_err(), "{label}");

        let out = tmp.path().join(format!("leech-{label}"));
        let leecher = Session::new_with_opts(tmp.path().join(format!("l-{label}")), session_opts(false)).await.unwrap();
        let lh = add(&leecher, &stripped, &out, Some(vec![addr])).await;
        assert_eq!(lh.info_hash(), sh.info_hash());
        wait_finished(&lh).await;
        assert!(same_tree(&content, &out), "downloaded tree differs ({label})");
        assert_eq!(lh.missing_piece_layers(), 0);
        let layers = lh.take_fetched_piece_layers().expect("fetched layers handed out");
        assert_eq!(&layers[..], &full_layers[..], "{label}");
        assert!(lh.take_fetched_piece_layers().is_none(), "once");
        let bytes = lh.torrent_bytes().unwrap();
        let again = librqbit::torrent_from_bytes(&bytes).unwrap();
        assert_eq!(again.piece_layers.as_ref().unwrap().0.as_ref(), &full_layers[..], "{label}");
        assert_eq!(again.info_hash, parsed.info_hash);
        assert_eq!(leecher.fetch_budget_used(), 0, "layer fetch memory returned");
        leecher.stop().await;
    }
    seeder.stop().await;
}

async fn wait_until(what: &str, secs: u64, f: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(secs), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Like libtorrent, a pure v2 .torrent lacking the layer of one file
/// starts at once: the other files download (and verify) while that one
/// waits. Here the only peer is a seeder with the same incomplete .torrent
/// and a damaged copy of that file (so it can't find the layer by hashing
/// the file either): it can't send the layer and has none of the file's
/// pieces verified, so the file stays at 0 while the others complete. Once
/// a seeder with the layer shows up, the layer is fetched and the file
/// completes; the first seeder, connected to it too, verifies its copy
/// from disk and downloads only the damaged piece.
#[tokio::test(flavor = "multi_thread")]
async fn other_files_complete_before_a_missing_layer_arrives() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = FILES.to_vec();
    files.push(("large.bin", 611 * 16 * 1024 + 5));
    let content = make_content(tmp.path(), "V2 One Layer Missing", &files);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::V2);
    let info = librqbit::torrent_from_bytes(&torrent).unwrap().info.data.clone().validate().unwrap();
    let layout = info.v2_layout().unwrap();
    // The file whose layer is left out: `large.bin` (several hash requests).
    let span = *layout
        .spans
        .iter()
        .find(|s| &*layout.entries[s.entry as usize].path[0] == b"large.bin")
        .unwrap();
    let missing_file = span.entry as usize;
    let stripped = strip_piece_layers(&torrent, |k| k != span.pieces_root.as_slice());
    let file_lens: Vec<(usize, u64, bool)> =
        layout.entries.iter().enumerate().map(|(i, e)| (i, e.len, e.padding)).collect();

    // Seeder A: the incomplete .torrent, and one piece of that file damaged.
    let a_data = tmp.path().join("a-data");
    std::fs::create_dir_all(&a_data).unwrap();
    copy_tree(&content, &a_data);
    let damaged = a_data.join("large.bin");
    let mut data = std::fs::read(&damaged).unwrap();
    data[3 * 16 * 1024 + 100] ^= 0xff;
    std::fs::write(&damaged, data).unwrap();
    let a = Session::new_with_opts(tmp.path().join("a"), session_opts(true)).await.unwrap();
    let ah = add(&a, &stripped, &a_data, None).await;
    wait_until("A to be live", 30, || ah.with_state(|s| matches!(s, ManagedTorrentState::Live(_)))).await;
    assert_eq!(ah.missing_piece_layers(), 1);
    assert!(!ah.stats().finished, "the file without a layer isn't verified");

    let out = tmp.path().join("leech");
    let l = Session::new_with_opts(tmp.path().join("l"), session_opts(false)).await.unwrap();
    let lh = add(&l, &stripped, &out, Some(vec![a.listen_addr().unwrap()])).await;
    let others_done = || {
        let p = lh.stats().file_progress;
        p.len() == file_lens.len()
            && file_lens.iter().all(|&(i, len, pad)| pad || i == missing_file || p[i] == len)
    };
    wait_until("the other files", 60, others_done).await;
    // Nothing of the file without a layer, and it stays that way.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let st = lh.stats();
    assert_eq!(st.file_progress[missing_file], 0);
    assert!(!st.finished);
    assert_eq!(lh.missing_piece_layers(), 1);
    assert!(lh.piece_states().iter().skip(span.first_piece as usize).take(span.num_pieces as usize).all(|&s| s == 0));

    // Seeder B has the layer: L fetches it, then the file.
    let b = Session::new_with_opts(tmp.path().join("b"), session_opts(true)).await.unwrap();
    let bh = add(&b, &torrent, &content, None).await;
    wait_finished(&bh).await;
    let b_addr = b.listen_addr().unwrap();
    lh.add_peers(vec![b_addr]);
    wait_finished(&lh).await;
    assert!(same_tree(&content, &out), "downloaded tree differs");
    assert_eq!(lh.missing_piece_layers(), 0);
    assert_eq!(l.fetch_budget_used(), 0);

    // A gets the layer from B, finds the file on disk but the damaged piece,
    // and downloads only that one.
    ah.add_peers(vec![b_addr]);
    wait_finished(&ah).await;
    assert!(same_tree(&content, &a_data), "A repaired its copy");
    let fetched = ah.stats().live.map(|l| l.snapshot.fetched_bytes).unwrap_or_default();
    assert_eq!(fetched, 16 * 1024, "A verified its copy instead of downloading it");

    a.stop().await;
    b.stop().await;
    l.stop().await;
}

/// Resume and recheck with a layer still missing: pieces of a file without
/// its layer never count as had, whatever the fast-resume bitfield says;
/// the file's data stays on disk and is verified once the layer is known
/// (while running, or while paused: then on resume).
#[tokio::test(flavor = "multi_thread")]
async fn missing_layers_survive_resume_and_recheck() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = FILES.to_vec();
    files.push(("large.bin", 40 * 16 * 1024 + 7));
    let content = make_content(tmp.path(), "V2 Resume", &files);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::V2);
    let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
    let full_layers = parsed.piece_layers.as_ref().unwrap().0.as_ref().to_vec();
    let info = parsed.info.data.clone().validate().unwrap();
    let layout = info.v2_layout().unwrap();
    let span = *layout
        .spans
        .iter()
        .find(|s| &*layout.entries[s.entry as usize].path[0] == b"large.bin")
        .unwrap();
    let stripped = strip_piece_layers(&torrent, |k| k != span.pieces_root.as_slice());
    let opts = || SessionOptions {
        persistence: Some(librqbit::SessionPersistenceConfig::FastResumeOnly { folder: tmp.path().join("resume") }),
        fastresume: true,
        ..session_opts(false)
    };
    let live = |h: &ManagedTorrentHandle| h.with_state(|st| matches!(st, ManagedTorrentState::Live(_)));

    // First run, complete data: like libtorrent's check, the file without
    // a layer hashes up to its root, which gives the layer; everything is
    // verified at once and the layer is handed out to be saved.
    let s = Session::new_with_opts(tmp.path().join("s"), opts()).await.unwrap();
    let h = add(&s, &stripped, &content, None).await;
    wait_finished(&h).await;
    assert_eq!(h.missing_piece_layers(), 0);
    assert_eq!(&h.take_fetched_piece_layers().expect("layer found on disk")[..], &full_layers[..]);
    let total = h.stats().total_bytes;
    s.pause(&h).await.unwrap();
    s.stop().await;
    drop(h);
    drop(s);

    // One piece of the file damaged on disk: the layer can't be found that
    // way any more.
    let damaged = content.join("large.bin");
    let mut data = std::fs::read(&damaged).unwrap();
    data[5 * 16 * 1024 + 9] ^= 0xff;
    std::fs::write(&damaged, &data).unwrap();
    let file_bytes = u64::from(span.num_pieces) * 16 * 1024;

    // Second run with the incomplete .torrent again: the fast-resume
    // bitfield says every piece is there, but that file can't be verified.
    let s = Session::new_with_opts(tmp.path().join("s"), opts()).await.unwrap();
    let h = add(&s, &stripped, &content, None).await;
    wait_until("live after restart", 30, || live(&h)).await;
    assert_eq!(h.stats().progress_bytes, total - file_bytes);
    assert_eq!(h.missing_piece_layers(), 1);
    // A recheck: the same.
    h.force_recheck().await.unwrap();
    wait_until("live after recheck", 30, || live(&h)).await;
    assert_eq!(h.stats().progress_bytes, total - file_bytes);
    // The layer arrives while paused: the file's data on disk is checked
    // on resume; only the damaged piece is missing (no peers to get it).
    s.pause(&h).await.unwrap();
    assert_eq!(h.add_piece_layers(&full_layers).unwrap(), 1);
    assert_eq!(h.missing_piece_layers(), 0);
    s.unpause(&h).await.unwrap();
    wait_until("the file checked", 30, || live(&h) && h.stats().progress_bytes == total - 16 * 1024).await;
    assert!(!h.stats().finished);
    assert_eq!(h.piece_states()[(span.first_piece + 5) as usize], 0);
    assert_eq!(h.add_piece_layers(&full_layers).unwrap(), 0, "nothing missing any more");
    s.stop().await;
}

/// A layer that doesn't hash up to its file's root is refused; nothing of
/// it is taken.
#[tokio::test(flavor = "multi_thread")]
async fn a_bad_piece_layer_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = FILES.to_vec();
    files.push(("large.bin", 40 * 16 * 1024 + 7));
    let content = make_content(tmp.path(), "V2 Bad Layer", &files);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::V2);
    let full_layers = librqbit::torrent_from_bytes(&torrent).unwrap().piece_layers.unwrap().0.as_ref().to_vec();
    let stripped = strip_piece_layers(&torrent, |_| false);
    let s = Session::new_with_opts(tmp.path().join("s"), session_opts(false)).await.unwrap();
    // No data: nothing to find the layers from.
    let h = add(&s, &stripped, &tmp.path().join("empty"), None).await;
    let live = || h.with_state(|st| matches!(st, ManagedTorrentState::Live(_)));
    wait_until("live", 30, live).await;
    let missing = h.missing_piece_layers();
    assert!(missing >= 2);
    let mut bad = full_layers.clone();
    let at = bad.len() - 2;
    bad[at] ^= 1;
    assert!(h.add_piece_layers(&bad).is_err());
    assert_eq!(h.missing_piece_layers(), missing);
    s.stop().await;
}

/// The core takes a pure v2 .torrent without piece layers and runs it at
/// once; the engine fetches the layers and the core saves the .torrent with
/// them (export and restart keep them). Adding a complete copy of the
/// .torrent to a torrent still missing layers completes it too.
#[test]
fn cores_fetch_missing_piece_layers() {
    let seeder = Node::start(free_port(), |_| {});
    let content = make_content(&seeder.downloads(), "V2 Core Layers", FILES);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::V2);
    let full = librqbit::torrent_from_bytes(&torrent).unwrap().piece_layers.unwrap().0.as_ref().to_vec();
    let stripped = strip_piece_layers(&torrent, |_| false);
    let hash = seeder
        .app
        .runtime()
        .block_on(seeder.app.add_torrent(
            TorrentSource::Bytes { data: torrent.clone() },
            AddTorrentParams { save_path: Some(seeder.downloads().to_string_lossy().into()), ..Default::default() },
        ))
        .expect("seeder add");
    seeder.wait_for(&hash, Duration::from_secs(30), "seeder to finish checking", is_seeding);

    let leecher = Node::start(free_port(), |_| {});
    let h2 = leecher
        .app
        .runtime()
        .block_on(leecher.app.add_torrent(TorrentSource::Bytes { data: stripped }, AddTorrentParams::default()))
        .expect("a .torrent without piece layers is accepted");
    assert_eq!(h2, hash);
    // Files are known right away (the layers are still missing).
    assert!(!leecher.app.files(hash.clone(), Vec::new()).unwrap().is_empty());
    leecher.app.add_peers(vec![hash.clone()], vec![format!("127.0.0.1:{}", seeder.port)]).expect("add_peers");
    leecher.wait_for(&hash, Duration::from_secs(60), "download to complete", is_seeding);
    assert!(same_tree(&content, &leecher.downloads().join("V2 Core Layers")));
    let exported_layers = |node: &Node| {
        let exported = node.app.export_torrent(hash.clone()).expect("export");
        librqbit::torrent_from_bytes(&exported).unwrap().piece_layers.map(|l| l.0.as_ref().to_vec())
    };
    // Saved on a maintenance tick.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while exported_layers(&leecher).as_deref() != Some(&full[..]) {
        assert!(std::time::Instant::now() < deadline, "the saved .torrent got its layers");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Another core with the incomplete .torrent and no peers, then the
    // complete one added on top: it takes the layers.
    let other = Node::start(free_port(), |_| {});
    let h3 = other
        .app
        .runtime()
        .block_on(other.app.add_torrent(TorrentSource::Bytes { data: strip_piece_layers(&torrent, |_| false) }, AddTorrentParams::default()))
        .expect("add incomplete");
    assert_eq!(h3, hash);
    assert!(
        other
            .app
            .runtime()
            .block_on(other.app.add_torrent(TorrentSource::Bytes { data: torrent.clone() }, AddTorrentParams::default()))
            .is_err(),
        "a duplicate"
    );
    assert_eq!(exported_layers(&other).as_deref(), Some(&full[..]));
}

#[test]
fn cores_seed_and_download_pure_v2() {
    let seeder = Node::start(free_port(), |_| {});
    let leecher = Node::start(free_port(), |_| {});
    let content = make_content(&seeder.downloads(), "V2 Core", FILES);
    let torrent = make_torrent_format(&content, PIECE, TorrentFormat::V2);

    let hash = seeder
        .app
        .runtime()
        .block_on(seeder.app.add_torrent(
            TorrentSource::Bytes { data: torrent.clone() },
            AddTorrentParams { save_path: Some(seeder.downloads().to_string_lossy().into()), ..Default::default() },
        ))
        .expect("seeder add");
    let s = seeder.wait_for(&hash, Duration::from_secs(30), "seeder to finish checking", is_seeding);
    // A pure v2 torrent has no v1 hash; its id is the truncated v2 hash.
    assert!(s.infohash_v1.is_empty());
    assert_eq!(s.infohash_v2.len(), 64);
    assert_eq!(&s.infohash_v2[..40], hash);
    assert!(s.magnet_uri.contains(&format!("xt=urn:btmh:1220{}", s.infohash_v2)), "{}", s.magnet_uri);
    assert!(!s.magnet_uri.contains("btih"), "{}", s.magnet_uri);

    let hash2 = leecher
        .app
        .runtime()
        .block_on(leecher.app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default()))
        .expect("leecher add");
    assert_eq!(hash, hash2);
    leecher.app.add_peers(vec![hash.clone()], vec![format!("127.0.0.1:{}", seeder.port)]).expect("add_peers");
    leecher.wait_for(&hash, Duration::from_secs(60), "download to complete", is_seeding);
    assert!(same_tree(&content, &leecher.downloads().join("V2 Core")));

    // The exported .torrent keeps the piece layers (re-adding it works).
    let exported = leecher.app.export_torrent(hash.clone()).expect("export");
    let other = Node::start(free_port(), |_| {});
    let h3 = other
        .app
        .runtime()
        .block_on(other.app.add_torrent(TorrentSource::Bytes { data: exported }, AddTorrentParams::default()))
        .expect("re-add exported torrent");
    assert_eq!(h3, hash);

    // The seeder's v2-only magnet link: metadata and piece layers come
    // from the seeder, then the content.
    let by_magnet = Node::start(free_port(), |_| {});
    let h4 = by_magnet
        .app
        .runtime()
        .block_on(by_magnet.app.add_torrent(
            TorrentSource::Url { url: format!("{}&x.pe=127.0.0.1:{}", s.magnet_uri, seeder.port) },
            AddTorrentParams::default(),
        ))
        .expect("add magnet");
    assert_eq!(h4, hash);
    let t = by_magnet.torrent(&hash).unwrap();
    assert!(t.infohash_v1.is_empty());
    assert_eq!(t.infohash_v2, s.infohash_v2);
    by_magnet.wait_for(&hash, Duration::from_secs(60), "magnet download to complete", is_seeding);
    assert!(same_tree(&content, &by_magnet.downloads().join("V2 Core")));
}

/// A hybrid torrent by its `btmh` magnet: the seeder accepts the handshake
/// with its truncated v2 hash, the metadata is checked with SHA-256, and the
/// torrent then runs on its v1 hash with the v2 hash as its other identity.
#[tokio::test(flavor = "multi_thread")]
async fn engine_sessions_transfer_hybrid_by_btmh_magnet() {
    let tmp = tempfile::tempdir().unwrap();
    let content = make_content(tmp.path(), "Hybrid Magnet", FILES);
    let torrent = make_torrent_format(&content, PIECE, TorrentFormat::Hybrid);
    let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
    let v2 = parsed.info_hash_v2.unwrap();

    let seeder = Session::new_with_opts(tmp.path().join("s"), session_opts(true)).await.unwrap();
    let sh = add(&seeder, &torrent, &content, None).await;
    wait_finished(&sh).await;
    assert_eq!(sh.info_hash(), parsed.info_hash);
    assert_eq!(sh.info_hash_alt(), Some(v2.truncate_for_dht()));

    let out = tmp.path().join("leech");
    let leecher = Session::new_with_opts(tmp.path().join("l"), session_opts(false)).await.unwrap();
    let magnet = format!("magnet:?xt=urn:btmh:1220{}", v2.as_string());
    let lh = leecher
        .add_torrent(
            AddTorrent::from_url(magnet.as_str()),
            Some(AddTorrentOptions {
                output_folder: Some(out.to_string_lossy().into_owned()),
                initial_peers: Some(vec![seeder.listen_addr().unwrap()]),
                ..Default::default()
            }),
        )
        .await
        .expect("add magnet")
        .into_handle()
        .unwrap();
    assert_eq!(lh.info_hash(), sh.info_hash());
    assert_eq!(lh.info_hash_alt(), sh.info_hash_alt());
    wait_finished(&lh).await;
    assert!(same_tree(&content, &out), "downloaded tree differs");
    seeder.stop().await;
    leecher.stop().await;
}

/// A hybrid torrent added by its `btih` magnet has no piece layers: it
/// connects to the seeder with the v1 hash and the v2 bit, the seeder
/// upgrades the connection to v2, and the leecher fetches the layers over
/// it (verified against the pieces roots). It then answers hash requests
/// itself, and hands the layers out once for saving.
#[tokio::test(flavor = "multi_thread")]
async fn engine_hybrid_by_btih_magnet_fetches_and_serves_piece_layers() {
    use librqbit::v2::{parse_file_tree, proof_layers_to_root, verify_layer_hashes};
    let tmp = tempfile::tempdir().unwrap();
    let mut files = FILES.to_vec();
    files.push(("large.bin", 611 * 16 * 1024 + 5));
    let content = make_content(tmp.path(), "Hybrid Layers", &files);
    let torrent = make_torrent_format(&content, 16 * 1024, TorrentFormat::Hybrid);
    let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
    let full = parsed.piece_layers.as_ref().unwrap().0.as_ref().to_vec();
    let alt = parsed.info_hash_v2.unwrap().truncate_for_dht().0;

    let seeder = Session::new_with_opts(tmp.path().join("s"), session_opts(true)).await.unwrap();
    let sh = add(&seeder, &torrent, &content, None).await;
    wait_finished(&sh).await;

    let out = tmp.path().join("leech");
    let leecher = Session::new_with_opts(tmp.path().join("l"), session_opts(true)).await.unwrap();
    let magnet = format!("magnet:?xt=urn:btih:{}", parsed.info_hash.as_string());
    let lh = leecher
        .add_torrent(
            AddTorrent::from_url(magnet.as_str()),
            Some(AddTorrentOptions {
                output_folder: Some(out.to_string_lossy().into_owned()),
                initial_peers: Some(vec![seeder.listen_addr().unwrap()]),
                ..Default::default()
            }),
        )
        .await
        .expect("add magnet")
        .into_handle()
        .unwrap();
    let bytes = lh.with_metadata(|m| m.torrent_bytes.clone()).unwrap();
    assert!(librqbit::torrent_from_bytes(&bytes).unwrap().piece_layers.is_none());
    wait_finished(&lh).await;
    assert!(same_tree(&content, &out), "downloaded tree differs");

    let layers = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(l) = lh.take_fetched_piece_layers() {
                return l;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the leecher fetched the piece layers");
    assert_eq!(&layers[..], &full[..]);
    assert!(lh.take_fetched_piece_layers().is_none(), "handed out once");
    assert_eq!(leecher.fetch_budget_used(), 0, "layer fetch memory returned");

    // The leecher now answers hash requests (here over an upgraded v1
    // connection, as libtorrent makes them).
    let tree = parsed.info.data.file_tree.as_ref().unwrap().0.as_ref();
    let layout = parse_file_tree(tree, 16 * 1024).unwrap();
    let span = layout.spans.iter().max_by_key(|s| s.num_pieces).unwrap();
    let height = librqbit::v2::layer_tree_height(span.num_pieces);
    let (mut peer, _, hash) = RawPeer::connect(leecher.listen_addr().unwrap(), parsed.info_hash.0, true).await;
    assert_eq!(hash, alt);
    let proofs = proof_layers_to_root(512, height);
    peer.send(21, &hash_request(&span.pieces_root, 0, 512, 512, proofs)).await;
    let (id, body) = peer.wait_for(&[22, 23]).await;
    assert_eq!(id, 22, "hash request rejected");
    let hashes: &[[u8; 32]] = body[48..].as_chunks::<32>().0;
    let (b, uncles) = hashes.split_at(512);
    assert!(verify_layer_hashes(&span.pieces_root, 512, b, uncles));

    seeder.stop().await;
    leecher.stop().await;
}

/// (info hash, peer) a test tracker hands out.
type ServedPeer = ([u8; 20], SocketAddr);

/// A minimal HTTP tracker on 127.0.0.1: records the info hashes it is
/// announced under and answers with the peers registered for that hash in
/// `serve` (nothing else).
struct TestTracker {
    url: String,
    announced: Arc<std::sync::Mutex<Vec<[u8; 20]>>>,
    serve: Arc<std::sync::Mutex<Vec<ServedPeer>>>,
}

fn pct_decode_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

async fn start_tracker() -> TestTracker {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let url = format!("http://{}/announce", listener.local_addr().unwrap());
    let t = TestTracker { url, announced: Default::default(), serve: Default::default() };
    let (log, serve) = (t.announced.clone(), t.serve.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let (log, serve) = (log.clone(), serve.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut n = 0;
                while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") && n < buf.len() {
                    match s.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(k) => n += k,
                    }
                }
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let target = req.split_whitespace().nth(1).unwrap_or("");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let hash = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("info_hash="))
                    .and_then(|v| <[u8; 20]>::try_from(pct_decode_bytes(v)).ok());
                let mut peers = Vec::new();
                if let (Some(h), "/announce") = (hash, path) {
                    log.lock().unwrap().push(h);
                    for (_, a) in serve.lock().unwrap().iter().filter(|(x, _)| *x == h) {
                        if let SocketAddr::V4(a) = a {
                            peers.extend_from_slice(&a.ip().octets());
                            peers.extend_from_slice(&a.port().to_be_bytes());
                        }
                    }
                }
                let mut body = format!("d8:intervali5e12:min intervali1e5:peers{}:", peers.len()).into_bytes();
                body.extend_from_slice(&peers);
                body.push(b'e');
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
            });
        }
    });
    t
}

/// A hybrid torrent is announced under both hashes, and a peer found only
/// under the v2 hash is connected to with it (v2 handshake bit set); the
/// seeder accepts that handshake.
#[tokio::test(flavor = "multi_thread")]
async fn engine_hybrid_swarm_under_both_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let content = make_content(tmp.path(), "Hybrid Tracker", FILES);
    let torrent = make_torrent_format(&content, PIECE, TorrentFormat::Hybrid);
    let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
    let v1 = parsed.info_hash.0;
    let alt = parsed.info_hash_v2.unwrap().truncate_for_dht().0;
    let tracker = start_tracker().await;

    let with_tracker = |out: &Path| AddTorrentOptions {
        overwrite: true,
        output_folder: Some(out.to_string_lossy().into_owned()),
        replace_trackers: Some(vec![(tracker.url.clone(), 0)]),
        trackers_local_source: Some(true),
        ..Default::default()
    };
    let seeder = Session::new_with_opts(tmp.path().join("s"), session_opts(true)).await.unwrap();
    // The seeder is handed out under the v2 hash only.
    tracker.serve.lock().unwrap().push((alt, seeder.listen_addr().unwrap()));
    let sh = seeder
        .add_torrent(AddTorrent::from_bytes(torrent.clone()), Some(with_tracker(&content)))
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&sh).await;

    let out = tmp.path().join("leech");
    let leecher = Session::new_with_opts(tmp.path().join("l"), session_opts(false)).await.unwrap();
    let lh = leecher
        .add_torrent(AddTorrent::from_bytes(torrent.clone()), Some(with_tracker(&out)))
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&lh).await;
    assert!(same_tree(&content, &out), "downloaded tree differs");
    let seen = tracker.announced.lock().unwrap().clone();
    // Both sessions announced under both hashes.
    assert!(seen.iter().filter(|h| **h == v1).count() >= 2, "{seen:?}");
    assert!(seen.iter().filter(|h| **h == alt).count() >= 2, "{seen:?}");
    seeder.stop().await;
    leecher.stop().await;
}

/// A hand-written peer speaking the wire protocol to a seeder.
struct RawPeer {
    s: tokio::net::TcpStream,
}

impl RawPeer {
    /// Connects with `info_hash` (v2 bit set if `v2`) and returns the
    /// seeder's handshake reserved bytes and info hash.
    async fn connect(addr: SocketAddr, info_hash: [u8; 20], v2: bool) -> (Self, [u8; 8], [u8; 20]) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut hs = vec![19u8];
        hs.extend_from_slice(b"BitTorrent protocol");
        let mut reserved = [0u8; 8];
        if v2 {
            reserved[7] |= 0x10;
        }
        hs.extend_from_slice(&reserved);
        hs.extend_from_slice(&info_hash);
        hs.extend_from_slice(b"-XX0000-raw-peer-tst");
        s.write_all(&hs).await.unwrap();
        let mut reply = [0u8; 68];
        s.read_exact(&mut reply).await.unwrap();
        (Self { s }, reply[20..28].try_into().unwrap(), reply[28..48].try_into().unwrap())
    }

    async fn send(&mut self, id: u8, payload: &[u8]) {
        use tokio::io::AsyncWriteExt;
        let mut m = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        m.push(id);
        m.extend_from_slice(payload);
        self.s.write_all(&m).await.unwrap();
    }

    /// The next message with one of `ids` (others are skipped).
    async fn wait_for(&mut self, ids: &[u8]) -> (u8, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let mut len = [0u8; 4];
                self.s.read_exact(&mut len).await.unwrap();
                let len = u32::from_be_bytes(len) as usize;
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u8; len];
                self.s.read_exact(&mut buf).await.unwrap();
                if ids.contains(&buf[0]) {
                    return (buf[0], buf[1..].to_vec());
                }
            }
        })
        .await
        .expect("timed out waiting for a message")
    }
}

fn hash_request(root: &[u8; 32], base: u32, index: u32, length: u32, proof_layers: u32) -> Vec<u8> {
    let mut r = root.to_vec();
    for v in [base, index, length, proof_layers] {
        r.extend_from_slice(&v.to_be_bytes());
    }
    r
}

/// What libtorrent does on the wire, by hand: connect to a hybrid seeder
/// with its truncated v2 hash (accepted, answered with that hash and the v2
/// bit), ask for piece-layer hashes with libtorrent's proof layer counting
/// (served from the .torrent's piece layers, verifiable against the pieces
/// root), and, on a pure v2 seeder, ask for a block cut at the end of a
/// file's data (served as asked).
#[tokio::test(flavor = "multi_thread")]
async fn engine_answers_v2_peers_like_libtorrent() {
    use librqbit::v2::{parse_file_tree, parse_piece_layers, proof_layers_to_root, proof_uncles, verify_layer_hashes};
    let tmp = tempfile::tempdir().unwrap();
    for format in [TorrentFormat::Hybrid, TorrentFormat::V2] {
        let content = make_content(tmp.path(), &format!("Raw {format:?}"), FILES);
        let torrent = make_torrent_format(&content, PIECE, format);
        let parsed = librqbit::torrent_from_bytes(&torrent).unwrap();
        let alt = parsed.info_hash_v2.unwrap().truncate_for_dht().0;
        let tree = parsed.info.data.file_tree.as_ref().unwrap().0.as_ref();
        let layout = parse_file_tree(tree, PIECE).unwrap();
        let layers = parse_piece_layers(parsed.piece_layers.as_ref().map(|l| l.0.as_ref()), &layout).unwrap();

        let seeder = Session::new_with_opts(tmp.path().join(format!("s{format:?}")), session_opts(true)).await.unwrap();
        let sh = add(&seeder, &torrent, &content, None).await;
        wait_finished(&sh).await;
        let addr = seeder.listen_addr().unwrap();

        let (mut peer, reserved, hash) = RawPeer::connect(addr, alt, true).await;
        assert_eq!(hash, alt, "{format:?}: answered with the hash we used");
        assert_ne!(reserved[7] & 0x10, 0, "{format:?}: v2 bit");

        if format == TorrentFormat::Hybrid {
            // libtorrent's upgrade: the v1 hash with the v2 bit is answered
            // with the v2 hash, and the connection serves hash requests.
            let v1 = parsed.info_hash.0;
            let (mut up, reserved, hash) = RawPeer::connect(addr, v1, true).await;
            assert_eq!(hash, alt, "a v1 connection with the v2 bit is upgraded");
            assert_ne!(reserved[7] & 0x10, 0);
            let span = layout.spans.iter().find(|s| s.len > u64::from(PIECE)).unwrap();
            let height = librqbit::v2::layer_tree_height(span.num_pieces);
            let base = (PIECE / 16384).trailing_zeros();
            up.send(21, &hash_request(&span.pieces_root, base, 0, 1, proof_layers_to_root(1, height))).await;
            assert_eq!(up.wait_for(&[22, 23]).await.0, 22);
            // Without the bit the connection stays v1; our handshake offers
            // the upgrade.
            let (_plain, reserved, hash) = RawPeer::connect(addr, v1, false).await;
            assert_eq!(hash, v1);
            assert_ne!(reserved[7] & 0x10, 0);
        }

        // Every file with a piece layer, 4 hashes at a time, proofs to the root.
        let base = (PIECE / 16384).trailing_zeros();
        for span in layout.spans.iter().filter(|s| s.len > u64::from(PIECE)) {
            let height = librqbit::v2::layer_tree_height(span.num_pieces);
            let len = 4u32.min(1 << height);
            for index in (0..span.num_pieces).step_by(len as usize) {
                let proofs = proof_layers_to_root(len, height);
                peer.send(21, &hash_request(&span.pieces_root, base, index, len, proofs)).await;
                let (id, body) = peer.wait_for(&[22, 23]).await;
                assert_eq!(id, 22, "{format:?}: hash request rejected");
                let hashes: &[[u8; 32]] = body[48..].as_chunks::<32>().0;
                assert_eq!(hashes.len() as u32, len + proof_uncles(len, proofs));
                let (b, uncles) = hashes.split_at(len as usize);
                assert!(verify_layer_hashes(&span.pieces_root, index, b, uncles));
                let first = span.first_piece as usize + index as usize;
                let real = (span.num_pieces - index).min(len) as usize;
                assert_eq!(&b[..real], &layers[first..first + real]);
            }
        }
        // Other layers and malformed requests are rejected.
        let span = layout.spans.iter().find(|s| s.len > u64::from(PIECE)).unwrap();
        for bad in [hash_request(&span.pieces_root, 0, 0, 4, 0), hash_request(&span.pieces_root, base, 1, 2, 0), hash_request(&[9; 32], base, 0, 2, 0)] {
            peer.send(21, &bad).await;
            assert_eq!(peer.wait_for(&[22, 23]).await.0, 23);
        }

        if format == TorrentFormat::V2 {
            // A file whose data ends inside a piece: libtorrent asks for its
            // last block without the padding.
            let span = layout.spans.iter().find(|s| s.len % u64::from(PIECE) > 16384).unwrap();
            let last = span.first_piece + span.num_pieces - 1;
            let data = span.len - u64::from(span.num_pieces - 1) * u64::from(PIECE);
            let begin = (data - 1) / 16384 * 16384;
            let length = (data - begin) as u32;
            assert!(length < 16384);
            peer.send(2, &[]).await; // interested
            peer.wait_for(&[1]).await; // unchoke
            let mut req = last.to_be_bytes().to_vec();
            req.extend_from_slice(&(begin as u32).to_be_bytes());
            req.extend_from_slice(&length.to_be_bytes());
            peer.send(6, &req).await;
            let (_, piece) = peer.wait_for(&[7]).await;
            assert_eq!(&piece[..8], &req[..8]);
            assert_eq!(piece.len() - 8, length as usize);
            let entry = &layout.entries[span.entry as usize];
            let path = entry.path.iter().fold(content.clone(), |p, c| p.join(String::from_utf8_lossy(c).as_ref()));
            let file = std::fs::read(path).unwrap();
            let at = (u64::from(span.num_pieces - 1) * u64::from(PIECE) + begin) as usize;
            assert_eq!(&piece[8..], &file[at..at + length as usize]);
        }
        seeder.stop().await;
    }
}

/// Cores: a hybrid torrent's id is its truncated v2 hash; added by its
/// `btih` magnet it is known under the v1 hash until the metadata arrives,
/// then under the v2-based id.
#[test]
fn cores_hybrid_ids_follow_qbittorrent() {
    let seeder = Node::start(free_port(), |_| {});
    let leecher = Node::start(free_port(), |_| {});
    let content = make_content(&seeder.downloads(), "Hybrid Core", FILES);
    let torrent = make_torrent_format(&content, PIECE, TorrentFormat::Hybrid);
    let hash = seeder
        .app
        .runtime()
        .block_on(seeder.app.add_torrent(
            TorrentSource::Bytes { data: torrent.clone() },
            AddTorrentParams { save_path: Some(seeder.downloads().to_string_lossy().into()), ..Default::default() },
        ))
        .expect("seeder add");
    let s = seeder.wait_for(&hash, Duration::from_secs(30), "seeder to finish checking", is_seeding);
    assert_eq!(s.infohash_v1.len(), 40);
    assert_eq!(&s.infohash_v2[..40], hash);

    let magnet = format!("magnet:?xt=urn:btih:{}&x.pe=127.0.0.1:{}", s.infohash_v1, seeder.port);
    let first = leecher
        .app
        .runtime()
        .block_on(leecher.app.add_torrent(TorrentSource::Url { url: magnet }, AddTorrentParams::default()))
        .expect("add magnet");
    assert_eq!(first, s.infohash_v1);
    let t = leecher.wait_for(&hash, Duration::from_secs(60), "magnet download to complete", is_seeding);
    assert_eq!((t.infohash_v1.as_str(), t.infohash_v2.as_str()), (s.infohash_v1.as_str(), s.infohash_v2.as_str()));
    assert!(leecher.torrent(&s.infohash_v1).is_none());
    assert!(same_tree(&content, &leecher.downloads().join("Hybrid Core")));

    // The piece layers fetched from the seeder (over the upgraded v2
    // connection) are saved into the leecher's .torrent.
    let expected = librqbit::torrent_from_bytes(&torrent).unwrap().piece_layers.unwrap().0.as_ref().to_vec();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let exported = leecher.app.export_torrent(hash.clone()).expect("export");
        if let Some(l) = librqbit::torrent_from_bytes(&exported).unwrap().piece_layers {
            assert_eq!(l.0.as_ref(), &expected[..]);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "piece layers never saved");
        std::thread::sleep(Duration::from_millis(200));
    }
}
