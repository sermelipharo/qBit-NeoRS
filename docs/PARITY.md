# Feature parity with qBittorrent 5.2.4

Status legend: ✅ done and tested · 🟡 partial (see notes) · ⏳ in progress ·
⬜ not started · ➖ not applicable on macOS.

Detailed requirements for every row are in `docs/spec/features.md` (§) and
`docs/spec/preferences.md`.

## Engine (BitTorrent protocol)

| Feature | Status | Where | Notes |
|---|---|---|---|
| TCP + µTP peers, DHT, LSD, PeX, magnet metadata (BEP 9) | ✅ | engine (upstream), engine-net | private torrents never use DHT/PeX/LSD; per-torrent "Disable DHT/PeX/LSD" applied live, persisted, shown in the Trackers tab (core: `set_dht_disabled`/`set_pex_disabled`/`set_lsd_disabled`, `discovery_options`; no WebAPI in qBittorrent either); toggled from the Torrent Options dialog |
| UPnP / NAT-PMP port mapping | ✅ | engine-net | `upnp` pref + lease; extra mappings for the WebUI and embedded tracker ports; external IP in `ServerState` + `ExternalAddressChanged` |
| File priorities 0/1/6/7, sequential, first/last piece | ✅ | engine-live | wired in the session |
| Piece states / availability, distributed copies | ✅ | engine-live | wired (pieces bar, Content availability) |
| Detailed peers (flags, client, source, progress, speeds) | ✅ | engine-live | `E`/`e`/`P` from the engine, I2P connection type; `h` always off (no hole punching) |
| Choking: upload slots, fixed/rate-based, seed algorithms, optimistic unchoke | ✅ | engine-live | applied live from preferences |
| Super seeding (BEP 16) | ✅ | engine-live | incl. share-limit action |
| Global / per-torrent connection and rate limits at runtime | ✅ | engine-live | uTP/LAN exemptions applied |
| Tracker tiers, status, scrape, edit at runtime, reannounce, events | ✅ | engine-trackers | list persisted by the core; tracker settings + "apply IP filter to trackers" live |
| Move storage, rename files/folders, recheck, skip hash check | ✅ | engine-storage | content layouts expressed as renames; one move at a time (core) |
| `.!qB` suffix, pre-allocation, `.unwanted` folder, missing files | ✅ | engine-storage | + disk-full errors, trash deletes, file handle pool, OS cache modes |
| Web seeds (BEP 19) | ✅ | engine-net | HTTP Sources, add/edit/remove applied to live torrents, list persisted in resume data |
| Encryption MSE/PE (allow/require/disable) | ✅ | engine-net | applied live |
| HTTP/SOCKS4/SOCKS5 proxies for peers & trackers, anonymous mode | ✅ | engine-net | applied live; UDP trackers skipped while a tracker proxy is on |
| Bind interface/address, outgoing port range, announce IP/port | ✅ | engine-net | live; also ToS, socket buffers, connection speed, TCP/uTP choice, `utp_tcp_mixed_mode` (peer-proportional as libtorrent; prefer-TCP emulates LEDBAT yielding under the session limit) |
| Live reconfiguration (port, DHT/LSD/PeX toggles, IP filter, bans) | ✅ | engine-net | no restart; only a changed DHT bootstrap list (or enabling DHT on a session started without it) restarts the engine |
| I2P (experimental in qBittorrent) | ✅ | engine-net | `i2p_*` prefs applied live; no I2P DHT/PeX |
| v2 / hybrid torrents | ✅ | engine (BEP 52) | pure v2 download/seed from .torrent and `btmh` magnets (merkle piece checks, hash request/hashes/hash reject served and used), Info hash v1/v2 shown, creator writes v2/hybrid; torrent id = truncated v2 hash whenever there is one (qBittorrent's TorrentID; a hybrid added by `btih` switches ids when its metadata arrives, duplicates found under either hash, old resume data migrated on load); hybrids run on their v1 half but are announced (trackers, DHT, LSD), reachable and served (piece layers) under both hashes like libtorrent; a pure v2 .torrent without (some) piece layers starts at once like libtorrent: files whose layer is known (and files no longer than a piece) download and verify right away, the others are held back (never requested, never counted as had) until their layer is fetched from peers (BEP 52 hash requests, verified against each file's root) or found by hashing a complete file on disk, a hybrid added by magnet fetches its layers from a v2 peer and serves them, v1 connections of hybrids are upgraded to v2 both ways like libtorrent; interop with libtorrent 2.1 checked by `scripts/interop/libtorrent_interop.py` (44 scenarios; ENGINE.md "Missing piece layers") |

