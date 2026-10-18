//! Acceptance: speed limits changed while a download runs (alternative speed
//! toggle, per-torrent limit, global limit) take effect at once, even though
//! the peers already have many block requests in flight.

use std::time::{Duration, Instant};

use qbt_core::model::{AddTorrentParams, TorrentSource};
use qbt_e2e::*;

const SEEDERS: usize = 3;
const SEEDER_BPS: i64 = 400_000;

/// Byte counter samples taken every 50 ms.
struct Samples(Vec<(Instant, i64)>);

impl Samples {
    fn record(read: impl Fn() -> i64, duration: Duration) -> Samples {
        let start = Instant::now();
        let mut v = vec![(start, read())];
        while start.elapsed() < duration {
            std::thread::sleep(Duration::from_millis(50));
            v.push((Instant::now(), read()));
        }
        Samples(v)
    }

    /// Highest rate over windows of at least `window` starting `from` or later.
    fn peak(&self, from: Duration, window: Duration) -> f64 {
        let t0 = self.0[0].0;
        let mut peak = 0f64;
        for (i, &(ti, bi)) in self.0.iter().enumerate() {
            if ti - t0 < from {
                continue;
            }
            let Some(&(tj, bj)) = self.0[i..].iter().find(|(tj, _)| *tj - ti >= window) else {
                break;
            };
            peak = peak.max((bj - bi) as f64 / (tj - ti).as_secs_f64());
        }
        peak
    }

    fn average(&self, from: Duration) -> f64 {
        let t0 = self.0[0].0;
        let &(ts, bs) = self.0.iter().find(|(t, _)| *t - t0 >= from).unwrap();
        let &(te, be) = self.0.last().unwrap();
        (be - bs) as f64 / (te - ts).as_secs_f64()
    }
}

/// From 1 s after the change on, every 2 s window is within 1.25x the limit
/// (the core's counters tick every 250 ms, too coarse for 1 s windows; the
/// engine tests check those) and the average is near the limit.
fn check(what: &str, s: &Samples, limit: i64) {
    let limit = limit as f64;
    let peak = s.peak(Duration::from_secs(1), Duration::from_secs(2));
    let avg = s.average(Duration::from_secs(1));
    eprintln!("{what}: limit {limit} B/s, 2 s peak {peak:.0} B/s, average {avg:.0} B/s");
    assert!(peak <= 1.25 * limit, "{what}: 2 s peak {peak:.0} B/s over the limit {limit} B/s");
    assert!(avg >= 0.6 * limit, "{what}: average {avg:.0} B/s far below the limit {limit} B/s");
}

#[test]
fn limit_changes_apply_to_a_running_download_at_once() {
    let fast = |p: &mut qbt_core::prefs::Preferences| p.advanced.refresh_interval = 250;
    let seeders: Vec<Node> = (0..SEEDERS).map(|_| Node::start(free_port(), fast)).collect();
    let leecher = Node::start(free_port(), |p| {
        fast(p);
        p.speed.alt_dl_limit = 300_000;
        p.speed.alt_up_limit = 0;
    });

    let content = make_content(&seeders[0].downloads(), "Big", &[("big.bin", 40 * 1024 * 1024)]);
    let torrent = make_torrent(&content, 256 * 1024);
    let mut hash = String::new();
    for s in &seeders {
        if !std::ptr::eq(s, &seeders[0]) {
            std::fs::create_dir_all(s.downloads().join("Big")).unwrap();
            std::fs::copy(content.join("big.bin"), s.downloads().join("Big/big.bin")).unwrap();
        }
        hash = s
            .app
            .runtime()
            .block_on(s.app.add_torrent(
                TorrentSource::Bytes { data: torrent.clone() },
                AddTorrentParams { save_path: Some(s.downloads().to_string_lossy().into()), ..Default::default() },
            ))
            .expect("seeder add");
        s.wait_for(&hash, Duration::from_secs(60), "seeder to finish checking", is_seeding);
        s.app.set_upload_limit(vec![hash.clone()], SEEDER_BPS).unwrap();
    }

    leecher
        .app
        .runtime()
        .block_on(leecher.app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default()))
        .expect("leecher add");
    let peers = seeders.iter().map(|s| format!("127.0.0.1:{}", s.port)).collect();
    leecher.app.add_peers(vec![hash.clone()], peers).unwrap();

    let read = || leecher.app.server_state().dl_session_data;
    // Unlimited until every seeder has a full request backlog.
    let start = Instant::now();
    while read() < 3 * 1024 * 1024 || start.elapsed() < Duration::from_secs(3) {
        assert!(start.elapsed() < Duration::from_secs(60), "download did not start");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Alternative speed limits on.
    leecher.app.set_alt_speed_limits_enabled(true);
    check("alternative limit", &Samples::record(read, Duration::from_secs(6)), 300_000);

    // Off again: back to the seeders' speed.
    leecher.app.set_alt_speed_limits_enabled(false);
    let s = Samples::record(read, Duration::from_secs(3));
    let avg = s.average(Duration::from_secs(1));
    assert!(avg > 600_000.0, "alternative limit off: {avg:.0} B/s");

    // A per-torrent limit (Torrent Options).
    leecher.app.set_download_limit(vec![hash.clone()], 200_000).unwrap();
    check("torrent limit", &Samples::record(read, Duration::from_secs(6)), 200_000);
    leecher.app.set_download_limit(vec![hash.clone()], 0).unwrap();

    // A global limit.
    leecher.app.set_global_download_limit(250_000).unwrap();
    check("global limit", &Samples::record(read, Duration::from_secs(6)), 250_000);
}
