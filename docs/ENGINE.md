# Engine fork (librqbit 9.0.1) — changes and capability map

The engine lives in `engine/` (fork of rqbit's library crates at upstream
commit `57e95d1`). This file records every capability the qBittorrent feature
set needs, whether upstream had it, and what the fork adds. "Syncing with
upstream" at the end describes how upstream changes are merged and logs what
was ported or skipped.

Design rule: the engine exposes *mechanisms* (per-torrent tracker list,
file priorities, rate limiters, peer details, storage moves); the core
(`qbt-core`) owns *policy* (categories, queueing, share limits, schedules).

| Capability | Upstream 9.0.1 | Fork work |
|---|---|---|
| TCP + uTP, DHT, LSD, UPnP port mapping, PEX, magnet metadata (BEP 9) | yes | per-torrent DHT/PeX/LSD switches (engine-net, done) |
| Session/torrent rate limits (governor, at request time) | yes (set at add time) | engine-live: runtime setters, uTP/LAN exemptions (done); own token bucket at the socket, live changes apply at once (done); alt-speed switch is core policy |
| Per-tracker status, tiers, scrape, add/edit/remove at runtime, reannounce | no | engine-trackers |
| DHT/PeX/LSD peer-source counters | no | engine-trackers |
| File priorities (0/1/6/7) driving piece order | include/exclude only | engine-live (done) |
| Sequential download, first/last piece priority | no | engine-live (done) |
| Piece states / availability, distributed copies | partial | engine-live (done) |
| Peer details (flags, client, progress, source, uTP/TCP, speeds) | partial | engine-live (done; DHT/LSD source tags need engine-trackers) |
| Choking: upload slots (global/per torrent), fixed vs rate-based, seed choking algorithms, optimistic unchoke | no (unchoke all) | engine-live (done) |
| Super seeding (BEP 16) | no | engine-live (done) |
| Global/per-torrent connection limits at runtime | per torrent at add time | engine-live (done) |
| Session statistics (payload vs overhead, wasted, connections) | partial | engine-live (done) |

| Session/torrent rate limits (governor, at request time) | yes (set at add time) | runtime setters, socket-level throttling, alt-speed switch |
| Per-tracker status, tiers, scrape, add/edit/remove at runtime, reannounce | no | **done** (engine-trackers) |
| DHT/PeX/LSD peer-source counters | no | **done** for tracker/DHT/LSD (engine-trackers); PeX needs engine-live to call `record_peer_source` |
| File priorities (0/1/6/7) driving piece order | include/exclude only | engine-live |
| Sequential download, first/last piece priority | no | engine-live |
| Piece states / availability, distributed copies | partial | engine-live |
| Peer details (flags, client, progress, source, uTP/TCP, speeds) | partial | engine-live |
| Choking: upload slots (global/per torrent), fixed vs rate-based, seed choking algorithms, optimistic unchoke | no (unchoke all) | engine-live |
| Super seeding (BEP 16) | no | engine-live |
| Global/per-torrent connection limits at runtime | per torrent at add time | engine-live |
| Session statistics (payload vs overhead, wasted, connections) | partial | engine-live |
| Move storage, rename files/folders, force recheck, skip hash check | no | engine-storage (done, tested) |
| `.!qB` incomplete suffix, pre-allocation, `.unwanted` folder | no | engine-storage (done, tested) |
| Missing files / disk full error kinds, runtime "max active checking", file handle pool, OS cache modes, delete with trash callback | no | engine-storage (done, tested) |
| Path sanitization of torrent names and renames | partial (`..` and separators rejected) | engine-storage (done, tested) |
| Web seeds (BEP 19) | no | `engine/webseed` + pseudo-peers in the live torrent, runtime add/edit/remove (engine-net, done) |
| Protocol encryption MSE/PE (allow/require/disable) | no | `engine/mse`, wired into TCP/uTP/proxied connections (engine-net, done) |
| HTTP CONNECT / SOCKS4 proxies for peers | SOCKS5 only | `engine/proxy` for peers, reqwest proxies for trackers/web seeds, runtime switch (engine-net, done) |
| Anonymous mode, announce IP/port, bind interface, outgoing port range | partial | done (engine-net + engine-trackers) |
| Live reconfiguration (port, DHT/LSD/PeX toggles, IP filter, bans) | no | done (engine-net); tracker settings in engine-trackers |
| NAT-PMP | no | `engine/natpmp` next to UPnP, extra ports, status (engine-net, done) |
| I2P | no | `engine/i2p`: I2P peers, `.i2p` HTTP trackers, mixed mode (engine-net, done; no I2P DHT/PeX) |
| v2 / hybrid torrents | v1 only | pure v2 from .torrent (started at once; missing piece layers fetched from peers or found on disk, each file verified and downloaded once its layer is known) and `btmh` magnets (merkle verification, BEP 52 hash messages), hybrid via its v1 half, announced, reachable and served under both hashes, v1 connections upgraded to v2, layers of a hybrid added by magnet fetched and served; checked against libtorrent 2.1; see "Pure v2 torrents (BEP 52)" |

## engine-live: live torrent features

All of this is generic engine mechanism; the core decides policy (defaults,
alternative speed limits, share-limit actions, persistence). Settings live
outside the live state: they can be changed while a torrent is live, paused
or still checking, and survive pause/unpause. The engine does not persist
them (`only_files` is still saved by the engine's own session persistence);
the core stores and re-applies them after a restart.

Code: `engine/librqbit/src/torrent_state/control.rs` (the `ManagedTorrent`
API), `torrent_state/runtime.rs` (per-torrent runtime settings),
`torrent_state/live/{priorities,availability,choker,peer_details,piece_info,superseed}.rs`,
`peer_policy.rs` (session policy, peer tick, stats), `conn_limiter.rs`,
`peer_client.rs`.

### File priorities and piece picking

```rust
// ManagedTorrent
pub fn file_priorities(&self) -> Vec<u8>;
pub fn set_file_priorities(&self, priorities: Vec<u8>) -> anyhow::Result<()>;
pub fn sequential(&self) -> bool;
pub fn set_sequential(&self, sequential: bool);
pub fn first_last_piece_priority(&self) -> bool;
pub fn set_first_last_piece_priority(&self, enabled: bool);
// constants: PRIORITY_IGNORE = 0, PRIORITY_DEFAULT = 1, PRIORITY_MAX = 7
```

* libtorrent levels `0..=7`, one entry per file (wrong length is an error,
  values above 7 are clamped). New torrents start at 1, or 0 for files left
  out by `only_files`. qBittorrent uses 0 (Do not download), 1, 6 and 7.
* Priority 0 is the old `only_files` exclusion: pieces used only by priority-0
  files are never requested. `only_files` / `Session::update_only_files`
  still work and are kept in sync (deselected files get 0, reselected ones 1).
* Each piece gets the highest priority of the files it overlaps (1 byte per
  piece). With first/last piece priority, the first and last 1% (at least one
  piece) of every wanted file get 7, qBittorrent's rule.
* Picker order: streaming read-ahead pieces first (unchanged), then the
  highest piece priority; within a priority, rarest first (fewest connected
  peers having it) with a per-torrent pseudo-random tie-break, or lowest index
  first when sequential. Unlike libtorrent, sequential mode still honours
  priorities 1-6, not only the top priority. The old "files in name order,
  first/last piece of each file first" order is gone.
* Changing priorities of a finished torrent so that it needs data again
  reconnects the peers it had dropped.

### Piece export

```rust
pub fn piece_states(&self) -> Vec<u8>;      // PIECE_STATE_MISSING 0 / DOWNLOADING 1 / HAVE 2
pub fn pieces_have(&self) -> u32;
pub fn piece_availability(&self) -> Vec<u32>;
pub fn distributed_copies(&self) -> f64;
```

* `piece_states`: 1 = requested from a peer right now. Empty while checking
  or without metadata; paused torrents report 0/2.
* `piece_availability`: connected peers having each piece (kept as one
  lock-free `u16` per piece, updated on bitfield/have/disconnect). Empty when
  we have every piece (libtorrent semantics), zeros when paused.
* `distributed_copies`: libtorrent's definition — minimum over pieces of
  (connected peers having it + our own copy), plus the fraction (rounded down
  to thousandths) of pieces above that minimum. `-1` when we are a seed or
  the state is unknown.

### Peers

```rust
pub fn peers_detailed(&self) -> Vec<PeerDetails>;
pub fn connections(&self) -> u32;
pub fn add_peers(&self, addrs: Vec<SocketAddr>) -> usize;           // source Manual
pub fn add_peer_with_source(&self, addr: SocketAddr, source: PeerSource) -> bool;
pub fn disconnect_peer(&self, addr: SocketAddr) -> bool;
// TorrentStateLive::add_peer_with_source(addr, source) -> Result<bool> is the live-state equivalent
pub fn client_name_from_peer_id(id: &[u8; 20]) -> String;            // crate root
```

`PeerDetails { addr, connection: ConnectionKind (Tcp/Utp/Socks), client,
peer_id: [u8; 20], flags: PeerFlags, sources: PeerSources, progress,
download_rate, upload_rate, downloaded, uploaded, relevance,
downloading_files: Vec<usize> }`, `PeerFlags { am_interested,
peer_interested, am_choking, peer_choking, optimistic_unchoke, snubbed,
incoming, utp, encrypted, holepunched, seed, super_seeding }`.

* Only handshaken peers are listed. `client` is the extended handshake `v`
  string if sent, else decoded from the peer id (Azureus `-qB5240-` →
  "qBittorrent 5.2.4", Transmission's two-digit 1.x-3.x minor versions, the
  µTorrent build letter, Shadow, Mainline and a table of custom ids; unknown
  ids give `Unknown [<prefix>]`).
* Rates: payload bytes/s, a libtorrent-style 5 s moving average updated once a
  second. `downloaded`/`uploaded`: payload bytes with this peer since the
  torrent went live. `relevance`: pieces the peer has that we lack / pieces we
  lack (0 when we have everything). `snubbed`: requests outstanding and no
  data for 20 s. `encrypted` is set by engine-net (MSE/RC4); `holepunched` is
  always false.
* Sources are a set (`PeerSources::contains`): the existing peer stream tags
  peers `Tracker` (it carries trackers, DHT, LSD and `initial_peers`), PEX
  messages `Pex`, incoming connections `Incoming`, `add_peers` `Manual`.
  engine-trackers can tag DHT/LSD peers by calling `add_peer_with_source`.
* `add_peers` on a torrent that isn't live queues up to 500 addresses and
  connects them when it starts. `disconnect_peer` closes the connection and
  doesn't reconnect it (the hook for banning; the ban list itself is session
  IP filtering). A "have" is now sent to every peer that doesn't have the
  whole torrent, even one that already has the piece (needed by super-seeding
  seeds).
* HAVEs are never lost: each writer remembers which pieces it advertised
  (BITFIELD + HAVE, one bit per piece, nothing for connections that started
  with every piece). Upstream dropped HAVEs when a writer fell more than 128
  pieces behind the torrent's broadcast; now the writer compares the have
  bitfield with what it advertised and sends the difference, each piece
  once (`advertised_haves.rs`, `peer_connection::tests`).
* Peer writes are batched: after waiting for one message the writer
  serializes everything else already queued (our requests, HAVEs, a
  throttled upload) into the connection's existing ~17 KiB write buffer and
  sends it with one write, one timeout and one yield (upstream: one of each
  per message). Order is kept; a `piece` block is read straight into the
  buffer and only into an otherwise empty batch, so queued messages never
  wait for a disk read and two blocks never share a write
  (`write_batch.rs`).

### Choking and upload slots

```rust
// SessionOptions::peer_policy: PeerPolicyOptions { max_uploads: Option<u32>,
//   max_uploads_per_torrent: Option<u32>, choking_algorithm: ChokingAlgorithm,
//   seed_choking_algorithm: SeedChokingAlgorithm, max_connections: Option<u32>,
//   rate_limit_utp: bool, rate_limit_lan: bool }   (default: unlimited, FixedSlots,
//   FastestUpload, limits apply to uTP and LAN)
// Session
pub fn peer_policy(&self) -> PeerPolicyOptions;
pub fn set_peer_policy(&self, opts: PeerPolicyOptions);
pub fn set_max_uploads(&self, max_uploads: Option<u32>);
pub fn set_default_max_uploads_per_torrent(&self, max_uploads: Option<u32>);
pub fn set_choking_algorithm(&self, algorithm: ChokingAlgorithm);        // FixedSlots | RateBased
pub fn set_seed_choking_algorithm(&self, algorithm: SeedChokingAlgorithm); // RoundRobin | FastestUpload | AntiLeech
// ManagedTorrent
pub fn max_uploads(&self) -> Option<u32>;
pub fn set_max_uploads(&self, max_uploads: Option<u32>);
```

