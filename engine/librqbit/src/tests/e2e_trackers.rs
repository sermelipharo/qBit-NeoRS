//! End-to-end tracker test: a seeder and a leecher find each other only
//! through a local HTTP tracker.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use parking_lot::Mutex;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions, TrackerState,
    create_torrent,
    listen::ListenerOptions,
    spawn_utils::BlockingSpawner,
    tests::test_util::{
        TestPeerMetadata, create_default_random_dir_with_torrents, setup_test_logging,
    },
};

#[derive(Clone, Debug)]
struct Announce {
    port: u16,
    event: Option<String>,
    left: u64,
    uploaded: u64,
    numwant: Option<u32>,
    key: Option<String>,
    user_agent: Option<String>,
}

#[derive(Default)]
struct FakeTracker {
    announces: Vec<Announce>,
    // port -> left
    swarm: HashMap<u16, u64>,
    completed: u32,
    scrapes: u32,
}

fn query_params(q: &str) -> HashMap<String, Vec<u8>> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            (
                k.to_owned(),
                urlencoding::decode_binary(v.as_bytes()).into_owned(),
            )
        })
        .collect()
}

fn param<T: std::str::FromStr>(p: &HashMap<String, Vec<u8>>, k: &str) -> Option<T> {
    std::str::from_utf8(p.get(k)?).ok()?.parse().ok()
}

fn handle_request(
    state: &Mutex<FakeTracker>,
    path: &str,
    query: &str,
    ua: Option<String>,
) -> Vec<u8> {
    let p = query_params(query);
    let mut st = state.lock();
    if path == "/scrape" {
        st.scrapes += 1;
        let hash = p.get("info_hash").cloned().unwrap_or_default();
        let complete = st.swarm.values().filter(|l| **l == 0).count();
        let incomplete = st.swarm.len() - complete;
        let mut body = b"d5:filesd20:".to_vec();
        body.extend_from_slice(&hash);
        body.extend_from_slice(
            format!(
                "d8:completei{complete}e10:downloadedi{}e10:incompletei{incomplete}eeee",
                st.completed
            )
            .as_bytes(),
        );
        return body;
    }
    let port: u16 = param(&p, "port").unwrap_or(0);
    let left: u64 = param(&p, "left").unwrap_or(0);
    let event = p
        .get("event")
        .map(|e| String::from_utf8_lossy(e).into_owned());
    st.announces.push(Announce {
        port,
        event: event.clone(),
        left,
        uploaded: param(&p, "uploaded").unwrap_or(0),
        numwant: param(&p, "numwant"),
        key: p
            .get("key")
            .map(|k| String::from_utf8_lossy(k).into_owned()),
        user_agent: ua,
    });
    match event.as_deref() {
        Some("stopped") => {
            st.swarm.remove(&port);
        }
        Some("completed") => {
            st.completed += 1;
            st.swarm.insert(port, left);
        }
        _ => {
            st.swarm.insert(port, left);
        }
    }
    let complete = st.swarm.values().filter(|l| **l == 0).count();
    let incomplete = st.swarm.len() - complete;
    let mut peers = Vec::new();
    for other in st.swarm.keys().filter(|o| **o != port) {
        peers.extend_from_slice(&Ipv4Addr::LOCALHOST.octets());
        peers.extend_from_slice(&other.to_be_bytes());
    }
    let mut body = format!(
        "d8:completei{complete}e10:incompletei{incomplete}e8:intervali1800e12:min intervali1e5:peers{}:",
        peers.len()
    )
    .into_bytes();
    body.extend_from_slice(&peers);
    body.push(b'e');
    body
}

/// A minimal HTTP/1.1 tracker.
async fn start_tracker() -> anyhow::Result<(SocketAddr, Arc<Mutex<FakeTracker>>)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(Mutex::new(FakeTracker::default()));
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let st = st.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let req = String::from_utf8_lossy(&buf).into_owned();
                let mut lines = req.lines();
                let target = lines
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let ua = lines
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                    .map(|(_, v)| v.trim().to_owned());
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let body = handle_request(&st, path, query, ua);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
            });
        }
    });
    Ok((addr, state))
}

