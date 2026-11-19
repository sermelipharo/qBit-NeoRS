# engine — forked librqbit

This directory is a fork of the library crates of [rqbit](https://github.com/ikatson/rqbit)
(`librqbit` 9.0.1, upstream commit `57e95d16c830863a1fde32d8fa98153b53a3d0b4`),
licensed under Apache-2.0 (see `LICENSE`). It is the pure-Rust BitTorrent engine
underneath qBit NeoRS.

Upstream crates kept: `librqbit`, `librqbit_core`, `bencode`, `buffers`,
`clone_to_owned`, `dht`, `librqbit_lsd`, `peer_binary_protocol`, `sha1w`,
`tracker_comms`, `upnp`. Dropped: the `rqbit` CLI, the Tauri desktop app, the
React web UI and the UPnP media server.

All changes made on top of upstream are recorded in the git history of this
repository and summarised in `../docs/ENGINE.md`.

Synced with upstream up to `f23bacdddcc5318caa813418b8eb1b1d7307faaa` (2026-11-03,
checked 2026-11-19). Upstream improvements are merged regularly:
`scripts/engine-upstream.sh` lists what changed upstream since this point, and
"Syncing with upstream" in `../docs/ENGINE.md` describes the procedure and records
what was ported or skipped.
