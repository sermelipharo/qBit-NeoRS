//! Acceptance: a seeder that sat idle (no peers, its once-a-second engine
//! tick parked) reports transfer speeds as soon as a peer starts downloading,
//! for the torrent and for the session.

use std::time::{Duration, Instant};

use qbt_core::model::{AddTorrentParams, TorrentSource};
use qbt_e2e::*;

const UPLOAD_BPS: i64 = 1_000_000;

#[test]
fn speeds_are_reported_after_an_idle_period() {
    let fast = |p: &mut qbt_core::prefs::Preferences| p.advanced.refresh_interval = 250;
    let seeder = Node::start(free_port(), fast);
    let leecher = Node::start(free_port(), fast);

    let content = make_content(&seeder.downloads(), "Idle", &[("idle.bin", 8 * 1024 * 1024)]);
    let torrent = make_torrent(&content, 256 * 1024);
    let hash = seeder
        .app
        .runtime()
        .block_on(seeder.app.add_torrent(
            TorrentSource::Bytes { data: torrent.clone() },
            AddTorrentParams { save_path: Some(seeder.downloads().to_string_lossy().into()), ..Default::default() },
        ))
        .expect("seeder add");
    seeder.wait_for(&hash, Duration::from_secs(30), "seeder to finish checking", is_seeding);
    // Paced, so the transfer lasts several seconds.
    seeder.app.set_upload_limit(vec![hash.clone()], UPLOAD_BPS).unwrap();

    // Long enough for the speed samplers and the session tick to park.
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(seeder.torrent(&hash).unwrap().up_speed, 0);

    leecher
        .app
        .runtime()
        .block_on(leecher.app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default()))
        .expect("leecher add");
    leecher.app.add_peers(vec![hash.clone()], vec![format!("127.0.0.1:{}", seeder.port)]).unwrap();

    let start = Instant::now();
    let (mut torrent_up, mut session_up, mut torrent_down, mut session_down) = (None, None, None, None);
    loop {
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(60), "download did not finish");
        let first = |slot: &mut Option<Duration>, speed: i64| {
            if speed > 0 && slot.is_none() {
                *slot = Some(elapsed);
            }
        };
        first(&mut torrent_up, seeder.torrent(&hash).unwrap().up_speed);
        first(&mut session_up, seeder.app.server_state().up_speed);
        let t = leecher.torrent(&hash).unwrap();
        first(&mut torrent_down, t.dl_speed);
        first(&mut session_down, leecher.app.server_state().dl_speed);
        if is_seeding(&t) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!(
        "first speed > 0 after: seeder torrent {torrent_up:?}, session {session_up:?}; \
         leecher torrent {torrent_down:?}, session {session_down:?}; done after {:?}",
        start.elapsed()
    );
    for (what, at) in [
        ("seeder torrent upload speed", torrent_up),
        ("seeder session upload speed", session_up),
        ("leecher torrent download speed", torrent_down),
        ("leecher session download speed", session_down),
    ] {
        let at = at.unwrap_or_else(|| panic!("{what} never above zero"));
        assert!(at < Duration::from_secs(5), "{what} first above zero after {at:?}");
    }
}
