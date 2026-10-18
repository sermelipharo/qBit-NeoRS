//! Session-level pause (`SessionImpl::pause/resume`, `BitTorrent/Session/StartPaused`):
//! a paused session contacts no tracker and no peer, keeps every torrent's
//! own started/stopped state, and contacts them after resume.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use qbt_core::model::{AddTorrentParams, TorrentSource, TrackerTierEntry};
use qbt_e2e::*;

/// A listener that only counts incoming connections.
struct Probe {
    addr: String,
    hits: Arc<AtomicUsize>,
}

fn probe() -> Probe {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        let end = Instant::now() + Duration::from_secs(120);
        while Instant::now() < end {
            match l.accept() {
                Ok((s, _)) => {
                    h.fetch_add(1, Ordering::SeqCst);
                    held.push(s);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
    Probe { addr, hits }
}

fn wait_hits(p: &Probe, what: &str, timeout: Duration) {
    let start = Instant::now();
    while p.hits.load(Ordering::SeqCst) == 0 {
        assert!(start.elapsed() < timeout, "no contact with the {what} after resume");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn leecher_torrent(node: &Node, tracker: &Probe, peer: &Probe) -> String {
    // The content only exists in a scratch directory, so the torrent is incomplete.
    let scratch = tempfile::tempdir().unwrap();
    let content = make_content(scratch.path(), "Paused Sample", &[("x.bin", 600_000)]);
    let torrent = make_torrent(&content, 64 * 1024);
    let app = &node.app;
    let hash = app.runtime().block_on(app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default())).unwrap();
    app.add_trackers(hash.clone(), vec![TrackerTierEntry { url: format!("http://{}/announce", tracker.addr), tier: 0 }]).unwrap();
    app.add_peers(vec![hash.clone()], vec![peer.addr.clone()]).unwrap();
    hash
}

#[test]
fn started_paused_makes_no_contact_until_resumed() {
    let (tracker, peer) = (probe(), probe());
    let node = Node::start(free_port(), |p| p.advanced.start_paused = true);
    assert!(node.app.is_session_paused());
    let hash = leecher_torrent(&node, &tracker, &peer);

    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(tracker.hits.load(Ordering::SeqCst), 0, "tracker contacted while paused");
    assert_eq!(peer.hits.load(Ordering::SeqCst), 0, "peer contacted while paused");
    let t = node.torrent(&hash).unwrap();
    assert!(!t.state.is_stopped(), "session pause must not stop the torrent: {:?}", t.state);
    assert_eq!((t.dl_speed, t.up_speed), (0, 0));

    node.app.runtime().block_on(node.app.resume_session()).unwrap();
    assert!(!node.app.is_session_paused());
    wait_hits(&tracker, "tracker", Duration::from_secs(30));
    wait_hits(&peer, "peer", Duration::from_secs(30));
}

#[test]
fn runtime_pause_and_resume_keep_torrent_state() {
    let (tracker, peer) = (probe(), probe());
    let node = Node::start(free_port(), |_| {});
    assert!(!node.app.is_session_paused());
    node.app.runtime().block_on(node.app.pause_session()).unwrap();
    assert!(node.app.is_session_paused());
    let hash = leecher_torrent(&node, &tracker, &peer);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(tracker.hits.load(Ordering::SeqCst) + peer.hits.load(Ordering::SeqCst), 0);

    // An explicitly stopped torrent stays stopped across pause and resume.
    node.app.runtime().block_on(node.app.stop_torrents(vec![hash.clone()])).unwrap();
    node.app.runtime().block_on(node.app.resume_session()).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert!(node.torrent(&hash).unwrap().state.is_stopped());
    assert_eq!(tracker.hits.load(Ordering::SeqCst) + peer.hits.load(Ordering::SeqCst), 0);

    node.app.runtime().block_on(node.app.start_torrents(vec![hash])).unwrap();
    wait_hits(&tracker, "tracker", Duration::from_secs(30));
    wait_hits(&peer, "peer", Duration::from_secs(30));
}