## Core (`qbt-core`)

| Feature | Status | Where | Notes |
|---|---|---|---|
| Preferences model (223 WebAPI keys + GUI-only core keys), persistence | ✅ | prefs/ | WebAPI JSON mapping in `prefs/api_json.rs` |
| Execution log (general + blocked IPs), file log with rotation | ✅ | logger.rs | |
| Torrent registry, states (§1.7), all TorrentInfo fields | ✅ | bittorrent/ | tested (unit + session integration + e2e) |
| Adding torrents (file/URL/magnet/hash), all AddTorrentParams | ✅ | bittorrent/ | duplicates/merge, extra trackers + from URL, export, auto-delete |
| Categories (+subcategories), tags, automatic torrent management | ✅ | bittorrent/ | category share limits; three TMM triggers |
| Queueing system, slow-torrent exemption | ✅ | bittorrent/ | core auto-manager (pure, tested) |
| Share limits (ratio/seeding/inactive) and actions | ✅ | bittorrent/ | |
| Alternative speed limits + scheduler | ✅ | bittorrent/ | |
| Watched folders, excluded file names, export dirs, auto-delete .torrent | ✅ | bittorrent/ | polled every 5 s (no FS events) |
| Run external program on added/finished, completion e-mail | ✅ | bittorrent/, net/smtp | + Mark-of-the-Web on completed files |
| IP filter (.dat/.p2p/.p2b) and banned IPs | ✅ | ipfilter.rs | applied live (peers + optionally trackers), matching peers disconnected at once; every blocked connection reaches the Blocked IPs log |
| Resume data (BT_backup), statistics persistence | ✅ | bittorrent/ | JSON fastresume; engine bitfields for fast restarts |
| RSS reader + auto-downloader (smart episode filter) | ✅ | rss/ | + RSS 1.0/RDF; hardened (see SECURITY.md) |
| Search engine (nova3 Python plugins) | ✅ | search/ | tested with Python 3.14; hardened (SECURITY.md) |
| Torrent creator v1/v2/hybrid | ✅ | creator/ | lt 2.0 layout/padding, lt 1.2 alignment for v1; seeding via Session::add_torrent; hardened (SECURITY.md) |
| HTTP client with proxies/cookies, GeoIP, DynDNS, SMTP | ✅ | net/ | + interfaces, trash, quarantine; hardened (SECURITY.md) |
| Embedded tracker | ✅ | tracker/ | hardened (see SECURITY.md) |
| WebAPI v2 (2.15.1) + bundled qBittorrent WebUI | ✅ | webui/ | HTTPS, auth/bans, CSRF/host checks, alternative WebUI; hardened (SECURITY.md) |
| Headless daemon (`qbittorrent-nox`) | ✅ | crates/qbt-nox | `qbit-neors-nox --profile --webui-port` |

## macOS app

| Feature | Status | Where | Notes |
|---|---|---|---|
| Main window: toolbar, menus, status bar | ✅ | apps/macos | |
| Filters sidebar (status, categories, tags, trackers) | ✅ | apps/macos | tracker favicons (opt-in "Download tracker's favicon"): fetched once per host by net/favicon.rs, 64 KiB cap, decoded with ImageIO, bounded memory cache |
| Transfer list with all columns, sorting, context menu | ✅ | apps/macos | |
| Properties: General, Trackers, Peers, HTTP Sources, Content, Speed | ✅ | apps/macos | |
| Add torrent dialog, add links, drag & drop, magnet/file handlers | ✅ | apps/macos | |
| Torrent dialogs (location, rename, limits, share limits, options, trackers) | ✅ | apps/macos | |
| Preferences window (all pages) | ✅ | apps/macos | 9 pages; UI lock: PBKDF2 hash in UserDefaults, View ▸ Lock (⌘L), Behavior row, lock screen replaces the main window, other windows closed, Dock/menu bar menus disabled |
| RSS reader + downloader rules windows | ✅ | apps/macos | |
| Search window + plugins dialog | ✅ | apps/macos | open tabs (and optionally results) restored from data/SearchUI/Session.json like qBittorrent |
| Execution log, statistics, torrent creator, about, cookies | ✅ | apps/macos | |
| Dock badge, menu bar extra, notifications | ✅ | apps/macos | |
| Power management, auto-shutdown on completion | ✅ | apps/macos | |
| Localization (qBittorrent's languages via String Catalog) | ✅ | scripts/i18n | 50+ languages imported from qBittorrent translations; Russian complete |
| App icon | ✅ | assets/ | |