async fn session(dir: &std::path::Path, name: &str) -> anyhow::Result<Arc<Session>> {
    let session = Session::new_with_opts(
        dir.into(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            persistence: None,
            peer_id: Some(TestPeerMetadata::good().as_peer_id()),
            listen: Some(ListenerOptions {
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                ..Default::default()
            }),
            client_name_and_version: Some(format!("test-{name}/1.0")),
            ..Default::default()
        },
    )
    .await?;
    // Loopback listeners have no announce port: report the real one, which
    // also exercises runtime tracker settings.
    let port = session.listen_addr().context("no listen addr")?.port();
    let mut s = session.tracker_settings();
    s.announce_port = port;
    s.stop_tracker_timeout = Duration::from_secs(3);
    session.set_tracker_settings(s)?;
    Ok(session)
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) -> anyhow::Result<()> {
    timeout(Duration::from_secs(20), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {what}"))
}

fn has(state: &Mutex<FakeTracker>, port: u16, event: Option<&str>) -> bool {
    state
        .lock()
        .announces
        .iter()
        .any(|a| a.port == port && a.event.as_deref() == event)
}

async fn e2e_trackers() -> anyhow::Result<()> {
    setup_test_logging();
    let (tracker_addr, state) = start_tracker().await?;
    let tracker_url = format!("http://{tracker_addr}/announce");
    // A dead tracker first in the same tier: failover must reach the live one.
    let dead_url = {
        let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        format!("http://{}/announce", l.local_addr()?)
    };

    let files = create_default_random_dir_with_torrents(2, 64 * 1024, Some("test_e2e_trackers"));
    let torrent = create_torrent(
        files.path(),
        CreateTorrentOptions {
            name: None,
            piece_length: Some(16384),
            trackers: vec![dead_url.clone(), tracker_url.clone()],
        },
        &BlockingSpawner::new(1),
    )
    .await?;
    let total_len: u64 = 2 * 64 * 1024;

    // Seeder.
    let seeder = session(files.path(), "seeder").await?;
    let seeder_port = seeder.listen_addr().unwrap().port();
    let seeder_handle = seeder
        .add_torrent(
            AddTorrent::from_bytes(torrent.as_bytes()?),
            Some(AddTorrentOptions {
                output_folder: Some(files.path().to_str().unwrap().to_owned()),
                overwrite: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    assert_eq!(
        seeder_handle.tracker_list(),
        vec![(dead_url.clone(), 0), (tracker_url.clone(), 0)]
    );

    wait_for("seeder started", || {
        has(&state, seeder_port, Some("started"))
    })
    .await?;
    {
        let st = state.lock();
        let a = st.announces.iter().find(|a| a.port == seeder_port).unwrap();
        assert_eq!(a.left, 0, "seeder announces left=0");
        assert_eq!(a.numwant, Some(200));
        assert_eq!(a.key.as_ref().map(|k| k.len()), Some(8));
        assert_eq!(a.user_agent.as_deref(), Some("test-seeder/1.0"));
    }
    wait_for("seeder tracker status", || {
        let t = seeder_handle.trackers();
        t.len() == 2
            && t[0].url == tracker_url
            && t[0].state == TrackerState::Working
            && t[1].state == TrackerState::NotWorking
    })
    .await?;
    let dead = seeder_handle
        .trackers()
        .into_iter()
        .find(|t| t.url == dead_url)
        .unwrap();
    assert!(!dead.message.is_empty());
    assert!(
        !dead.message.contains("announce"),
        "no URL in messages: {}",
        dead.message
    );

    // Leecher: no initial peers, so the tracker is the only way to find the seeder.
    let client_dir = TempDir::with_prefix("test_e2e_trackers_client")?;
    let leecher = session(client_dir.path(), "leecher").await?;
    let leecher_port = leecher.listen_addr().unwrap().port();
    let leecher_handle = leecher
        .add_torrent(
            AddTorrent::from_bytes(torrent.as_bytes()?),
            Some(AddTorrentOptions::default()),
        )
        .await?
        .into_handle()
        .context("no handle")?;

    wait_for("leecher started", || {
        has(&state, leecher_port, Some("started"))
    })
    .await?;
    {
        let st = state.lock();
        let a = st
            .announces
            .iter()
            .find(|a| a.port == leecher_port && a.event.as_deref() == Some("started"))
            .unwrap();
        assert_eq!(a.left, total_len);
    }
    timeout(
        Duration::from_secs(20),
        leecher_handle.wait_until_completed(),
    )
    .await
    .context("download timed out")??;
    wait_for("leecher completed", || {
        has(&state, leecher_port, Some("completed"))
    })
    .await?;
    {
        let st = state.lock();
        let a = st
            .announces
            .iter()
            .find(|a| a.port == leecher_port && a.event.as_deref() == Some("completed"))
            .unwrap();
        assert_eq!(a.left, 0);
    }

    // Swarm counters from the announce response, times downloaded via scrape.
    wait_for("leecher status with scrape", || {
        leecher_handle.trackers().iter().any(|t| {
            t.url == tracker_url
                && t.state == TrackerState::Working
                && t.seeds.is_some()
                && t.downloaded.is_some()
        })
    })
    .await?;
    assert!(state.lock().scrapes > 0);
    assert!(leecher_handle.swarm_counts().0.is_some());
    let seeder_addr: SocketAddr = (Ipv4Addr::LOCALHOST, seeder_port).into();
    assert!(
        leecher_handle
            .peer_sources(seeder_addr)
            .contains(crate::PeerSource::Tracker)
    );
    assert!(leecher_handle.peer_source_counts().dht == 0);

    // Forced reannounce ignores the minimum interval and sends no event.
    let before = state.lock().announces.len();
    assert!(leecher_handle.force_reannounce(None));
    wait_for("forced reannounce", || {
        state.lock().announces[before..]
            .iter()
            .any(|a| a.port == leecher_port && a.event.is_none())
    })
    .await?;

    // Stopping sends `stopped` and resets statuses.
    leecher.pause(&leecher_handle).await?;
    wait_for("leecher stopped", || {
        has(&state, leecher_port, Some("stopped"))
    })
    .await?;
    wait_for("leecher status reset", || {
        leecher_handle
            .trackers()
            .iter()
            .all(|t| t.state == TrackerState::NotContacted && !t.updating)
    })
    .await?;

    // The list survives pause/unpause and edits apply to the stopped torrent.
    assert_eq!(
        leecher_handle.remove_trackers(std::slice::from_ref(&dead_url)),
        1
    );
    assert_eq!(
        leecher_handle.tracker_list(),
        vec![(tracker_url.clone(), 0)]
    );
    let starts_before = state
        .lock()
        .announces
        .iter()
        .filter(|a| a.port == leecher_port && a.event.as_deref() == Some("started"))
        .count();
    leecher.unpause(&leecher_handle).await?;
    wait_for("leecher restarted", || {
        state
            .lock()
            .announces
            .iter()
            .filter(|a| a.port == leecher_port && a.event.as_deref() == Some("started"))
            .count()
            > starts_before
    })
    .await?;

    // Deleting the seeder sends `stopped`, with the upload counted.
    seeder.delete(seeder_handle.id().into(), false).await?;
    wait_for("seeder stopped", || {
        has(&state, seeder_port, Some("stopped"))
    })
    .await?;
    {
        let st = state.lock();
        let a = st
            .announces
            .iter()
            .find(|a| a.port == seeder_port && a.event.as_deref() == Some("stopped"))
            .unwrap();
        assert!(a.uploaded > 0, "seeder reports uploaded bytes");
    }

    // Session shutdown waits for the leecher's `stopped`.
    let stops_before = state
        .lock()
        .announces
        .iter()
        .filter(|a| a.port == leecher_port && a.event.as_deref() == Some("stopped"))
        .count();
    leecher.stop().await;
    let stops_after = state
        .lock()
        .announces
        .iter()
        .filter(|a| a.port == leecher_port && a.event.as_deref() == Some("stopped"))
        .count();
    assert_eq!(stops_after, stops_before + 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_trackers() -> anyhow::Result<()> {
    timeout(Duration::from_secs(90), e2e_trackers()).await?
}