* Every connection starts choked (the old unconditional unchoke is gone).
  When a peer becomes interested it is unchoked at once if the session and
  torrent have a free slot (libtorrent's preemptive unchoke). Not-interested
  peers are choked immediately and their slot is handed on.
* One session task ticks every second: per-peer rates; every 10 s (or right
  after a policy change) a rechoke over all interested peers of all live
  torrents; every 30 s an optimistic unchoke rotation. Ranking follows
  libtorrent `unchoke_sort`: most bytes received from the peer in the last
  round first (tit-for-tat), then the seed algorithm (round-robin with the
  20-piece quota and 1 minute minimum, fastest upload, or anti-leech score).
* Slots: fixed = session `max_uploads` (`None` unlimited); rate-based =
  libtorrent's `rate_based_choker` (thresholds 1 KiB/s + 2 KiB/s per slot,
  plus one). `max(1, slots / 5)` slots are optimistic (given to the peers
  that waited longest); a rechoke that reserves them while none is held fills
  them at once. Per-torrent `max_uploads` (`Some(0)` = no uploading) also
  bounds optimistic unchokes.
* Requests from choked peers are ignored; queued uploads to a peer that got
  choked are dropped.

### Super seeding

```rust
pub fn super_seeding(&self) -> bool;
pub fn set_super_seeding(&self, enabled: bool);
```

BEP 16, active only while we have every piece and only for connections made
while it is on: no bitfield is sent; each peer is shown (HAVE) the rarest
piece it lacks that isn't being shown to someone else, and the next one once
it announces that piece (up to two revealed pieces per peer, as libtorrent;
requests for other pieces are ignored). Turning it off sends those peers HAVE
for every piece they lack.

### Connection limits

```rust
pub fn set_max_connections(&self, max_connections: Option<u32>); // Session, global
pub fn peer_limit(&self) -> Option<usize>;                        // ManagedTorrent
pub fn set_peer_limit(&self, limit: Option<usize>);
```

Every peer connection (incoming or outgoing, including connecting) holds a
torrent slot and a session slot. Limits are resizable semaphores
(`conn_limiter.rs`): lowering below the current count refuses new connections
until enough close. Lowering a torrent's limit also disconnects the excess
(connecting peers first, then peers neither side is interested in, then the
slowest); they may be retried later within the limit. The global limit does
not disconnect. A torrent added without `peer_limit` (torrent or session) gets
128, as before.

### Rate limits

```rust
pub fn ratelimits(&self) -> LimitsConfig;                 // ManagedTorrent
pub fn set_ratelimits(&self, limits: LimitsConfig);
pub fn get_ratelimits(&self) -> LimitsConfig;              // Session
pub fn set_ratelimits(&self, limits: LimitsConfig);
pub fn set_rate_limit_utp(&self, apply: bool);             // Session
pub fn set_rate_limit_lan(&self, apply: bool);
```

Per-torrent limits now live with the torrent (not the live state), so they
work while paused and survive pause/unpause. With `rate_limit_utp` /
`rate_limit_lan` off, uTP peers / peers on loopback, private, link-local or
unique-local addresses skip the session limits (like libtorrent, the torrent
limits still apply). Limits below one 16 KiB block per second work (the
block just waits longer).

Where the limits apply (like libtorrent's bandwidth manager, not upstream
rqbit, which only paced new requests):

* Download: the peer's reader waits for the torrent, session and mixed mode
  limiters before it handles each `piece` message, so nothing more is read
  from the socket until the bytes are paid for. Data requested before a limit
  was set therefore cannot arrive at full speed (upstream let up to 128
  requested blocks per peer, 2 MiB, through unthrottled, which in a real
  swarm kept the old speed for tens of seconds). While any download limit
  applies to a peer, its request window is libtorrent's
  `request_queue_time × rate / block` (3 s of the peer's 5 s average rate,
  at least 2 blocks, at most the normal window), so the backlog stays small;
  without a limit the window is unchanged.
* Upload: the peer's writer queues accepted requests and waits for the
  limiters right before reading and sending each block (one at a time, in
  request order); other messages are not held behind them. A limit set while
  requests are queued applies to them too.
* Web seeds still pay for a piece's blocks before fetching it.

The limiter (`limits.rs`) is a token bucket per direction with a burst of
200 ms of the rate. Setting a limit starts it empty and changes the rate in
place: callers reserve bytes first come, first served (a reservation beyond
the tokens is debt that the caller sleeps off), waiters re-plan when the rate
changes and are released at once when the limit is removed, and an abandoned
reservation is given back. No timers or tasks: waiting callers sleep. Tests:
`limits::tests` (paused clock), `tests::e2e_ratelimit` (local seeders:
torrent/session download and upload limits set mid-transfer; 1 s and 2 s
windows ≤ 1.25× the limit from 1 s after the change, lifting restores the
speed) and `qbt-e2e/tests/rate_limits.rs` (alternative speed toggle,
per-torrent and global limits through the core).

### uTP/TCP mixed mode

```rust
pub enum MixedModeAlgorithm { PreferTcp /* default */, PeerProportional }
PeerPolicyOptions::mixed_mode: MixedModeAlgorithm      // serde default PreferTcp
pub fn set_mixed_mode_algorithm(&self, MixedModeAlgorithm); // Session, live
```

libtorrent's `mixed_mode_algorithm` (qBittorrent `utp_tcp_mixed_mode`), in
`mixed_mode.rs`. The once-a-second peer tick sums, per direction, the peers
outside the local network that transferred (non-zero rate) and their rates
by protocol (uTP vs everything else), then sets two extra session limiters
(TCP class, uTP class) that non-LAN peers wait on after the torrent and
session limiters:

* `PeerProportional` — libtorrent's algorithm unchanged: while uTP peers are
  active in a direction, TCP is throttled to
  `max(total_rate × tcp_peers / (tcp_peers + utp_peers), floor)` (no active
  TCP peer counts as one); uTP is never throttled.
* `PreferTcp` — in libtorrent this means "no balancing" because its uTP uses
  LEDBAT and yields to TCP by itself. The engine's uTP (`librqbit-utp`) uses
  CUBIC and does not yield, so the yielding is emulated: while both TCP and
  uTP peers are active in a direction and the session limit applies to uTP
  (`rate_limit_utp`), uTP gets ¾ of what TCP leaves of the session limit,
  never less than the floor (capped at the limit). Without a session limit
  (or with uTP exempt from it) nothing is throttled — there is no shared
  budget to protect.

Floors are libtorrent's lower limits: 5 kB/s up, 30 kB/s down. A limiter's
rate is only changed when it moves by more than 1/8 (fewer re-plans). Switching the algorithm clears both throttles; the
new one applies at the next tick. Policy tests: `mixed_mode::tests`.

### Statistics

```rust
pub fn transfer_stats(&self) -> SessionTransferStats; // Session
pub fn wasted_bytes(&self) -> u64;                     // ManagedTorrent
```

`SessionTransferStats { payload_downloaded, payload_uploaded,
protocol_downloaded, protocol_uploaded, ip_overhead_downloaded,
ip_overhead_uploaded, wasted_bytes, wasted_failed_bytes,
wasted_redundant_bytes, payload_download_rate, payload_upload_rate,
connected_peers, unchoked_peers, dht_nodes }`, cumulative since the session
started. Protocol bytes are every peer-protocol byte read or written
(handshakes, framing, payload); IP overhead is estimated per write/read as
libtorrent does (40/60 bytes TCP over IPv4/IPv6, 48/68 for uTP, per 1500-byte
packet). Wasted = bytes of pieces that failed the hash check + blocks we
didn't need (late after a cancel, already completed, or stolen by another
peer). qBittorrent's totals are `protocol + ip_overhead`; payload rates are
`payload_*_rate`. Tracker and DHT traffic are not included.

## engine/mse — Message Stream Encryption

New crate `librqbit-mse`: `initiate(stream, info_hash)` (outgoing, peer A),
`accept(stream, policy, lookup)` (incoming; detects plaintext handshakes and
identifies the torrent via `req2_hash(info_hash)`), and `MseStream`, an
`AsyncRead + AsyncWrite` wrapper applying RC4 (keystream offset 1024) with a
bounded write buffer. Only RC4 is negotiated inside the encrypted handshake,
matching qBittorrent's `allowed_enc_level = pe_rc4`. `EncryptionPolicy` maps
to qBittorrent's encryption modes (Enabled = allow, Forced = require,
Disabled). Degenerate DH public keys are rejected; pads and lengths are
bounded; malformed input returns errors, never panics.

## engine/webseed — BEP 19 web seeds

New crate `librqbit-webseed`: `Layout` (engine-independent file layout),
`file_url` (BEP 19 URL rules with percent-encoding; only http/https),
`requests_for_piece` (byte ranges across files, BEP 47 pad files skipped) and
`fetch_piece` (Range GETs, 206/200 handling, `Retry-After` on 503, bodies read
with a hard size limit). Hash verification and piece scheduling belong to the
live torrent.

## engine/natpmp — NAT-PMP port mapping

New crate `librqbit-natpmp` (RFC 6886): `Client::{external_address, map}`
with retransmission (250 ms doubling, 5 tries) and result-code errors,
`default_gateway()` (macOS `route -n get default`), and `run_mapper`, a task
that keeps a set of TCP/UDP ports mapped, renews at half the lease and removes
the mappings when stopped.

## engine/proxy — proxy connector

New crate `librqbit-proxy`: `connect(&ProxyConfig, Target, timeout)` for
qBittorrent's proxy types — HTTP `CONNECT` (Basic auth, bounded response head
read byte-by-byte so no tunnelled bytes are lost), SOCKS4/4a (user id) and
SOCKS5 (user/password) via tokio-socks — honouring "hostname lookup via
proxy" (`remote_dns`).

## engine/i2p — I2P through SAM v3.1

New crate `librqbit-i2p`: `Session::create` (HELLO, `SESSION CREATE
STYLE=STREAM` with qBittorrent's tunnel quantity/length settings, own
destination via `NAMING LOOKUP NAME=ME`), `connect` (STREAM CONNECT),
`accept` (STREAM ACCEPT, returns the peer destination), `lookup`, I2P base64
and `.b32.i2p` helpers. Lines are length-limited and every SAM error is
surfaced. Swarm integration: see engine-net.

## engine-net: networking wired into the live engine

Code: `engine/librqbit/src/net/` (settings, MSE glue, socket tuning, peer
filter, connection pacing, port mapping), `session/networking.rs` (the
`Session::set_*` methods), `stream_connect.rs` (outgoing connections),
`listen.rs`, `peer_connection.rs`, `i2p.rs` + `i2p_http.rs`, web seeds in
`torrent_state/webseeds.rs` + `torrent_state/live/webseed.rs`. Small additions
in the crates: `librqbit-mse` (`MseStream::into_parts`), `librqbit-upnp`
(TCP/UDP mappings, description, status callback), `librqbit-i2p`
(`lookup(&self)` on its own SAM connection, `accept_with_prefix`),
`librqbit-tracker-comms` (UDP skipping, HTTP client rebuild, `.i2p` trackers),
`librqbit-peer-protocol` (absent extension ids are not serialized).

Every change below applies without restarting the session.

### Settings

```rust
// SessionOptions (new fields; everything else unchanged)
pub net: NetSettings,                       // runtime: Session::set_net_settings
pub port_mapping: Option<PortMappingConfig>,// None = from ListenerOptions::enable_upnp_port_forwarding
pub i2p: Option<I2pSettings>,               // None = off

pub struct NetSettings {
    pub encryption: EncryptionPolicy,       // Enabled (default) | Forced | Disabled
    pub proxy: Option<ProxySettings>,
    pub anonymous_mode: bool,
    pub enable_tcp: bool, pub enable_utp: bool,   // outgoing protocols
    pub outgoing_ports: Option<(u16, u16)>,       // inclusive local port range
    pub peer_tos: u8,                             // IP ToS byte (DSCP << 2), 0 = OS default
    pub send_buffer_size: u32, pub recv_buffer_size: u32, // bytes, 0 = OS default
    pub connection_speed: u32,                    // outgoing attempts/s, 0 = unlimited
    pub pex: bool,
}
pub struct ProxySettings { pub config: ProxyConfig /* engine/proxy */, pub peer_connections: bool, pub tracker_connections: bool }

// Session
pub fn net_settings(&self) -> NetSettings;
pub fn set_net_settings(&self, s: NetSettings) -> anyhow::Result<()>;
pub fn set_encryption_policy(&self, EncryptionPolicy) -> anyhow::Result<()>;
pub fn set_proxy(&self, Option<ProxySettings>) -> anyhow::Result<()>;
pub fn set_anonymous_mode(&self, bool) -> anyhow::Result<()>;
pub fn set_pex_enabled(&self, bool) -> anyhow::Result<()>;
pub fn set_outgoing_protocols(&self, tcp: bool, utp: bool) -> anyhow::Result<()>;
pub fn set_outgoing_port_range(&self, Option<(u16, u16)>) -> anyhow::Result<()>;
pub fn set_peer_tos(&self, u8) -> anyhow::Result<()>;
pub fn set_socket_buffer_sizes(&self, send: u32, recv: u32) -> anyhow::Result<()>;
pub fn set_connection_speed(&self, per_second: u32) -> anyhow::Result<()>;
```

