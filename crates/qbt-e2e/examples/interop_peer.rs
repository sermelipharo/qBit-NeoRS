//! One engine session for interoperability tests against other clients
//! (`scripts/interop/libtorrent_interop.py`). Local only: no DHT, LSD or
//! port mapping; listens on 127.0.0.1.
//!
//! ```text
//! interop_peer --add <file.torrent | magnet> --out <dir> [--listen <port>]
//!              [--peer <ip:port>]... [--tracker <url>] [--timeout <secs>]
//!              [--seed-for <secs>] [--plaintext] [--stdin-peers]
//! ```
//!
//! Prints `LISTEN <port>`, `HASH <v1 or v2 id> <alt or ->` once added, and
//! `DONE` when every piece is verified, then keeps seeding for `--seed-for`
//! seconds (default 0), printing `LAYERS <bytes>` when a hybrid torrent
//! added without piece layers fetched them from a peer. A pure v2 torrent
//! whose .torrent lacks piece layers prints `PARTIAL <files without a
//! layer>` once every piece that can be verified is had while some layers
//! are still missing. Exits with 1 on timeout or error. `--plaintext`: no
//! MSE encryption (libtorrent upgrades v1 connections of hybrid torrents to
//! v2 only without it). `--stdin-peers`: lines `PEER <ip:port>` on stdin
//! add peers while running.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use librqbit::{
    AddTorrent, AddTorrentOptions, EncryptionPolicy, ListenerOptions, ManagedTorrentState, NetSettings, Session,
    SessionOptions,
};

struct Args {
    add: String,
    out: String,
    listen: u16,
    peers: Vec<SocketAddr>,
    timeout: u64,
    seed_for: u64,
    tracker: Option<String>,
    plaintext: bool,
    stdin_peers: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut a = Args {
        add: String::new(),
        out: String::new(),
        listen: 0,
        peers: Vec::new(),
        timeout: 120,
        seed_for: 0,
        tracker: None,
        plaintext: false,
        stdin_peers: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or_else(|| anyhow::anyhow!("{k} needs a value"));
        match k.as_str() {
            "--add" => a.add = v()?,
            "--out" => a.out = v()?,
            "--listen" => a.listen = v()?.parse()?,
            "--peer" => a.peers.push(v()?.parse()?),
            "--timeout" => a.timeout = v()?.parse()?,
            "--seed-for" => a.seed_for = v()?.parse()?,
            "--tracker" => a.tracker = Some(v()?),
            "--plaintext" => a.plaintext = true,
            "--stdin-peers" => a.stdin_peers = true,
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    anyhow::ensure!(!a.add.is_empty() && !a.out.is_empty(), "--add and --out are required");
    Ok(a)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let dir = tempfile::tempdir()?;
    let session = Session::new_with_opts(
        dir.path().to_path_buf(),
        SessionOptions {
            dht: None,
            persistence: None,
            disable_local_service_discovery: true,
            net: NetSettings {
                encryption: if args.plaintext { EncryptionPolicy::Disabled } else { EncryptionPolicy::Enabled },
                ..Default::default()
            },
            listen: Some(ListenerOptions {
                listen_addr: (Ipv4Addr::LOCALHOST, args.listen).into(),
                // Loopback listeners announce no port by default.
                announce_port: (args.listen != 0).then_some(args.listen),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await?;
    if let Some(a) = session.listen_addr() {
        println!("LISTEN {}", a.port());
    }
    let add = if args.add.starts_with("magnet:") {
        AddTorrent::from_url(args.add.clone())
    } else {
        AddTorrent::from_bytes(std::fs::read(&args.add)?)
    };
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let added = tokio::time::timeout(
        Duration::from_secs(args.timeout),
        session.add_torrent(
            add,
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(args.out.clone()),
                initial_peers: (!args.peers.is_empty()).then(|| args.peers.clone()),
                replace_trackers: args.tracker.clone().map(|t| vec![(t, 0)]),
                trackers_local_source: args.tracker.as_ref().map(|_| true),
                ..Default::default()
            }),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out adding the torrent"))??;
    let h = added.into_handle().ok_or_else(|| anyhow::anyhow!("no handle"))?;
    println!("HASH {} {}", h.info_hash().as_string(), h.info_hash_alt().map(|a| a.as_string()).unwrap_or("-".into()));
    let (peer_tx, mut peer_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    if args.stdin_peers {
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                if let Some(Ok(addr)) = line.strip_prefix("PEER ").map(|a| a.trim().parse()) {
                    let _ = peer_tx.send(addr);
                }
            }
        });
    }
    let mut partial_reported = false;
    loop {
        while let Ok(addr) = peer_rx.try_recv() {
            h.add_peers(vec![addr]);
        }
        let done = h.with_state(|s| match s {
            ManagedTorrentState::Live(_) => Ok(h.stats().finished),
            ManagedTorrentState::Error(e) => Err(anyhow::anyhow!("torrent error: {e:#}")),
            _ => Ok(false),
        })?;
        if done {
            break;
        }
        let missing = h.missing_piece_layers();
        if missing > 0 && !partial_reported && h.with_state(|s| matches!(s, ManagedTorrentState::Live(_))) {
            // Every piece that can be verified (its file's layer known) is had.
            let states = h.piece_states();
            let all_known_had = h
                .with_metadata(|m| {
                    states.iter().enumerate().all(|(p, &s)| s == 2 || !m.info.piece_hash_known(p as u32))
                })
                .unwrap_or(false);
            if all_known_had && !states.is_empty() {
                println!("PARTIAL {missing}");
                partial_reported = true;
            }
        }
        if Instant::now() > deadline {
            let s = h.stats();
            anyhow::bail!("timed out: {} of {} bytes", s.progress_bytes, s.total_bytes);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("DONE");
    let seed_until = Instant::now() + Duration::from_secs(args.seed_for);
    while Instant::now() < seed_until {
        if let Some(layers) = h.take_fetched_piece_layers() {
            println!("LAYERS {}", layers.len());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    session.stop().await;
    Ok(())
}
