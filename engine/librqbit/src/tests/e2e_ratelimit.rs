// End-to-end checks that rate limit changes take effect quickly on live
// transfers, over real local TCP connections: a limit set while many block
// requests are already in flight must not let the backlog through at full
// speed, and the throughput must settle at the limit.

use std::{
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroU32,
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, ensure};
use bytes::Bytes;
use tokio::time::Instant;
use tracing::info;

use crate::{
    AddTorrent, AddTorrentOptions, ListenerMode, ManagedTorrent, Session, SessionOptions,
    create_torrent,
    limits::LimitsConfig,
    listen::ListenerOptions,
    spawn_utils::BlockingSpawner,
    tests::test_util::{
        DropChecks, create_default_random_dir_with_torrents, setup_test_logging, wait_until,
        wait_until_i_am_the_last_task,
    },
};

const MIB: usize = 1024 * 1024;

async fn make_torrent(size: usize) -> (tempfile::TempDir, Bytes) {
    let dir = create_default_random_dir_with_torrents(1, size, Some("rqbit_ratelimit"));
    let torrent = create_torrent(
        dir.path(),
        crate::CreateTorrentOptions {
            piece_length: Some(256 * 1024),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    let bytes = torrent.as_bytes().unwrap();
    (dir, bytes)
}

async fn session(out: &Path, listen: bool) -> anyhow::Result<Arc<Session>> {
    Session::new_with_opts(
        out.to_owned(),
        SessionOptions {
            dht: None,
            listen: listen.then(|| ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .await
}

async fn start_seeder(
    torrent: Bytes,
    dir: &Path,
    upload_bps: Option<u32>,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>, SocketAddr)> {
    let session = session(dir, true).await?;
    let handle = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent),
            Some(AddTorrentOptions {
                overwrite: true,
                skip_hash_check: true,
                output_folder: Some(dir.to_str().unwrap().to_owned()),
                ratelimits: LimitsConfig {
                    upload_bps: upload_bps.and_then(NonZeroU32::new),
                    download_bps: None,
                },
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    wait_until(
        || {
            let s = handle.stats();
            ensure!(s.live.is_some() && s.finished, "seeder not live yet");
            Ok(())
        },
        Duration::from_secs(60),
    )
    .await?;
    let addr = session.listen_addr().context("no listen addr")?;
    Ok((session, handle, addr))
}

async fn start_leecher(
    torrent: Bytes,
    out: &Path,
    peers: Vec<SocketAddr>,
    download_bps: Option<u32>,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>)> {
    let session = session(out, false).await?;
    let handle = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent),
            Some(AddTorrentOptions {
                paused: true,
                ratelimits: LimitsConfig {
                    upload_bps: None,
                    download_bps: download_bps.and_then(NonZeroU32::new),
                },
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    wait_until(
        || {
            ensure!(!handle.piece_states().is_empty(), "not checked yet");
            Ok(())
        },
        Duration::from_secs(30),
    )
    .await?;
    handle.add_peers(peers);
    session.unpause(&handle).await?;
    Ok((session, handle))
}

/// Byte counter samples taken every 50 ms.
struct Samples(Vec<(Instant, u64)>);

impl Samples {
    async fn record(read: impl Fn() -> u64, duration: Duration) -> Samples {
        let start = Instant::now();
        let mut v = vec![(start, read())];
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        while start.elapsed() < duration {
            interval.tick().await;
            v.push((Instant::now(), read()));
        }
        Samples(v)
    }

    /// Highest rate (bytes/s) over windows of at least `window` that start at
    /// or after `from` (relative to the first sample) and end by the last sample.
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

    /// Average rate from `from` to the end.
    fn average(&self, from: Duration) -> f64 {
        let t0 = self.0[0].0;
        let &(ts, bs) = self.0.iter().find(|(t, _)| *t - t0 >= from).unwrap();
        let &(te, be) = self.0.last().unwrap();
        (be - bs) as f64 / (te - ts).as_secs_f64()
    }

    fn per_second(&self) -> Vec<u64> {
        let t0 = self.0[0].0;
        let mut out = Vec::new();
        let mut next = Duration::from_secs(1);
        let mut prev = self.0[0].1;
        for &(t, b) in &self.0 {
            if t - t0 >= next {
                out.push((b - prev) / 1024);
                prev = b;
                next += Duration::from_secs(1);
            }
        }
        out
    }
}

/// Checks the samples taken right after a limit of `limit` bytes/s was set:
/// within 1 s every 1 s and 2 s window stays within 1.25x the limit, and the
/// average after settling is close to it.
fn check_settles(what: &str, s: &Samples, limit: u32) {
    let limit = f64::from(limit);
    let settle = Duration::from_secs(1);
    let p1 = s.peak(settle, Duration::from_secs(1));
    let p2 = s.peak(settle, Duration::from_secs(2));
    let avg = s.average(Duration::from_secs(2));
    info!(
        what,
        limit,
        peak_1s = p1,
        peak_2s = p2,
        avg,
        per_second_kib = ?s.per_second(),
        "rate after the limit change"
    );
    assert!(
        p1 <= 1.25 * limit,
        "{what}: 1 s peak {p1:.0} B/s over limit {limit} B/s; per second (KiB): {:?}",
        s.per_second()
    );
    assert!(
        p2 <= 1.25 * limit,
        "{what}: 2 s peak {p2:.0} B/s over limit {limit} B/s; per second (KiB): {:?}",
        s.per_second()
    );
    assert!(
        avg >= 0.6 * limit,
        "{what}: average {avg:.0} B/s far below limit {limit} B/s; per second (KiB): {:?}",
        s.per_second()
    );
}

/// Waits until `read` grows by `bytes` and at least `min` has elapsed, so
/// that every peer has a full request backlog.
async fn warm_up(read: impl Fn() -> u64, bytes: u64, min: Duration) {
    let start = Instant::now();
    let base = read();
    while read() - base < bytes || start.elapsed() < min {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// These tests move tens of megabytes through several sessions each: one at a
// time, so that they neither disturb each other's measurements nor starve the
// other end-to-end tests.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(name: &'static str, f: impl AsyncFnOnce(&DropChecks) -> anyhow::Result<()>) {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    setup_test_logging();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let drop_checks = DropChecks::default();
        tokio::time::timeout(Duration::from_secs(150), f(&drop_checks))
            .await
            .context(name)
            .context("timed out")
            .unwrap()
            .unwrap();
        wait_until_i_am_the_last_task().await.unwrap();
        drop_checks.check().unwrap();
    });
}

// Several slow seeders keep the leecher's request pipelines full; a download
// limit set (per torrent or per session) while those requests are in flight
// applies at once, and lifting it restores the full speed at once.
async fn download_limit_change(
    drop_checks: &DropChecks,
    session_limit: bool,
) -> anyhow::Result<()> {
    const SEEDERS: usize = 4;
    const SEEDER_BPS: u32 = 500_000;
    const LIMIT: u32 = 400_000;
    let (dir, torrent) = make_torrent(40 * MIB).await;
    let mut seeders = Vec::new();
    for _ in 0..SEEDERS {
        let s = start_seeder(torrent.clone(), dir.path(), Some(SEEDER_BPS)).await?;
        drop_checks.add(&s.0, "seeder session");
        seeders.push(s);
    }
    let out = tempfile::TempDir::with_prefix("rqbit_ratelimit_leech").unwrap();
    let (leecher, handle) = start_leecher(
        torrent,
        out.path(),
        seeders.iter().map(|s| s.2).collect(),
        None,
    )
    .await?;
    drop_checks.add(&leecher, "leecher session");
    let read = || leecher.transfer_stats().payload_downloaded;

    warm_up(read, 3 * MIB as u64, Duration::from_secs(3)).await;
    let before = Samples::record(read, Duration::from_secs(1)).await;
    info!(rate = before.average(Duration::ZERO), "unlimited rate");
    let limit = LimitsConfig {
        upload_bps: None,
        download_bps: NonZeroU32::new(LIMIT),
    };
    if session_limit {
        leecher.set_ratelimits(limit);
    } else {
        handle.set_ratelimits(limit);
    }
    let after = Samples::record(read, Duration::from_secs(8)).await;
    check_settles(
        if session_limit {
            "session download"
        } else {
            "torrent download"
        },
        &after,
        LIMIT,
    );

    // Lifting the limit: back to the seeders' speed within a second or two.
    if session_limit {
        leecher.set_ratelimits(LimitsConfig::default());
    } else {
        handle.set_ratelimits(LimitsConfig::default());
    }
    let lifted = Samples::record(read, Duration::from_secs(3)).await;
    let rate = lifted.average(Duration::from_secs(1));
    info!(rate, per_second_kib = ?lifted.per_second(), "rate after lifting the limit");
    ensure!(
        rate > 2.0 * f64::from(LIMIT),
        "lifting the limit did not restore the speed: {rate:.0} B/s"
    );

    drop(handle);
    drop(leecher);
    drop(seeders);
    Ok(())
}

#[test]
fn test_e2e_torrent_download_limit_takes_effect_at_once() {
    run("torrent download limit", async |d| {
        download_limit_change(d, false).await
    });
}

#[test]
fn test_e2e_session_download_limit_takes_effect_at_once() {
    run("session download limit", async |d| {
        download_limit_change(d, true).await
    });
}

// A seeder uploading to a leecher (itself limited, so that the transfer lasts)
// gets an upload limit (per torrent or per session) mid-transfer.
async fn upload_limit_change(drop_checks: &DropChecks, session_limit: bool) -> anyhow::Result<()> {
    const LEECHER_BPS: u32 = 2_000_000;
    const LIMIT: u32 = 400_000;
    let (dir, torrent) = make_torrent(24 * MIB).await;
    let (seeder, seed, addr) = start_seeder(torrent.clone(), dir.path(), None).await?;
    drop_checks.add(&seeder, "seeder session");
    let out = tempfile::TempDir::with_prefix("rqbit_ratelimit_leech").unwrap();
    let (leecher, handle) =
        start_leecher(torrent, out.path(), vec![addr], Some(LEECHER_BPS)).await?;
    drop_checks.add(&leecher, "leecher session");
    let read = || seeder.transfer_stats().payload_uploaded;

    warm_up(read, 2 * MIB as u64, Duration::from_secs(2)).await;
    let limit = LimitsConfig {
        upload_bps: NonZeroU32::new(LIMIT),
        download_bps: None,
    };
    if session_limit {
        seeder.set_ratelimits(limit);
    } else {
        seed.set_ratelimits(limit);
    }
    let after = Samples::record(read, Duration::from_secs(6)).await;
    check_settles(
        if session_limit {
            "session upload"
        } else {
            "torrent upload"
        },
        &after,
        LIMIT,
    );

    drop(handle);
    drop(leecher);
    drop(seed);
    drop(seeder);
    Ok(())
}

#[test]
fn test_e2e_torrent_upload_limit_takes_effect_at_once() {
    run("torrent upload limit", async |d| {
        upload_limit_change(d, false).await
    });
}

#[test]
fn test_e2e_session_upload_limit_takes_effect_at_once() {
    run("session upload limit", async |d| {
        upload_limit_change(d, true).await
    });
}