`ConnectionOptions::proxy_url` still works (`socks5://`, `socks5h://`,
`socks4://`, `socks4a://`, `http://`, credentials percent-decoded) and means
"peers and trackers through this proxy" unless `net.proxy` is set;
`ConnectionOptions::enable_tcp = false` clears `net.enable_tcp`.

### Protocol encryption (MSE/PE)

* Outgoing (`StreamConnector::connect`, TCP, uTP and proxied): `Disabled` →
  plaintext; `Forced` → MSE handshake (RC4 only, like qBittorrent's
  `allowed_enc_level = pe_rc4`) or no connection; `Enabled` → MSE first; if
  the encrypted handshake fails (or times out after the read/write timeout)
  the peer is reconnected in plaintext and remembered (bounded set of 4096,
  cleared when the policy changes), so the next attempt is plaintext
  directly. This is libtorrent's `pe_enabled` outgoing behaviour (it also
  retries a failed encrypted connection in plaintext and marks the peer).
* Incoming: unless `Disabled`, `librqbit_mse::accept` tells plaintext from
  encrypted handshakes (plaintext is refused with `Forced`) and identifies the
  torrent through `HASH('req2', info_hash)` → info hash, a map the session keeps
  up to date on add/delete (`Req2Index`). The encrypted info hash must match the
  BitTorrent handshake. With `Disabled` encrypted connections fail the
  plaintext handshake parse and are dropped.
* After the handshake the split read/write halves are wrapped
  (`net/encryption.rs`): reads replay bytes read ahead during the handshake
  and decrypt (vectored reads included); writes encrypt each chunk once and
  report it written only when all ciphertext reached the socket, so nothing is
  left buffered between messages.
* `PeerFlags::encrypted` (qBittorrent "E") is set for RC4 connections on both
  sides. `PeerFlags::encrypted_handshake` ("e", obfuscated handshake with
  plaintext payload) exists but is never set, since only RC4 is negotiated.

### Proxies

* `peer_connections`: outgoing peer connections go through
  `librqbit_proxy::connect` (HTTP CONNECT with Basic auth, SOCKS4/4a, SOCKS5
  with user/password; `remote_dns` = hostname lookup on the proxy), shown as
  `ConnectionKind::Socks`; uTP is not used, incoming connections are refused
  (libtorrent `proxy_peer_connections`; I2P streams excepted), our listen port
  is not sent in the extended handshake, and web seeds are fetched through the
  proxy. Encryption works through the tunnel.
* `tracker_connections`: HTTP(S) trackers use a `reqwest` proxy (HTTP,
  SOCKS4/4a, SOCKS5/5h, credentials); the tracker HTTP client is rebuilt on
  every proxy change. UDP trackers are skipped ("Unreachable": "UDP trackers
  are not used through the proxy") because no proxy relays UDP here (no SOCKS5
  UDP ASSOCIATE). Proxied trackers keep the existing SSRF rule for proxied
  host names (only literal addresses can be checked).
* The torrent-download `reqwest` client (`AddTorrent::Url`, block lists) uses
  the tracker proxy as configured at session start.

### Anonymous mode

`NetSettings::anonymous_mode` (libtorrent `anonymous_mode`):

* new connections and announces use a second peer id with the generic
  libtorrent prefix `-LT2000-` + random bytes (both ids count as "ourselves");
* the extended handshake carries no `v` (client name) and no `p` (listen
  port); otherwise `p` is now sent on outgoing connections (libtorrent does);
  `yourip` is kept (it is the peer's address), and never sent to I2P peers;
* the tracker side switches with it (`TrackerSettings::anonymous_mode`: no
  `User-Agent` except for private torrents, no `ip=`), and the tracker peer id
  changes too. Setting `TrackerSettings::anonymous_mode` alone only affects
  trackers.

### Listeners, interface, protocols

```rust
pub fn listen_options(&self) -> Option<ListenerOptions>;
pub async fn set_listen_options(self: &Arc<Self>, Option<ListenerOptions>) -> anyhow::Result<()>; // None = stop listening
pub async fn set_listen_port(self: &Arc<Self>, port: u16) -> anyhow::Result<()>;
pub async fn set_listen_ip(self: &Arc<Self>, ip: IpAddr) -> anyhow::Result<()>;
pub async fn set_network_interface(self: &Arc<Self>, Option<String>) -> anyhow::Result<()>;
pub fn listen_addr(&self) -> Option<SocketAddr>;   // now dynamic
pub fn announce_port(&self) -> Option<u16>;        // now dynamic
```

* Rebinding stops the old listeners, binds the new ones (retrying for up to
  1 s while the old socket on the same port is released) and, if that fails,
  restores the old ones and returns the error. Existing TCP connections stay
  up; uTP connections of the old socket die with it, and outgoing uTP uses the
  new socket. The TCP/uTP listener mode is `ListenerOptions::mode`; outgoing
  protocols are `set_outgoing_protocols`.
* Afterwards the tracker announce port, the extended-handshake `p`, the port
  mappings and the DHT/LSD announces follow the new port, and every torrent
  reannounces.
* `set_network_interface` binds outgoing peer sockets, listeners (rebinding),
  DHT and LSD (restarted), port mapping and the tracker / web seed HTTP
  clients (rebuilt) to the interface. The UDP tracker socket keeps the
  interface it was created with (limitation).
* Socket tuning (ToS/traffic class, `SO_SNDBUF`, `SO_RCVBUF`) applies to
  outgoing TCP sockets and accepted TCP sockets; uTP sockets are not tuned.
  The outgoing port range binds outgoing TCP sockets to a random port of the
  range (`SO_REUSEADDR`, up to 32 ports tried per connection).

### DHT, LSD, PeX, connection speed

```rust
pub fn dht_enabled(&self) -> bool;
pub async fn set_dht_enabled(self: &Arc<Self>, bool) -> anyhow::Result<()>;
pub fn lsd_enabled(&self) -> bool;
pub async fn set_lsd_enabled(self: &Arc<Self>, bool) -> bool; // false if LSD can't start
pub fn get_dht(&self) -> Option<Dht>;   // was Option<&Dht>
```

* DHT restarts with the configuration the session was created with
  (`SessionOptions::dht`, or the defaults when it was `None`); with
  persistence the routing table and port come back. Stopping cancels the DHT
  tasks.
* Each torrent's DHT and LSD peer streams follow the session: they restart
  when DHT/LSD are switched or the announce port changes (a session-wide
  generation counter), so running torrents start or stop using them at once.
  Private torrents and I2P-only torrents never use them.
* PeX off: `ut_pex` is not advertised in new extended handshakes, received
  PeX messages are ignored and the per-peer PeX tasks send nothing (they keep
  running, so switching PeX on again resumes sending to existing peers).
  Synthetic I2P addresses are never sent in PeX.
* `connection_speed`: a session-wide pacer spaces outgoing connection attempts
  (`1 s / rate`), after the connection slots are acquired.
* Memory: a torrent's `get_peers` lookup is rounds of iterative requests. A
  round holds a slot of the DHT's lookup semaphore (32 slots; the first round
  of each torrent has 8 separate slots so a new torrent is not queued behind
  the cycling ones), keeps at most 8 requests in flight and drops its
  candidate list when it ends, so hundreds of torrents do not hold hundreds
  of lookups' state. Empty rounds (no nodes yet, or no IPv6) back off from 1 s
  to 8 s.

### Session persistence and memory

`SessionPersistenceConfig::FastResumeOnly { folder }` keeps only the
fast-resume bitfields (`<hash>.bitv`) in `folder`: no `session.json`, no
`.torrent` copies, and no per-torrent file names in the store's memory (the
embedder re-adds its torrents itself, as the core does). `Json` is unchanged.
Other per-torrent savings: the per-piece locks are striped (128 at most; the
stealing check only try-locks, so a shared stripe merely makes two pieces
unstealable together), the peer and stream maps use 4 and 2 shards instead of
dashmap's 4 per CPU, and the speed estimator samples idle torrents once a
second instead of ten times.

Fast-resume validation by file size and mtime (`torrent_state/resume_stats.rs`):
next to `<hash>.bitv` the JSON store keeps `<hash>.fstat` (own versioned
binary format: per file length + mtime, plus the have-bitfield snapshot taken
right after the files were stat'ed). It is written after each initial check
(unless nothing changed), on `Session::pause` and for torrents paused by
`Session::stop`, and removed by `clear` (force recheck, errors) and delete. On
start, pieces of files whose size and mtime are unchanged are kept without
reading (if both the loaded bitfield and the snapshot have them); pieces
touching a changed file are hashed, those touching a missing file dropped,
and a corrected bitfield is stored. Files modified less than 2 s before being
recorded are stored as uncertain (coarse timestamps). Without a `.fstat`
(older data, imported bitfields, skip hash check, non-disk storages, the
Postgres store) the old sampling check runs; a sampled piece with a wrong hash
now invalidates the fast-resume data (it used to pass). The module docs
explain why stat-then-snapshot is safe while pieces are being written.

### Idle wakeups

An idle session (torrents seeding, no peers) arms no periodic timers in the
engine, so the process sleeps between the embedder's own ticks (16 seeding
torrents, DHT/LSD/UPnP off: about 2 wakeups/s for the whole headless core,
down from 17).

* `spawn_utils::spawn` has no "still running" trace timer (it was a 5 s
  interval in every engine task).
* The external peer adder of a torrent waits on its peer stream only; it is
  cancelled with the live state instead of polling every 5 s.
* The web seed scheduler of a finished torrent waits for `new_pieces_notify`
  (priority changes, streams on unwanted files via
  `reconnect_all_not_needed_peers`) with a 60 s safety net instead of a 5 s
  poll.
* The session speed estimator is sampled by the session peer tick (one 1 s
  timer instead of two). The peer tick parks without a timer when no peer
  is live and the session and every live torrent read zero speed with their
  samplers parked; a peer going live (outgoing or incoming), web seed data
  or a policy change wakes it, and the speed windows restart then
  (`SpeedEstimator::reset`) so the first samples are not averaged over the
  parked time. Test: `qbt-e2e/tests/idle_seed.rs`.

### Per-torrent DHT/PeX/LSD and private torrents

```rust
pub struct DisabledDiscovery { pub dht: bool, pub pex: bool, pub lsd: bool } // peer_source.rs
AddTorrentOptions::disabled_discovery: DisabledDiscovery   // serde default: all on
pub fn disabled_discovery(&self) -> DisabledDiscovery;      // ManagedTorrent
pub fn set_disabled_discovery(&self, DisabledDiscovery);    // live
```

libtorrent's `disable_dht` / `disable_pex` / `disable_lsd` torrent flags,
which qBittorrent's Torrent Options dialog sets ("Disable DHT/PeX/LSD for
this torrent"; there is no WebAPI for them). The switches live in the
torrent's `TorrentTrackers` (a `watch` channel, so they survive pause and
apply before the first peer stream is built, magnet metadata fetches
included):

* DHT/LSD: each discovery stream (`switching_discovery_stream` in
  `session/networking.rs`) watches the torrent's switch next to the session
  generation; switching a source off drops the running lookup/announce at
  once, switching it on starts a new one.
* PeX: the per-torrent switch joins the session switch in
  `TorrentStateLive::pex_enabled` — nothing is sent or accepted, and new
  connections don't offer `ut_pex` in the extended handshake.

Private torrents (BEP 27) never use DHT, LSD or PeX, whatever the switches
say: no DHT/LSD streams are created for them (a magnet whose metadata turns
out private stops them through `wait_private`), received PeX messages are
dropped, the PeX sender is never spawned, and — new — `ut_pex` is no longer
offered in their extended handshake. `ut_metadata` requests for private
torrents are refused (upstream).

Magnet metadata downloads (`peer_info_reader`) are memory-bounded: the
peer's `metadata_size` (≤ 32 MiB) allocates nothing up front, pieces are
kept as they arrive, each charged to the session's `FetchBudget` (shared
with piece layer fetches) after its index and size are checked, the info
hash is computed over the pieces once all arrived, and at most
`MAX_METADATA_FETCHES_PER_TORRENT` (2) of the peers tried for a magnet
download at once: the others wait for a slot before requesting anything
(`PeerConnectionHandler::after_extended_handshake`). See
`docs/SECURITY.md`. Tests: `discovery_tests` (stream switching),
`trackers::tests::disabled_discovery_switches`,
`e2e_net::test_e2e_net_pex_disabled_for_torrent`,
`e2e_net::test_e2e_net_private_torrent_never_uses_pex`.

### IP filter, bans, blocked-peer log

```rust
pub fn set_peer_ip_filter(&self, Option<IpFilterFn>);     // Fn(IpAddr) -> bool, true = blocked
pub fn set_banned_ips(&self, Vec<IpAddr>);
pub fn ban_peer_ip(&self, IpAddr);
pub fn unban_peer_ip(&self, IpAddr) -> bool;
pub fn banned_ips(&self) -> Vec<IpAddr>;
pub fn set_blocked_peer_callback(&self, Option<BlockedPeerCallback>); // Arc<dyn Fn(BlockedPeer)>
pub fn disconnect_blocked_peers(&self) -> usize;
// BlockedPeer { addr, reason: PeerBlockReason, incoming }
// PeerBlockReason: IpFilter | Banned | I2pMixedMode (non_exhaustive)
```

* Checked for incoming connections right after accept (before any
  handshake), for outgoing ones in the peer adder (before a connection slot is
  taken), with IPv4-mapped IPv6 addresses checked as IPv4. Every refusal is
  reported to the callback (the core's "Blocked IPs" log) and counted in
  `blocked_incoming` / `blocked_outgoing`.
* Changing the filter or the bans disconnects matching connected (and
  connecting) peers of every live torrent at once, reported with `incoming` as
  the connection direction; they are not reconnected.
* The filter is separate from `set_tracker_ip_filter` (the core sets both
  when "Apply to trackers" is on) and from the URL block/allow lists.

### Port mapping (UPnP + NAT-PMP)

```rust
pub struct PortMappingConfig { pub upnp: bool, pub natpmp: bool, pub natpmp_gateway: Option<SocketAddr>, pub lease_secs: u32, pub description: Option<String> }
pub fn set_port_mapping(&self, PortMappingConfig);
pub fn port_mapping_config(&self) -> PortMappingConfig;
pub fn add_port_mapping(&self, MappingProtocol, port: u16) -> bool;    // WebUI, embedded tracker
pub fn remove_port_mapping(&self, MappingProtocol, port: u16) -> bool;
pub fn port_mapping_status(&self) -> PortMappingStatus;
// PortMappingStatus { upnp, natpmp, requested: Vec<(MappingProtocol, u16)>, mappings: Vec<PortMapping>,
//                     external_ip: Option<IpAddr>, natpmp_error, upnp_error }
// PortMapping { transport: NatPmp | Upnp, protocol: Tcp | Udp, internal_port, external_port, lifetime_secs }
```

* The listen port is mapped for TCP (TCP listener) and UDP (uTP listener),
  plus the extra ports. Any change (ports, config, interface) restarts the
  mappers: NAT-PMP deletes its old mappings (lifetime 0) before exiting, UPnP
  leases simply expire. NAT-PMP renews at half the lease and reports the
  gateway's external address; UPnP reports each successful
  `AddPortMapping` and, at the same interval, the gateway's
  `GetExternalIPAddress` answer. `ListenerOptions::enable_upnp_port_forwarding` maps to
  both UPnP and NAT-PMP (qBittorrent has one switch); `set_listen_options` with
  a different value switches both.

### External address

```rust
pub fn external_ips(&self) -> (Option<IpAddr>, Option<IpAddr>);   // voted IPv4, IPv6
```

* Like libtorrent's `ip_voter`: the NAT-PMP / UPnP gateway, trackers (BEP 24
  `external ip`) and peers (BEP 10 `yourip`) vote (`net/external_ip.rs`).
  Each source (tracker host, peer IP) holds one vote per family, so repeating
  itself does not help; the last 32 votes per family are kept. Weights:
  gateway 4 (one slot, cleared when the mappers restart), tracker 2, peer 1;
  an address needs 2 to be elected and ties keep the current one, then
  prefer the gateway's.
* Non-public addresses never vote (private, loopback, link-local, CGNAT
  100.64/10, unique-local fc00::/7, documentation, reserved ...), from any
  source; nor do peers on our own network. `PortMappingStatus::external_ip`
  still shows the gateway's raw answer.
* Changing the network interface forgets all votes.

### I2P

```rust
pub struct I2pSettings { pub sam: SamOptions /* host, port, tunnel quantities/lengths */, pub mixed_mode: bool }
pub fn set_i2p(self: &Arc<Self>, Option<I2pSettings>);
pub fn i2p_settings(&self) -> Option<I2pSettings>;
pub fn i2p_status(&self) -> I2pStatus;   // { enabled, connected, destination, b32, error }
pub fn add_i2p_peers(&self, torrent: &ManagedTorrent, destinations: &[String]) -> usize;
pub fn i2p_peer_addr(&self, destination: &str) -> Option<SocketAddr>;
pub fn i2p_destination_of(&self, addr: SocketAddr) -> Option<String>;
```

* The SAM session (`SESSION CREATE STYLE=STREAM`, transient destination) is
  created in the background and recreated with backoff (5 s doubling to 5 min)
  when it fails or is lost.
* The engine's peer table is keyed by `SocketAddr`, so each I2P destination
  (base64, or a `*.i2p` / `*.b32.i2p` name resolved through SAM `NAMING
  LOOKUP` when connecting) gets a stable synthetic address
  `[100::1220:a:b:c]:1` (IPv6 discard prefix, RFC 6666). The book holds at
  most 65 536 destinations and then forgets the oldest; indices start at a
  random value per session and are never reused, so an address never names a
  different destination. Peer details show it with `ConnectionKind::I2p`;
  `i2p_destination_of` maps it back. Synthetic (100::/64) addresses are only
  accepted from `.i2p` trackers, incoming I2P streams and manual peers — never
  from clearnet trackers, DHT, LSD or PeX.
* Outgoing: the connector opens `STREAM CONNECT` for synthetic addresses
  (encryption policy applies as for TCP). Incoming: one `STREAM ACCEPT` at a
  time, at most 64 being checked at once; the accepted stream (with any bytes
  read ahead with the destination line) goes through the normal incoming path
  (filter, MSE, handshake). Our clearnet listen port is never sent to I2P
  peers (extended handshake `p`).
* `.i2p` HTTP trackers: the tracker client sends them through a loopback HTTP
  proxy that only connects to `*.i2p` hosts through SAM and requires a random
  per-session password. Announces carry `ip=<our destination>.i2p`;
  responses' peers are parsed as I2P peers (compact 32-byte hashes →
  `<b32>.b32.i2p`, or dictionaries with `ip=<destination>.i2p`) and become
  synthetic addresses. Without an I2P session `.i2p` trackers are "Unreachable"
  ("I2P is not enabled"); UDP `.i2p` trackers and scrapes over I2P are skipped.
* Mixed mode off: a torrent with an `.i2p` tracker only talks to I2P peers —
  other peers are refused both ways (reported as `I2pMixedMode`) — and uses
  no DHT/LSD, no clearnet trackers (announces and scrapes are skipped with
  "I2P mixed mode is off") and no web seeds. Mixed mode on: no restriction.
* Limitations: no I2P DHT or PeX for I2P peers (`ut_pex` carries IP addresses;
  libtorrent's i2p_pex is not implemented); one pending `STREAM ACCEPT`
  (incoming I2P connections are accepted one after the other); peer details show the
  synthetic address, not the destination (use `i2p_destination_of`).

### Web seeds

Code: `torrent_state/webseeds.rs` (list, status, API), `torrent_state/live/webseed.rs`
(scheduler), fetching/URL mapping in `engine/webseed`.

```rust
// ManagedTorrent (works in every state, applies to live torrents immediately)
pub fn web_seeds(&self) -> Vec<WebSeedStatus>;
pub fn web_seed_urls(&self) -> Vec<String>;            // for persistence
pub fn add_web_seeds(&self, urls: Vec<String>) -> usize;
pub fn remove_web_seeds(&self, urls: &[String]) -> usize;
pub fn edit_web_seed(&self, old: &str, new: &str) -> Result<(), WebSeedEditError>;
pub fn replace_web_seeds(&self, urls: Vec<String>);
// AddTorrentOptions
pub web_seeds: Option<Vec<String>>;          // extra, after the torrent's
pub replace_web_seeds: Option<Vec<String>>;  // exactly these (restore a persisted list)
```

`WebSeedStatus { url, state: WebSeedState, message: Option<String>, downloaded,
download_rate, hash_failures }`; `WebSeedState` is `Idle | Downloading | Backoff |
Banned | Error`; `WebSeedEditError` is `NotFound | InvalidUrl | AlreadyExists`.

* Sources: the torrent's `url-list` (a string or a list of strings) and magnet
  `ws` parameters (untrusted), then `AddTorrentOptions::web_seeds` and seeds added
  through the API (trusted). With `replace_web_seeds`, seeds are trusted when
  the torrent is from a local source or they are listed in
  `AddTorrentOptions::trusted_urls`. Only http/https URLs, at most 4096 bytes, at most 100
  seeds per torrent, duplicates skipped. Invalid torrent-supplied URLs are listed
  with state `Error`. The engine does not persist the list; the core stores
  `web_seed_urls()` and restores it with `replace_web_seeds`.
* Scheduling: one task per live torrent drives all its seeds, at most 4
  fetching at once per torrent and 16 per session. Each usable seed
  fetches one piece at a time (overall deadline 60 s + 1 s per 16 KiB), picked by the same picker as peers (stream
  read-ahead, priorities, rarest-first or sequential). In the piece tracker a seed
  is a pseudo-peer with the address `[100::7765:6273:<id>]:0` (IPv6 discard prefix
  100::/64, RFC 6666, never a real peer). It "has" every piece and never steals.
  Peers may steal a slow seed's piece; the seed's data is then counted as wasted.
* Each 16 KiB block of the piece passes the torrent's and (unless exempted) the
  session's download limiter before the HTTP fetch (`fetch_piece`: Range GETs
  across files, pad files zero-filled, bounded bodies). Blocks are written through
  the storage like peer blocks, then the piece is hash-checked. On success the same
  counters as for peers are updated (fetched, downloaded-and-checked, have,
  session fetched bytes), then `on_piece_completed` and HAVE to peers. A piece
  that fails the check is re-queued and counted as wasted (failed); the seed is
  banned after 3 such pieces.
* Errors (network, HTTP status, short body) give the piece back and back the seed
  off for 30 s, doubling up to 1 h. A 503 `Retry-After` (5 s–1 h) is honoured.
  "Nothing to download" re-checks after 1 s.
* The scheduler idles when the torrent is finished (re-checking every 5 s, so a
  priority change resumes it) or has no seeds (it then waits for list changes).
  Pieces held by a removed or edited seed, or by a paused torrent, go back to the
  queue.
* HTTP clients: two session-wide reqwest clients (`Session::webseed_http_client`,
  trusted and untrusted) using the peer proxy (when "proxy peer connections" is
  on) or the bound interface, 15 s connect / 30 s read timeouts, at most 5
  redirects, http/https redirects only, no `User-Agent` in anonymous mode. They
  are rebuilt when the proxy, the interface or anonymous mode changes.
* SSRF mitigation: a torrent-supplied (untrusted) seed whose host is a literal
  loopback, private, link-local, unique-local or CGNAT address (or `localhost`)
  and whose URL has a query string is never contacted (state `Error`). Untrusted
  seeds also use a client whose resolver drops local addresses and whose
  redirects never lead to a local address or `localhost` (unless the seed is
  itself local and the target has no query). User-added seeds are trusted. Seeds
  on a literal address blocked by the IP filter are not contacted; dot segments
  (`.`, `..`) in the torrent's paths are refused; bytes of a full-file reply
  before the wanted range are discarded while streaming. Logs never contain
  seed URLs.

Limitations: one piece in flight per seed (libtorrent pipelines several
requests); `download_rate` is per verified piece over 5 s, so it is bursty; seeds
are not shown in `peers_detailed`; the engine's JSON session persistence does not
store the list.

Tests: unit tests (`url-list` string/list/garbage, magnet `ws`, SSRF rule,
add/edit/remove/duplicates, ban after 3 hash failures, backoff, pseudo-address
uniqueness). `cargo test -p librqbit e2e_webseed` runs a leecher without peers
against a local Range-capable HTTP server: it completes a 3-file torrent with
pieces crossing files, re-fetches a piece corrupted once (`hash_failures == 1`,
wasted ≥ 1 piece, byte-identical files, add/edit/remove at runtime), and bans a
seed that keeps corrupting a piece (`Banned`, 3 failures, torrent not finished).

### Tests

`cargo test -p librqbit net::` (proxy URL parsing, settings validation,
reqwest proxies for every kind, MSE split halves over a small duplex in both
directions incl. vectored reads and partial writes, plaintext prefix replay,
outgoing port range + ToS, IP filter/bans/mapped addresses/callback, connection
pacing), `i2p::` and `i2p_http::` (synthetic addresses, destination
validation, proxy request parsing and clearnet refusal), tracker-comms
(`parse_i2p_peers`, UDP skipping), and `cargo test -p librqbit e2e_net` (local
sessions): encrypted transfer with `Forced` on both sides (E flag on both
ends), `Disabled`↔`Enabled` interop both ways, `Enabled`↔`Enabled`, `Forced`
vs `Disabled` never connecting and a runtime switch to `Forced`; a transfer
through a local HTTP CONNECT proxy (encrypted through the tunnel); anonymous
mode (`-LT2000-` id, no `v`, tracker side); live listen port change (old port
closed, new leecher downloads from the new port, mode change, stop
listening); PeX off then switched on at runtime (a third peer is learned only
after); IP filter disconnect + refused reconnects + bans + outgoing blocks;
NAT-PMP mappings against a fake gateway (listen port, extra port, port change,
switch off); an I2P swarm through a fake SAM bridge with an `.i2p` HTTP
tracker, mixed mode off refusing the clearnet address.

## Pure v2 torrents (BEP 52)

Pure v2 torrents (`meta version` 2, `file tree` + `piece layers`, no v1
`pieces`) download and seed from .torrent files and from `btmh` magnet
links. Hybrid torrents run on their v1 half (SHA-1 pieces, v1 file list
with BEP 47 pad files, v1 info hash as the engine's identity) but, like in
libtorrent, are announced, reachable and served under their v2 hash too
(see "Hybrid torrents: both hashes"). Every v1 code path (piece picker,
chunk tracker, storage, resume bitfields) is reused by mapping a v2 torrent
onto a v1-shaped layout. Interoperability with libtorrent 2.1 is checked by
`scripts/interop/libtorrent_interop.py` (see "Interoperability with
libtorrent").

Code: `librqbit_core/src/v2.rs` (file tree, piece layers, merkle hashing and
proofs), `torrent_metainfo.rs` (parsing, validation, identity),
`librqbit/src/file_ops.rs` (verification), `peer_binary_protocol` (hash
messages, v2 handshake bit), `peer_info_reader/v2_layers.rs` (magnet layer
fetch), `torrent_state/live/v2_hashes.rs` (serving hash requests, fetching
missing layers while running), `torrent_state/missing_layers.rs` and
`torrent_state/live/v2_missing.rs` (pure v2 files waiting for a layer).

### Metainfo and identity

```rust
// TorrentMetaV1Info: pieces may be empty; new fields
pub meta_version: Option<u32>,                   // "meta version"
pub file_tree: Option<RawBencode<BufType>>,      // raw "file tree", parsed with limits
// TorrentMetaV1
pub piece_layers: Option<RawBencode<BufType>>,   // raw "piece layers"
pub info_hash: Id20,                             // wire identity (see below)
pub info_hash_v2: Option<Id32>,                  // SHA-256 of the info dict (v2 + hybrid)
// ValidatedTorrentMetaV1Info
pub fn is_pure_v2(&self) -> bool;
pub fn is_single_file(&self) -> bool;            // v1 `files` absent, or one top-level v2 file
pub fn with_piece_layers(self, layers: Option<&[u8]>) -> Result<Self>;
pub fn piece_check(&self, piece: u32) -> Option<PieceCheck>; // V1(sha1) | V2{data_len, leaves, expected}
pub fn v2_layout(&self) -> Option<&V2Layout>;
pub fn v2_piece_hashes(&self) -> Option<&[Hash32]>;
```

* `torrent_from_bytes` hashes the info dict with SHA-256 whenever `meta
  version` is 2. A pure v2 torrent's `info_hash` (handshake, trackers, DHT,
  LSD, PeX, MSE, resume names, `TorrentIdOrHash::Hash`) is that hash
  truncated to 20 bytes; hybrids keep the SHA-1 hash.
* `meta version` other than 1/2 is refused. `validate()` on a pure v2 info
  dict parses the file tree; `with_piece_layers` (called for every added
  .torrent, by the core's `parse_torrent` and after a magnet's layers are
  fetched) checks the piece layers and keeps one expected hash per piece.
  Layers that are present must be valid (else the torrent is refused);
  missing ones are fetched from peers while the torrent runs, like
  libtorrent does (see "Missing piece layers"; `missing_piece_layers`,
  `with_partial_piece_layers`, `v2::V2PieceHashes`).
* File tree rules: keys are path components (no empty, `.`, `..`, `/`, `\`
  or NUL); a file is `name: {"": {length, pieces root}}` with nothing else in
  that dict; non-empty files need a 32-byte `pieces root`; `attr` `x`/`h` are
  kept (symlinks and `p` inside a v2 tree are treated as plain files);
  piece length a power of two ≥ 16 KiB. Keys are walked in sorted order
  whatever the encoding order.

### Layout: v2 as v1 with virtual padding

`parse_file_tree` builds the engine's file list in file-tree order and puts a
virtual pad file (`.pad/<n>`, `FileDetailsAttrs::padding`) after every
non-empty file that doesn't end on a piece boundary *and is followed by more
data* (no tail padding: the last piece of the torrent is short, as in a v1
torrent). Each piece belongs to exactly one real file (`V2Layout::spans`,
binary search by piece). Storage already reads zeros for and drops writes to
padding, so `Lengths`, the picker, storage, resume and web seeds work
unchanged; pad files are hidden by the core like BEP 47 pads. Peers compute
the same piece count; whether a peer pads the tail doesn't matter on the
wire. libtorrent never requests padding: the last block of a file's data in
a piece is requested cut at the end of the data (e.g. 3616 bytes), which
`requested_chunk` accepts for pure v2 torrents (only that block, only ending
exactly at the piece's data length) and serves as asked. Our own requests
are whole blocks (padding included), which libtorrent serves.

### Verification

`file_ops::PieceHasher` replaces the SHA-1 calls of the initial check,
`check_piece` (downloads, web seeds, skip-hash-check reads):

* v1/hybrid: SHA-1 over the whole piece, padding included (as before).
* pure v2: SHA-256 of each 16 KiB block of the *real* file data (a short last
  block hashed as is, padding never hashed), folded into the merkle root over
  `piece length / 16 KiB` leaves (missing leaves are zero hashes) and
  compared with the file's piece-layer entry. A file no longer than a piece
  is compared with its `pieces root`, over leaves padded only to the next
  power of two of its block count. The fold keeps one pending hash per tree
  level (`MerkleHasher`), whatever the piece size; the 64 KiB read buffer is
  unchanged.

### Wire protocol

* Our handshake sets the v2 bit (reserved byte 7, `0x10`) for pure v2
  torrents, v2 magnets and every connection of a hybrid torrent
  (`PeerConnectionHandler::handshake_v2`). On a hybrid's v1 connection the
  bit offers libtorrent's upgrade to v2 (see "Hybrid torrents: both
  hashes").
* BEP 52 messages 21/22/23 (`Message::HashRequest`, `Hashes`, `HashReject`):
  fixed 48-byte header, `hashes` payload a multiple of 32 bytes and bounded by
  the 32 KiB read buffer; `MAX_MSG_LEN` grew to fit our largest reply (512
  base hashes + 32 proof hashes, ~17 KiB).
* Proof layers are counted like libtorrent (the reference implementation;
  BEP 52 leaves it open): `proof_layers` counts the layers above the base
  layer, and the first log2(length) − 1 of them lie inside the subtree the
  requested hashes span, so a reply carries `max(0, proof_layers −
  log2(length) + 1)` uncle hashes (`v2::proof_uncles`; a proof to the root
  is requested with `proof_layers = height − 1`, `v2::proof_layers_to_root`).
  libtorrent's own chunking is accepted: 512 hashes, the last chunk of a
  file rounded up to a power of two (possibly 1).
* Serving (live pure v2 and hybrid torrents): requests for the piece layer
  of a known file are answered with the base hashes (pad hashes past the
  end of the file) and the uncle hashes; any other layer (libtorrent asks
  for block hashes after a failed piece), file or malformed request (length
  not a power of two in 1..=512, unaligned index, past the padded width,
  `proof_layers` ≥ the tree height) gets `hash reject`. A hybrid torrent's
  v2 file layout and piece layers are parsed from its .torrent on first use
  (a hybrid added by a magnet link fetches them, see "Missing piece
  layers"; until then its requests are rejected). `LayerTree` keeps a
  file's tree only from level 9 (512-hash subtrees) up, about 1/256 of the
  layer, built once per file off the async thread; a request then costs at
  most a few hundred hashes. At most 8 replies are queued per peer; further
  requests are dropped. Received `hashes`/`hash reject` on a live torrent
  are used only by a hybrid's layer fetch (from the one peer fetching),
  ignored otherwise.
* Magnets: `Magnet::parse` accepts `xt=urn:btmh:1220…`. With only a v2 hash
  the torrent's id is the truncated hash; the BEP 9 metadata is checked with
  SHA-256 against the full hash. Then, on the same connection, the piece
  layer of every file longer than a piece (identical files once) is
  requested in chunks of at most 512 hashes with proof layers up to the
  pieces root, at most 4 requests in flight. Every reply must match a
  request we sent and verify against the pieces root (`verify_layer_hashes`);
  a reject, an unsolicited or a bad reply drops the peer and another one is
  tried. The resulting `piece layers` are added to the torrent bytes the
  engine builds for the magnet (so resume and export keep them). A `btmh`
  magnet whose metadata is a hybrid torrent needs no layers to run: once
  verified against the v2 hash it runs on its v1 hash like any hybrid (and
  fetches its layers while running). Magnets with both hashes use the v1
  path.

### Missing piece layers

libtorrent accepts a v2 .torrent without (some of) its `piece layers`,
starts it at once and fetches the missing layers from peers with hash
requests; a file is verified (and downloaded) once its layer is known.
The engine does the same. Memory is bounded for every layer fetch
(magnet, .torrent and hybrid alike): requests are generated lazily,
verified chunks are kept as they arrive, and all of it is charged to one
session-wide `FetchBudget` (192 MiB, shared with BEP 9 metadata
downloads, `Session::fetch_budget_used`); a fetch that would exceed it
fails instead of allocating. A magnet's metadata connections fetch with
at most `MAX_LAYER_FETCHES_PER_TORRENT` (2) peers at once (the others
wait for a slot, `PeerConnectionHandler::after_extended_handshake` is
async for this); a live torrent fetches from one peer at a time. See
`docs/SECURITY.md`.

* Pure v2 .torrent (`torrent_state::missing_layers`,
  `live::{v2_hashes,v2_missing}`): `add_torrent` validates the layers that
  are present (`with_partial_piece_layers`; a bad one refuses the
  torrent) and starts the torrent like any other.
  `v2::V2PieceHashes` holds the piece hash table with one "known" bit per
  file; a file's hashes are added in place once its whole layer verified
  against its `pieces root` (`set_layer`, identical files together) and
  never change afterwards.
  * Picking: the pieces of files without a layer are held back
    (`TorrentStateLocked::v2_blocked`, folded into the picker's "peer has
    it" test, so peers, steals, streaming read-ahead and web seeds all
    skip them) without touching file priorities or the wanted/needed
    accounting: the torrent isn't finished while they are missing, and
    sequential mode, first/last piece and priorities 0..7 apply to the
    other files as usual. A block of such a piece is never requested, so
    one sent anyway is unsolicited and drops the peer (libtorrent
    downloads such pieces and keeps them "finished but not verified" until
    the hashes arrive; never requesting them gives the same result for a
    complete download without ever holding unverifiable data).
  * Fetching (`V2HashServer`): every v2 connection's messages may start a
    fetch when none is running (one peer at a time; a reject, a stall of
    60 s or a disconnect hands it to the next peer, a bad reply drops the
    peer, failed peers aren't asked again). The layers of files with
    priority above 0 come first, then the others (so the .torrent ends up
    complete and the layers can be served). Each file's layer is taken as
    soon as it is whole (`LayerFetch::take_complete`, its memory goes back
    to the budget), so a fetch that fails later keeps the files it
    completed. Known layers are served to hash requests, others rejected.
  * Unblocking (`TorrentStateLive::on_v2_layers_known`): pieces of that
    file with data on disk (see below) are hash-checked first, in a task
    of their own (`block_in_place_with_semaphore`, ended with the
    torrent), then every piece of the file goes to the picker: those that
    matched are had (HAVE sent), the others are downloaded.
  * Checking (`file_ops`): pieces of a file without a layer never count as
    had (initial check, recheck, skip hash check, and fast-resume bits are
    taken out). Their merkle roots are computed while reading: when a
    whole file on disk hashes up to its `pieces root` it yields the layer
    (as libtorrent's check does) and the file is verified at once
    (`LayerFinder`; on resume, files that fast-resume data says are
    complete are hashed the same way, `FileOps::find_layers`). Pieces with
    other data (not all zeros) are remembered
    (`ManagedTorrentShared::v2_unverified`) and checked once the layer
    arrives instead of downloaded; layers that became known while the
    torrent wasn't live are handled the same way when it starts.
  * Persistence: `ManagedTorrent::take_fetched_piece_layers` hands out
    every layer known so far after each change (fetched or found on disk)
    for the owner to save in its .torrent (`torrent_with_piece_layers`:
    every other key keeps its raw bytes); `ManagedTorrent::torrent_bytes`
    gives the .torrent with them (the engine's own persistence uses it).
    `missing_piece_layers` counts the files still without one, and
    `add_piece_layers` takes layers from another copy of the .torrent
    (each checked against its root; all or nothing).
  * `list_only` returns the .torrent as given (no fetch). Unlike a
    magnet's, the add needs no peers: the torrent waits for them.
* Hybrid added by a magnet link (`live::v2_hashes`): it runs on SHA-1 at
  once; while running, the first v2 connection (made with the v2 hash or
  upgraded) whose peer sends a message fetches every layer (one peer at a
  time; a reject, a stall of 60 s or a disconnect hands it to the next
  peer, a bad reply drops the peer). Once complete the layers are checked
  as a whole and served to hash requests, kept in
  `ManagedTorrentShared::v2_fetched_layers` across pause/resume, and
  handed out once by `ManagedTorrent::take_fetched_piece_layers` for the
  owner to save in its .torrent. libtorrent asks for a hybrid's layers on
  its v2 connections and retries rejects, so without this a hybrid added
  by magnet couldn't help libtorrent peers that lack layers.

### Hybrid torrents: both hashes

libtorrent announces a hybrid torrent under its v1 hash and under its v2
hash truncated to 20 bytes, accepts connections under either, and connects
to peers found under the v2 hash with that hash (`protocol_v2` peers). The
engine does the same:

* `ManagedTorrentShared::info_hash_alt` (`ManagedTorrent::info_hash_alt`,
  `has_info_hash`) is the truncated v2 hash of a hybrid, computed when the
  torrent is added (`hybrid_hashes`; a `btmh` magnet of a hybrid switches to
  the v1 hash as `info_hash` once the metadata is verified).
* Incoming: the session finds torrents by either hash (plain and MSE: both
  hashes are in the `req2` index); the connection is answered with the hash
  the peer used, plus the v2 bit when that is the v2 hash.
* Trackers: each announce job of a hybrid announces both hashes concurrently
  with the same event and stats (the tracker id belongs to the main
  announce). The tracker's status follows the v1 announce, or the v2 one
  when only that worked; scrapes use the v1 hash. No extra task: both run in
  the existing per-torrent announce job.
* DHT and LSD: one more `get_peers`/LSD announce stream per hybrid torrent
  for the v2 hash (as libtorrent), started with the others and following the
  same session switches, private flag and per-torrent DHT/LSD switches.
* Peers learned under the v2 hash (trackers, DHT, LSD, a `btmh` magnet's
  discovery) are remembered in the torrent's bounded peer source registry
  (`PeerSourceRegistry::mark_v2`, same 16 Ki-address cap as the sources,
  cleared on restart) and connected to with the v2 hash and the v2 bit;
  every other peer with the v1 hash. PeX carries no v2 flag (libtorrent
  strips `pex_lt_v2` too).
* Hash requests on hybrids are served from the .torrent's piece layers (see
  above): libtorrent asks for them on v2 connections and retries forever
  when they are rejected. A hybrid added by a magnet link fetches them
  first (see "Missing piece layers").
* Connection upgrade (libtorrent's `protocol_v2` for v1 connections): our
  handshake on a hybrid's v1 connection carries the v2 bit. An incoming
  handshake with our v1 hash and the v2 bit is answered with the truncated
  v2 hash (and the bit), and the connection is a v2 one from then on; an
  outgoing one may be answered with the v2 hash, which is accepted
  (`PeerConnectionHandler::accepts_other_info_hash`) and marks the
  connection v2 (`PeerHandler::v2_hash`). Without the bit the connection
  stays v1, answered with the hash the peer used. libtorrent 2.1.1 does
  the same, with one limit seen in testing: when it accepts an MSE
  encrypted connection, the hash the encryption was keyed with (`req2`)
  decides and the bit is ignored, so only plaintext v1 connections to
  libtorrent are upgraded (connections from libtorrent to us are upgraded
  either way). Hash requests are only made on v2 connections (libtorrent
  disconnects a v1 peer that sends one).

### Core

* `metainfo::parse_torrent` accepts pure v2, validates the piece layers, and
  fills `TorrentMeta::info_hash_v2` (v2 and hybrid). The torrent id is
  qBittorrent's `TorrentID` (libtorrent `info_hash_t::get_best()`): the v2
  hash truncated to 20 bytes whenever the torrent has one (pure v2 and
  hybrid), the v1 hash otherwise; magnet links with a `btmh` hash use the
  same rule. `TorrentMeta::info_hash` stays the engine's identity (v1 hash of
  a hybrid; engine fast-resume files are named after it).
* A hybrid added by its `btih` magnet is known under its v1 hash until the
  metadata arrives, then takes its v2-based id
  (`SessionImpl::handleTorrentInfoHashChanged`): the entry is re-keyed, the
  new `BT_backup/<id>.torrent` and `.fastresume` are written, then the old
  files removed. Duplicates are found under either hash
  (`SessionImpl::findTorrent`: a hybrid entry added by its v1 hash, or the
  hybrid whose v1 hash is a v1-only descriptor's id).
* Resume data saved by older versions under a hybrid's v1 hash (and records
  of magnet links with both hashes) move to the v2-based id when the
  session loads (`resume::migrate_id`: new files first, then the old ones
  go, so an interruption only repeats the migration); all other fields are
  kept.
* The WebAPI accepts torrent ids as qBittorrent does (`getTorrent(id)`:
  the id only, not the v1 hash of a hybrid); `torrents/add` and the
  metadata endpoints parse links with qBittorrent's id rule.
* "Info hash v1"/"Info hash v2" are filled in `TorrentInfo`, properties, the
  add-dialog metadata and the external-program variables (v1 empty for pure
  v2). Magnet links carry `btih` and/or `btmh`.
* `parse_magnet` accepts `btmh`-only links and bare 64-hex hashes; the v2
  hash (and the v1 hash of a link with both) is kept on the entry (and in
  the resume record) until the metadata arrives.
* Exported .torrent files and re-added torrents keep their `piece layers`.
* A pure v2 .torrent without (all of) its piece layers is accepted
  (`TorrentMeta::missing_piece_layers`) and goes to the engine like any
  other, which starts it at once and fetches the layers while it runs (see
  "Missing piece layers"). Re-adding a complete .torrent of it hands its
  layers to the engine (`ManagedTorrent::add_piece_layers`) and replaces
  `BT_backup/<id>.torrent`.
* Layers fetched while running (a hybrid's, all at once; a pure v2
  torrent's, as each file's arrives or is found on disk) are written into
  `BT_backup/<id>.torrent` on the next tick (`save_fetched_piece_layers`);
  a restart starts from what was saved (and the engine finds the layers
  of complete files on disk again).
* Piece hashes (WebAPI `pieceHashes`) of a pure v2 torrent are its SHA-256
  piece hashes (qBittorrent has no SHA-1 hashes to show there); empty for
  pieces whose layer isn't known yet.

### Limits (malicious torrents and peers)

File tree: ≤ 64 path components, ≤ 2^20 files (bencode nesting ≤ 128 is
enforced by the parser first); v2 file trees (pure v2, and the v2 half of
a hybrid when serving hashes) ≤ 2^21 pieces (the piece hash table, 32 bytes
per piece, is otherwise not bounded by the metadata size since identical
files share a layer); every layer's length must match its file and hash up
to its pieces root (each distinct root verified once). No `unwrap` on
attacker data: malformed input is an error. See `docs/SECURITY.md`.

### Interoperability with libtorrent

`scripts/interop/libtorrent_interop.py` (not run in CI) runs libtorrent
(Python bindings, `pip install libtorrent` in a throw-away venv) against
`crates/qbt-e2e/examples/interop_peer.rs` (one engine session) on
127.0.0.1 only, with DHT, LSD, UPnP, NAT-PMP and uTP off in libtorrent.
Torrents are made by libtorrent (`create_torrent`, v1-only, v2-only and
hybrid) at 16 KiB pieces (piece layer = block layer) and 64 KiB pieces
(4 blocks per piece), with a multi-piece file with a short last block, a
file of exactly one piece, sub-piece files, an empty file, nested folders,
a file of 520 pieces (several hash requests, the last one shorter) and an
unaligned last file. Per format: libtorrent seeds and we download from the
.torrent and from libtorrent's magnet link, and the other way round; for
hybrids also both directions of a `btmh`-only magnet, and three runs with a
small HTTP tracker that hands out peers only under the v2 hash (both sides
must announce both hashes and connect with the v2 hash). Pure v2: we
download from a .torrent without `piece layers` (fetched from libtorrent),
libtorrent downloads from us with such a .torrent (we serve the layers),
and a .torrent lacking the layer of the 520-piece file: we download the
other files from a libtorrent that has the same .torrent and a damaged
copy of that file (so it can't find or send the layer) until every
verifiable piece is had (`interop_peer` prints `PARTIAL`), then a second
libtorrent with the complete .torrent joins (`interop_peer --stdin-peers`)
and we fetch the layer (libtorrent logs our hash requests, no reject) and
the file from it. Hybrid: we download from the .torrent over a plaintext v1
connection and libtorrent must answer with the v2 hash (its peer log); we
download by a `btih` magnet, fetch the layers over the upgraded connection
(libtorrent logs our hash requests and no reject), then a second
libtorrent with the hybrid .torrent stripped of its layers downloads from
us alone: its v1 connection is upgraded by us and it gets `hashes` (no
`hash reject`) for its requests. `interop_peer --plaintext` turns MSE off,
and it prints `LAYERS` when a hybrid fetched its layers.

Results with libtorrent 2.1.1 (2026-10-16): all 44 scenarios pass (earlier
that day the first 40, 2026-10-15 the first 34). Seen on the way: like us,
libtorrent finds the layer of a complete file whose layer its .torrent
lacks by hashing it (the late-layer case damages one piece of its copy to
prevent that). Found
and fixed on the way: libtorrent counts BEP 52 proof layers differently
(uncles only above the requested subtree; our replies and requests used
"number of uncles", so multi-request layer fetches failed both ways);
libtorrent asks for the last block of a file's data in a pure v2 piece
without its padding (we rejected the request and dropped the peer, so
downloads from us stalled); libtorrent's `protocol_v2` connections to a
hybrid ask for piece layers and retry rejects forever (now served).
libtorrent 2.1.1 doesn't upgrade an incoming MSE connection (the `req2`
hash decides; our interop peer encrypts by default), so the upgrade case
runs in plaintext.

### Tests

`librqbit_core::v2` (layout and padding, malformed trees, streaming merkle vs
full tree, piece layers complete and partial, merging layer dictionaries,
proofs with libtorrent's proof layer counting), splicing layers into a
.torrent, peer protocol round trips, `v2_layers` (honest and hostile hash
replies, only the wanted files), `live::v2_hashes` (a hybrid's layer fetch:
one peer at a time, ignored, unsolicited, tampered and rejected replies,
completion, serving, pause; a pure v2 torrent's missing layers file by
file, wanted files first, files kept after a failed fetch, only known
layers served, budget returned), `v2::V2PieceHashes` (layers added one
file at a time, identical files together, bad layers refused), the
have bitfield losing unverifiable pieces,
the peer source registry's v2 marks, the core creator tests (the creator's
pure v2 torrents parse, map to the expected layout and every piece
verifies; tampered layers refused; core ids and hashes) and
`crates/qbt-e2e/tests/v2_swarm.rs`: two engine sessions transfer pure v2 and
hybrid multi-file torrents (pads, sub-piece files, empty file, corrupted
piece caught), a pure v2 torrent by `btmh` magnet with multi-request
layers, a hybrid by its `btmh` magnet, a hybrid swarm through a local
tracker that only hands out peers under the v2 hash (both hashes announced),
a hand-written peer that connects with a hybrid's v2 hash, requests piece
layers libtorrent's way and a block cut at the end of a v2 file's data, and
connects with the v1 hash with and without the v2 bit (upgraded or not), a
pure v2 .torrent with none or some of its layers (fetched from the seeder,
the .torrent handed out has every layer), a pure v2 .torrent lacking one
file's layer whose other files complete from a peer that can't send it
before a seeder with the layer joins (that peer then checks its damaged
copy from disk and downloads only the bad piece), resume and recheck with
a layer missing (fast-resume bits of unverifiable pieces dropped, a
complete file's layer found on disk, a layer added while paused checked
on resume), a bad layer refused, a hybrid by its
`btih` magnet that fetches its layers over the upgraded connection and then
answers hash requests, two cores with a layer-less pure v2 .torrent (layers
fetched while running, export has them; a complete .torrent added on top
completes another core's copy) and the hybrid `btih` core test (fetched layers
saved into the .torrent), and
two cores seed, download, export and re-add a pure v2 torrent, download it
by its magnet, and download a hybrid by its `btih` magnet (id switching to
the v2-based id). `crates/qbt-core/tests/session.rs` covers hybrid ids,
the id change, duplicates under either hash, the migration of old resume
data and engine fast-resume files surviving a restart.

## engine-trackers

Crates: `engine/tracker_comms` (protocol, policy, tier state machine) and
`engine/librqbit` (`torrent_state/trackers.rs`, `session_trackers.rs`,
`peer_source.rs`, tracker plumbing in `session.rs`).

### Per-torrent tracker list

Every torrent owns a `TorrentTrackers` manager (in `ManagedTorrentShared`, so
it survives pause/unpause). Its initial list is built at add time from:
the announce-list tiers (BEP 12; `announce` only without an announce-list),
magnet `tr` parameters (one tier each, like libtorrent),
`AddTorrentOptions::trackers` (one new tier after the torrent's),
`AddTorrentOptions::tracker_entries` (explicit `(url, tier)`) and, for public
torrents, `SessionOptions::trackers` (one more tier; dropped when magnet
metadata turns out private). `AddTorrentOptions::replace_trackers` replaces
all of that (restore a persisted list). Duplicates (by URL string) and
unparsable URLs are skipped; at most 1000 trackers per torrent. Unsupported
schemes (anything but http/https/udp) are listed but never contacted
("unsupported URL protocol", not retried).

`ManagedTorrent` API (changes apply immediately, also to live torrents):

```rust
fn trackers(&self) -> Vec<TrackerStatus>;              // announce order
fn tracker_list(&self) -> Vec<(String, u32)>;          // (url, tier), for persistence
fn add_trackers(&self, entries: Vec<(String, u32)>) -> usize;   // returns added
fn remove_trackers(&self, urls: &[String]) -> usize;
fn edit_tracker(&self, old: &str, new: &str) -> Result<(), TrackerEditError>; // keeps tier
fn replace_trackers(&self, entries: Vec<(String, u32)>); // kept URLs keep their status
fn force_reannounce(&self, url: Option<&str>) -> bool;  // ignores min interval
fn swarm_counts(&self) -> (Option<u32>, Option<u32>);   // max seeds/leechers over trackers
fn peer_source_counts(&self) -> PeerSourceCounts;       // connected peers per source
fn peer_sources(&self, addr: SocketAddr) -> PeerSources;
fn notify_completed(&self);                             // optional; completion is detected
fn is_private(&self) -> bool;
```

`TrackerEditError` is `NotFound | InvalidUrl | AlreadyExists`. The list is not
persisted by the engine's own session store beyond plain URLs; the core
persists `tracker_list()` and restores it with `replace_trackers`.

### Status

`TrackerStatus { url, tier, state, updating, message, peers, seeds, leechers,
downloaded, next_announce_in, min_announce_in, last_announce, endpoints }`;
`TrackerState` is `NotContacted | Working | NotWorking | TrackerError |
Unreachable` (qBittorrent `TrackerEndpointState`; `updating` is separate, as in
WebAPI 2.13). Counts are `None` when unknown (WebAPI `-1`).

* Errors map like libtorrent's: a `failure reason` (HTTP) or error action
  (UDP) is `TrackerError`; an endpoint that could not be used (e.g. no IPv4
  address in IPv4-only mode, libtorrent `announce_skipped`) is `Unreachable`;
  DNS, connection, timeout, HTTP status, malformed responses and policy blocks
  are `NotWorking`.
* `message` is the failure reason, error text or `warning message`; it is
  sanitized (no control characters, at most 512 bytes) and never contains the
  tracker URL.
* `endpoints` hold the per-address-family results of the last announce
  (`AddressFamily::V4/V6`, or `Any` for HTTP when unknown): UDP trackers are
  announced over IPv4 and IPv6 when both resolve. The tracker row aggregates
  them like qBittorrent's `updateTrackerEntryStatus` (working if any endpoint
  works, max of the counts, message from an endpoint in the row's state).
* `seeds`/`leechers` come from the announce (`complete`/`incomplete`) or a
  scrape; `downloaded` from the announce or a scrape. `peers` is the number of
  peers in the last response.
* Statuses reset to `NotContacted` when the torrent stops.

### Announce behaviour (libtorrent semantics)

* One announcer task per started torrent, spawned when the torrent's peer
  stream is first polled (live, or resolving a magnet), on the session token.
* Events: `started` first (again after a failed `started`), `completed` once
  when the torrent gets all pieces during the run (a torrent that starts as a
  seed never sends it), `stopped` when the peer stream is dropped (pause,
  delete, error, session stop) to every tracker that got `started`, bounded by
  `stop_tracker_timeout`; `0` disables `stopped` announces (libtorrent). A new
  run waits for the previous run's `stopped` announces. `Session::stop` waits
  for them before cancelling the session.
* Parameters: `uploaded`/`downloaded` of the current run (downloaded = verified
  payload), `left` = total − verified bytes (16 KiB while metadata is
  unknown), `numwant` (200; 0 for `stopped`), per-torrent random `key`
  (`%08X`), `trackerid` echoed back, `ip=` from `announce_ip`, `port` from
  `announce_port` or the listen port, `compact=1&no_peer_id=1`. UDP sends the
  URL path and query as BEP 41 URL data.
* Timing: next announce after `max(interval, min_announce_interval)` (300 s
  floor), `min interval` (default 30 s) respected except for `completed` and
  forced reannounces; failures back off `5 + 12.5 × fails²` s (max 1 h) or the
  tracker's BEP 31 `retry in`; `retry in: never` and HTTP 410 stop retrying.
  Tracker-imposed delays are capped at one week.
* Tiers: trackers stay sorted by tier; a working tracker moves to the front of
  its tier, a failing one to the back. `announce_to_all_trackers` announces to
  every tracker of a tier, `announce_to_all_tiers` (default true, as in
  qBittorrent) to every tier; with neither, only the first working tracker is
  used (BEP 12). A forced reannounce of one URL announces to it even if it is
  a backup tracker of its tier.
* Scrape: HTTP scrape URL per BEP 48 (last path component `announce*` →
  `scrape*`), UDP action 2. A working tracker whose announce lacked
  `downloaded` or seed counts is scraped right after the announce and then at
  most every 30 min; trackers without a scrape URL are not scraped again.
* Private torrents (`private=1`) use all their trackers with tiers, never DHT
  or LSD (also when the flag arrives with magnet metadata: the DHT/LSD streams
  end and session extra trackers are removed); PeX is already disabled by the
  live engine.

### Session settings

`SessionOptions::tracker_settings: TrackerSettings`, changeable at runtime with
`Session::set_tracker_settings` (read back with `tracker_settings()`):
`announce_ip`, `announce_port` (0 = listen port), `max_concurrent_http_announces`
(0 = unlimited; applies to HTTP announces and scrapes), `validate_https_tracker_certificate`,
`ssrf_mitigation` (default true), `stop_tracker_timeout` (default 5 s),
`announce_to_all_trackers` (false), `announce_to_all_tiers` (true),
`anonymous_mode` (no `User-Agent` for non-private torrents and no `ip=`, like
libtorrent), `user_agent` (default: the session client name), `num_want`
(200), `min_announce_interval` (300 s), `announce_timeout` (30 s).

Also on `Session`:

* `set_tracker_ip_filter(Option<IpFilterFn>)` — `Fn(IpAddr) -> bool`, true =
  blocked; tracker addresses it blocks are not contacted ("Apply to trackers").
* `reannounce_all()` — force reannounce on every torrent (IP change, wake-up).
* `SessionOptions::disable_trackers` / `AddTorrentOptions::disable_trackers`
  turn announcing off (the list still exists).
* `AddTorrentOptions::trackers_local_source` — whether the torrent's trackers
  are trusted by SSRF mitigation (default: true for torrent bytes, false for
  URLs and magnets). Trackers added through the API are always trusted.
  `AddTorrentOptions::trusted_urls` lists trackers and web seeds trusted
  anyway (user additions), and `replace_trackers_with_trust` keeps that
  distinction for runtime list changes. The core adds every torrent with
  `trackers_local_source = false` and its persisted `trusted_urls` (the
  trackers and seeds of a .torrent file handed to it directly, plus user
  additions); torrents from URLs, RSS, search plugins and magnets are
  untrusted.

### Security

Tracker responses and URLs are treated as hostile:

* HTTP bodies are capped at 1 MiB; bencode is decoded by a limited decoder
  (depth 16, 100 000 values, no recursion beyond the depth limit, trailing
  garbage ignored); at most 1000 peers per response; peers with port 0,
  unspecified, multicast or broadcast addresses are dropped, and loopback
  peers are dropped unless the tracker itself is on loopback; host names in
  dictionary peers are ignored (never resolved).
* UDP: replies are accepted only from the address the request went to and
  with our random transaction id; requests are retransmitted once (5 s + 10 s);
  connection ids expire after 60 s; truncated or garbage datagrams are
  ignored without panicking.
* Only http, https and udp trackers are contacted; HTTP redirects are limited
  to 5, http/https only, and never to local addresses (when SSRF mitigation is
  on).
* SSRF mitigation (`tracker_comms::NetPolicy`): an HTTP tracker on a loopback
  address must have a path starting with `/announce` (libtorrent rule;
  `/scrape` for derived scrape URLs); a tracker URL with its own query string
  that points to a loopback, private, link-local, unique-local or CGNAT
  address is refused unless the tracker is trusted (local torrent source or
  added by the user). Host names are resolved by a guarded DNS resolver that
  drops local addresses of hosts not vetted by that check, so DNS rebinding
  cannot bypass it. When a proxy resolves names, only literal addresses can
  be checked.
* Logs and status messages never contain full tracker URLs: `redact_url`
  masks query values and passkey-like path segments, and HTTP errors are
  stripped of their URL.
* Fuzz-style tests cover the bencode decoder, the HTTP announce/scrape
  parsers and the UDP datagram parser.

### Peer sources

`librqbit::peer_source` defines `PeerSource` (`Tracker | Dht | Pex | Lsd |
Incoming | Manual`), the bit set `PeerSources`, `PeerSourceCounts { tracker,
dht, pex, lsd }` and `SourcedPeerStream = BoxStream<(SocketAddr, PeerSource)>`.
`Session::make_peer_rx` builds a sourced stream (DHT, LSD, trackers, initial
peers = `Manual`), records each address in the torrent's bounded
`PeerSourceRegistry` (16 384 addresses, cleared when the torrent starts) and
converts it to the existing `PeerStream` of plain addresses at that boundary
(`torrent_state::trackers::record_and_strip`). Integration points for the live
engine: `ManagedTorrentShared::record_peer_source(addr, PeerSource)` (call it
for PeX and incoming peers), `ManagedTorrent::peer_sources(addr)` for per-peer
flags, and `peer_source_counts()` counts live peers per source from
`per_peer_stats_snapshot`. When the live engine gains
`add_peer_with_source(addr, PeerSource)`, `record_and_strip` can be replaced by
passing the sourced stream through.

### Tests

`cargo test -p librqbit-tracker-comms`: tier selection for every
`announce_to_all_*` combination, failover and promotion, backoff, `retry in`,
410, forced reannounces, events, interval floors and caps, status
aggregation, scrape planning, URL policy and redaction, query building, BEP 48
derivation, parsers (compact/dict/IPv6 peers, failure reason, warning, min
interval, counts), fuzzing, a local UDP tracker (spoofed and wrong-transaction
replies ignored), and HTTP/UDP client tests against local servers (SSRF,
redirects, size cap, IP filter). `cargo test -p librqbit e2e_trackers`: a
seeder and a leecher that only meet through a local HTTP tracker (events,
stats, failover, scrape, forced reannounce, pause/unpause, delete, shutdown).

## engine-storage — storage operations (`engine/librqbit/src/storage`, `torrent_state/storage_ops.rs`)

### Layout

Every torrent has a `StorageLayout` (`ManagedTorrentShared::layout`,
`ManagedTorrent::storage_layout()`), shared by all storage instances the
torrent goes through. It holds the output folder (dynamic: read it with
`ManagedTorrent::output_folder()` / `ManagedTorrentShared::output_folder()`,
both return `PathBuf`; `ManagedTorrentOptions::output_folder` is gone), each
file's user path (renames) and actual on-disk path (decorations), per-file
completion and wanted state. Each file has its own lock: I/O takes it shared,
path changes take it exclusively, so paths never change under in-flight I/O.

`FilesystemStorage` resolves paths through the layout and opens files lazily
through a session-wide LRU pool of handles (`StorageSettings::file_pool_size`,
default 100). Files are no longer opened (or created) up front; selected files
are created/sized after the check as before. A torrent whose storage factory
isn't (or doesn't wrap) `FilesystemStorageFactory` keeps path bookkeeping but
skips every disk operation. `TorrentStorage::layout()` (default `None`)
exposes the layout to code that only has the storage.

### Path safety

`storage::paths`: torrent-supplied names are sanitized per component in
`TorrentMetadata::new` (empty, `.`, `..` → `_`; separators, NUL and control
characters → `_`; Windows-invalid characters on Windows; components truncated
to 255 bytes keeping the extension; collisions after sanitizing get `_<index>`).
The default sub-folder derived from the torrent/magnet name is one sanitized
component. User paths (renames, `AddTorrentOptions::renamed_files`) go through
`validate_relative_path`, which rejects empty paths, absolute paths and
prefixes, empty components, `.`/`..`, NUL and over-long components. Upstream
only rejected `..` and separators, before decoding.

### Public API

`ManagedTorrent`:

* `async move_storage(self: &Arc<Self>, new_output_folder: PathBuf, mode: MoveMode) -> anyhow::Result<()>`
  — absolute destination; live torrents are paused internally (user intent
  kept), files are renamed (same volume) or copied + fsynced + deleted (across
  volumes), then the torrent resumes there. `MoveMode::{AlwaysReplace,
  FailIfExist, DontReplace}` follow libtorrent's `move_flags_t`;
  `DontReplace` keeps files already at the destination (any decoration
  variant) and rechecks the torrent. On failure moved files are moved back and
  the folder is unchanged. Empty source folders of the torrent are removed (and
  the old output folder if owned). The new folder is persisted.
* `move_progress() -> Option<MoveProgress { destination, moved_bytes, total_bytes }>`
  — for qBittorrent's "Moving" state.
* `async rename_file(index, new_relative_path)` /
  `async rename_files(Vec<(usize, PathBuf)>)` — batch-validated (also against
  other files: same path, file-vs-folder clashes; case-sensitive so case-only
  renames work), renamed on disk immediately (target must not exist unless it's
  the same file), also while live; leftover empty folders removed; persisted.
  Folder renames = rename every file under the prefix (core policy).
* `file_paths() -> Vec<PathBuf>` (user paths), `actual_file_paths()` (on
  disk, with decorations), `renamed_files() -> BTreeMap<usize, PathBuf>`.
* `async force_recheck(self: &Arc<Self>)` — live, paused, checking or errored
  torrents: throws away fastresume and seed-mode data, hashes everything
  through the initializing state (`TorrentStats::progress_bytes` =
  checked bytes), then returns to running/paused as the user had it.
* `error_kind() -> Option<TorrentErrorKind>` — `MissingFiles`, `DiskFull`
  or `Other` when in the error state (`storage::error::error_kind_of` walks
  the error chain for `StorageError`).

`Session`:

* `set_storage_settings(StorageSettings)` (async) / `storage_settings()`;
  also `SessionOptions::storage`. `StorageSettings { incomplete_suffix:
  Option<String>, use_unwanted_folder, preallocate, file_pool_size,
  read_cache, write_cache: DiskCacheMode }`. Toggling the suffix or unwanted
  folder renames the files of all torrents.
* `set_max_active_checking_torrents(i64)` (negative = unlimited, 0 = none) /
  `max_active_checking_torrents()`; replaces the fixed init semaphore
  (`SessionOptions::concurrent_init_limit` is the initial value, default 3). A
  pause request ends a torrent's wait for a slot.
* `delete_with_options(id, DeleteOptions { delete_files, trash: Option<Box<TrashFn>> })`;
  `delete(id, delete_files)` calls it. Deletes only the torrent's files at
  their actual paths, then the torrent's folders left empty (junk files
  `.DS_Store`, `Thumbs.db`, `desktop.ini`, `.directory`, `*~` don't count), then
  the output folder only if the torrent owns it. Waits for a running move.

`AddTorrentOptions`: `skip_hash_check`, `renamed_files`,
`owns_output_folder` (default: true when the session derived the folder —
default sub-folder or `sub_folder` — false for an explicit `output_folder`;
upstream removed any empty non-default output folder). JSON session
persistence stores renames and the ownership flag (Postgres doesn't).

`storage::available_space(path)` — free bytes for the user on that volume
(`statfs` on macOS) for qBittorrent's free-disk-space display.

### Behaviours

* **Incomplete suffix**: a non-empty incomplete file lives at
  `<path><suffix>`; when its last piece completes it's renamed (from
  `TorrentStorage::on_piece_completed`, per file). On start, data is looked up
  under every variant (plain, suffixed, `.unwanted`, both; the conventional
  `.!qB` is searched even when the option is off), then names are reconciled
  after the check, so restarts with partially suffixed files work.
* **Unwanted folder**: files not wanted (not in `only_files`; priority 0)
  live in `<parent>/.unwanted/`; suffix first, then folder. They move back
  when wanted again (`update_only_files` hooks
  `ManagedTorrent::on_files_wanted_changed(wanted)`; a future priority API
  should call it too). Empty `.unwanted` folders are removed.
* **Pre-allocation**: selected files are allocated at the end of the check
  (`F_PREALLOCATE` on macOS, `fallocate` on Linux, falling back to `set_len`).
* **OS cache**: `DiskCacheMode::Disabled` sets `F_NOCACHE` on macOS
  (no-op elsewhere); `WriteThrough` (writes) fsyncs a piece's files when it
  completes. Changing cache modes closes all handles.
* **Skip hash check**: the first check of a torrent added with it (and no
  fastresume) marks every piece present without hashing and remembers them as
  unverified. `FileOps::read_chunk` hashes an unverified piece before serving
  it; on mismatch the piece is marked missing (`ChunkTracker::mark_piece_missing`),
  peers are reconnected and the piece is downloaded again, and the read fails.
  Streaming reads don't verify.
* **Missing files**: if fastresume data claims pieces of a file that doesn't
  exist, the check fails with `StorageError::MissingFiles` (progress kept);
  going live from paused does the same check. Starting such a torrent re-checks
  with fastresume (detects again or validates), `move_storage` to where the
  files are recovers it, `force_recheck` starts over.
* **Disk full**: `ENOSPC`/`EDQUOT` on write become `StorageError::DiskFull`;
  the live torrent stops with that error as before.
* While a move/recheck holds a torrent internally paused, `start`/`pause`
  only record the user's intent, applied when the operation ends.

### Hooks outside the storage modules

`torrent_state/mod.rs` (start/pause/update_only_files hooks, sanitized file
infos, `layout` field, `skip_hash_check` option), `session.rs` (layout
creation, settings/limiter fields and setters, delete), `file_ops.rs`
(verification on read), `chunk_tracker.rs` + `piece_tracker.rs` (additive
`mark_piece_missing`), persistence and `api.rs` (output folder accessor).
Nothing in `torrent_state/live/**` changed.

Limitations: moves are serialized per torrent only (qBittorrent's global FIFO
move queue is core policy); Postgres persistence doesn't store renames;
seed-mode state isn't persisted (after a restart fastresume validation samples
pieces as usual); custom storages that neither are nor wrap the filesystem
storage get no decorations, moves on disk or skip-hash-check verification (the
bundled middlewares forward `layout()` and `on_piece_completed`).

## Syncing with upstream

The fork follows [rqbit](https://github.com/ikatson/rqbit) (Igor Katson,
Apache-2.0). The sync point is recorded in `engine/README.md` ("Synced with
upstream up to <sha> (<date>)").

Procedure:

1. `scripts/engine-upstream.sh [--prs]` clones or fetches upstream into a cache
   outside the repository (`~/.cache/qbt-neo/rqbit-upstream` by default) and
   prints the upstream commits after the sync point that touch a kept crate,
   with the paths they touch and a classification hint, then the commits that
   only touch dropped crates, CI or docs. `--prs` also lists open upstream pull
   requests that touch kept crates. The script never adds a remote here, never
   builds upstream code and never pushes.
2. Classify each commit: bug fix, security, performance, feature, refactor or
   irrelevant (CLI, desktop app, web UI, media server). Read each one before
   porting it; upstream code is not built or run outside this workspace.
3. Port commit by commit: `git format-patch -1 <sha>` in the cache, then
   `git apply -p2 --directory=engine --reject` here (upstream `crates/<name>`
   is `engine/<name>`), and resolve conflicts by hand. Keep the fork's
   behaviour where it deliberately differs (session-wide timers and idle
   parking, socket-level throttling, choking, tracker management, storage,
   the peer idle timeout and body deadlines, the fixes in
   `docs/security-audit.md`). Commit as
   `Port upstream rqbit <shortsha>: <subject>` with a line on conflicts.
4. Check upstream's `Cargo.toml`/`Cargo.lock` for dependency bumps that carry
   fixes, and check every dependency against the RustSec advisory database
   (`cargo audit`, or the advisory-db repository by hand).
5. Run the engine, core and e2e tests and clippy, and compare localhost
   throughput and idle wakeups with the previous build (see "Idle wakeups").
6. Move the sync point in `engine/README.md` and add the ported and skipped
   changes to the log below.

### Sync log

**2026-10-18** — sync point `57e95d1` (upstream `main`, 2026-10-09; latest
release v9.0.1, 2026-08-20). Upstream `main` has no commits after the fork
point, so the only source was the open upstream pull requests touching kept
crates; their fixes are ported where the fork has the bug. The upstream branch
`push-qnpqwoylyozn` (`93af83bf`, "accept no longer panics": a `select!` whose
pattern-matched branch could leave every branch disabled) was checked too; the
fork's listener already always matches and has an `else` branch.

| Upstream change | Kind | Result |
|---|---|---|
| PR #704 `a6607a4e` ut_metadata piece bounds off by one (remote panic) | security | already fixed (`metadata_piece_range`) |
| PR #700 `28caa8fb` LSD announce task outlives its torrent | bug fix | already fixed (announce token cancelled on drop) |
| PR #653 `1e924f87` middlewares drop `on_piece_completed` | bug fix | already fixed (all middlewares forward it) |
| PR #695 `eb831e15` `CurrentPiece::piece_offset` was the piece number | bug fix | ported |
| PR #701 `2563195c` duplicate `.pad/<len>` names reject hybrid torrents | bug fix | ported (unit tests; upstream e2e test not taken) |
| PR #706 `618f426c` DHT get_peers queue and rate-limiter wait unbounded | performance / memory | rate-limiter wait ported; the in-flight cap was already stricter here (8 per round, 32 rounds, 16 nodes per reply) |
| PR #656 `a742b747` per-file progress stops at a zero-length file | bug fix | ported (also for web seed and v2 layer completions) |
| PR #667 `aa45753a` peers map grows by one entry per incoming address | memory | ported with a rescan guard; the upload-disabled refusal not taken (no such mode here) |
| PR #614 `6913d56d` UPnP WANIPConnection:2 | feature (parity) | ported (one socket for both searches) |
| PR #657 `f2bdbfb0` commit a piece to storage before marking it have | bug fix | skipped: the fork's filesystem storage can't refuse a piece (`on_piece_completed` only updates the layout), so the reordering changes nothing here |
| PR #703, #680 file descriptor limits, release handles on pause | resource use | skipped: covered by the fork's file handle pool (engine-storage) |
| PR #696, #702, #693, #640, #574 streaming piece stealing and readahead | feature | skipped: streaming is not a qBittorrent feature; sequential download is covered |
| PR #583 BEP 5 `nodes` from .torrent files into the DHT | feature | skipped for now: rarely useful (normal bootstrap finds the swarm) and it turns torrent-supplied host names into DNS lookups and DHT queries; revisit if merged upstream |
| PR #581 BEP 6 fast extension | feature | skipped: unreviewed, conflicting, bundled with an RPC server. BEP 6 itself is a parity gap (libtorrent supports it) to implement separately |
| PR #540 tracker announce lifecycle | bug fix | skipped: the fork replaced tracker management (engine-trackers) |
| PR #633 MSE, #624 categories, #639 connect timeout, #644 peer bitfields, #650 chunk progress | feature | skipped: the fork has its own MSE, categories are core policy, the rest is already exposed or not needed |
| PR #692, #694, #705, #670, #658, #629, #628, #645, #569, #554, #512 | feature / refactor | skipped: embedder APIs, Windows-only, or covered by the fork's v2 and storage work |

Dependencies: upstream changed none since the fork point. RustSec check of
`Cargo.lock` (advisory-db as of 2026-10-18): `rustls` 0.23.45 had four
advisories (RUSTSEC-2026-0346…0349, fixed in 0.23.46), updated; `ringbuf`
0.4.8 (via `librqbit-utp`, RUSTSEC-2026-0293) is only affected when an
element's `Drop` panics, and uTP stores `u8` there, so not reachable;
`crypto-hash` (SHA-1 backend, RUSTSEC-2025-0060) is unmaintained but
not vulnerable (it uses CommonCrypto on macOS).
