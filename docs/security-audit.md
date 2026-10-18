# Security audit — network-facing modules (2026-10-17)

Scope: every module that talks to the network or parses data that comes
from it. That covers:

* the peer wire protocol and its extensions (BEP 3/6/9/10/11/52, MSE);
* bencode and torrent metadata, magnet links, and the file layout on disk;
* DHT, HTTP/UDP trackers, LSD, UPnP/NAT-PMP and uTP;
* the WebUI/WebAPI;
* RSS, search plugins and HTTP downloads;
* migration from qBittorrent;
* the macOS front end where it renders or opens untrusted content.

Earlier reviews are in `docs/SECURITY.md` (2026-10-15, two passes). This
audit starts from a list of vulnerabilities previously found in other torrent
clients and libraries and checks each class against this code. It also looks
for shortcuts that skip a check:

* lengths trusted before they are validated;
* `unwrap`, indexing or slicing on attacker data (the release profile has
  `panic = "abort"`, so any reachable panic kills the app);
* truncating `as` casts;
* recursion without a depth limit;
* unbounded queues fed by the network.

Method: the code was read for each class. Every fix comes with a regression
test, and each test was confirmed to fail without its fix:

* the LSD crash: the panic reproduced in the test;
* the DHT fan-out: 251 queries in 3 s without the fix, 21 with it.

All tests run against local in-process servers only. No traffic was sent to
third-party hosts, and the running app was not touched apart from listing its
sockets.

Exposed services of a default instance (from `lsof`):

* WebUI on TCP `*:9292`;
* the peer port on TCP and UDP (uTP) `*:<port>`;
* DHT on UDP;
* LSD on UDP `*:6771`;
* an ephemeral UDP socket for SSDP/NAT-PMP.

## Summary of new findings

| # | Severity | Area | Finding | Status |
|---|---|---|---|---|
| 1 | **Critical** | LSD / hash ids | One LSD datagram whose `Infohash` header is padded base32 of the wrong decoded length (`AAAA…A=`) panicked in `Id::from_str` (`copy_from_slice`): remote crash, no authentication, any host that can reach UDP 6771 | Fixed `1533c74` |
| 2 | High | DHT | Lookup fan-out was unbounded: every node of every reply (≈300–600 per datagram) was queued at each depth. A hostile node returning fresh ids that point at itself grew the queues by ~150k entries/s (memory), used up the 250 queries/s budget, and could aim our queries at a third party | Fixed `94a0a0d` |
| 3 | Medium (non-default config) | WebUI | A reverse proxy on the same host connects from loopback, so with "bypass authentication for localhost" its remote clients were treated as local. With "reverse proxy support" on, a client-supplied `X-Forwarded-For: 127.0.0.1` passed through the trusted proxy also became loopback. Leads to unauthenticated WebAPI access, which means code execution (class of qBittorrent's localhost-bypass-behind-proxy issue) | Fixed `4647910` (partial, see residual) |
| 4 | Medium | WebUI | Brute-force ban race: the ban was checked once before the queued PBKDF2 verification, so N concurrent login/Basic-auth requests each got a guess (≈500 per ban window instead of 5) | Fixed `f0dd5cb` |
| 5 | Medium | WebUI | `search/downloadTorrent` (adds a torrent) and `rss/setFeedRefreshInterval` accepted GET, and HEAD ran any GET action. With the localhost bypass or the subnet whitelist, a cross-site `<img>` could add a torrent (upstream parity) | Fixed `e9a0980` |
| 6 | Medium | peer wire | Unbounded writer queue: a cancelled upload request stayed in the writer's queue. A peer that stopped reading could cancel and re-request one chunk in a loop and grow the queue without bound (the 500-request cap never triggered) | Fixed `79928a8` |
| 7 | Medium | peer wire | Unbounded writer queue, second path: each interested/not-interested toggle queued an unchoke+choke pair (≈10 bytes from the peer per two queued messages) | Fixed `74e5926` |
| 8 | Medium | DHT | Reflection: every incoming query was answered at once with a reply up to ~24× its size (`want=n4,n6` + 100 values), with no per-source limit | Fixed `bdcee72` |
| 9 | Medium | metadata / storage | v1 file paths had no depth limit (v2 had 64). The storage path checks hash every ancestor, so one 100k-component path (~300 KB of metadata) was quadratic, ~10¹⁰ operations on a runtime worker | Fixed `3dda48d` |
| 10 | Medium | macOS RSS | Hostile article description froze the main thread. The sanitizer rescanned to the end of the input at every `<` after an unterminated tag; the BBCode/tag regexes backtracked over whole lines. Up to 1 MiB per description, and the newest article is selected automatically | Fixed `ccf94ef` |
| 11 | Low-Med | DHT / peers | Untrusted sources (DHT values up to ~2000 per datagram, trackers, PeX, LSD) grew a torrent's peer list without bound until reconnect backoffs ran out | Fixed `73a4bee` |
| 12 | Low-Med | UPnP | Any LAN host could answer SSDP thousands of times; each reply started a description fetch of up to 256 KiB / 10 s | Fixed `3218ea6` |
| 13 | Low | WebUI | Login bodies had a 120 s deadline before authentication: 500 trickling connections locked the WebUI | Fixed `facdf67` |
| 14 | Low (latent) | net | `parse_cookie_date` sliced `token[..3]` inside a UTF-8 character (panic); unreachable today because headers arrive as visible ASCII | Fixed `8ae470b` |
| 15 | Low (latent) | magnet | `so=0-18446744073709551615` expanded the whole range (allocation abort); the core never hands raw magnets to the engine | Fixed `c2c5402` |

Residual risk for #3: a reverse proxy that adds no forwarding header
(nginx's default `proxy_pass` sends none) still cannot be told apart from a
local client. `docs/SECURITY.md` now says not to enable the localhost bypass
behind such a proxy.

## Known vulnerability classes checked

Status: **safe** (not vulnerable), **fixed** (vulnerable, fixed in this
audit), **partial** (mitigated, residual risk noted), **open** (not fixed;
see "Open").

### Peer wire protocol (`engine/peer_binary_protocol`, `engine/librqbit`)

| Known issue / class | Status | Evidence |
|---|---|---|
| Message length prefix trusted, allocation sized by attacker (`Vec::with_capacity(len)`) | safe | nothing is allocated from the length; messages over the 32 KiB read buffer are an error (`peer_binary_protocol/src/lib.rs:450-469`, `librqbit/src/read_buf.rs:201-205`) |
| Transmission CVE-2014-4909 (bitfield integer overflow → OOB write) | safe | the bitfield length must match the piece count exactly (`torrent_state/live/mod.rs` `on_bitfield`); spare bits are clamped (`availability.rs:51-63`) |
| `have` / request / piece for an out-of-range index | safe | `peers/mod.rs:148-167`, `validate_piece_index`, `lengths.rs:207-230` (exact chunk offset and size) |
| `begin + length` overflow, length 0 or > 16 KiB | safe | `chunk_info_from_received_data` requires exact chunks; v2 short blocks are checked in u64 (`live/mod.rs` `requested_chunk`) |
| Piece data we never requested / duplicates | safe | in-flight check; unexpected data disconnects the peer (`live/mod.rs` `on_received_piece`) |
| Request flooding | safe | at most 500 outstanding (`reqq`); flooders disconnected (`upload_queue.rs`) |
| Request/cancel churn and interest toggling filling the writer queue | **fixed** #6, #7 | `upload_queue.rs:25,59` (`MAX_UPLOAD_REQUESTS_IN_WRITER`), `choker.rs:47` (`preemptive_unchoke_allowed`) |
| Extended handshake: huge dict, repeated handshakes, `reqq` → allocation, `m` ids 0/255 | safe | 32 KiB cap, bencode depth 128, `reqq` clamped, PEX task spawned once, repeated metadata handshake refused (`peer_info_reader/mod.rs:452-460`) |
| libtorrent `ut_metadata` size abuse / piece index past the end | safe | `metadata_size` 1..=32 MiB, session budget of 192 MiB, `record_piece` checks index/size/duplicate, serving uses the checked `metadata_piece_range` |
| Net::BitTorrent CVE-2026-57079 (path checks skipped for metadata from `ut_metadata`) | safe | metadata from peers goes through the same `validate` and core sanitisation as .torrent files, and must match the info hash |
| `ut_pex` odd lengths, flags mismatch, huge lists, special addresses | safe | `compact_ip.rs` length multiple; `PEX_MAX_ADDED`, 45 s interval, `pex_candidates` filter |
| BEP 52 hash request / hashes / reject | safe | fixed 48-byte header, multiple-of-32 data, request bounds in `v2.rs:990-999`, replies must match our request |
| Hash-failure bans ("smart ban") | open (Low) | the peer that completed a bad piece is disconnected but may reconnect; no address ban (`live/mod.rs` hash-check failure branch) |
| Slow-drip / slowloris peers | partial (Low) | 30 s handshake deadline and ≤ 4 pending per address; after the handshake the 10 s timeout is per read, not per message (`read_buf.rs:207-213`) |
| Handshake: pstrlen, unknown info hash, self-connection | safe | `lib.rs:632-637`, `session.rs` incoming lookup, `is_own_peer_id` |
| MSE: pad lengths, sync search, VC, crypto selection | safe | PadC/PadD ≤ 512, IA ≤ 272, `sync_on` bounded, RC4 required (`engine/mse/src/lib.rs`) |
| MSE DH CPU cost per incoming attempt | open (Low, already listed in SECURITY.md) | |
| Panics on attacker data (`unwrap`, indexing, `as`) | safe | all parser `unwrap`s follow length checks; bencode depth is capped; no unbounded recursion |
| uTP (`librqbit-utp` 0.7.0): header/extension parsing, SACK | safe (parsing), partial (state machine not reviewed in depth) | `raw.rs:154-212` uses `get`; SACK is truncated to 64 bits; SYN backlog 32, 128 streams per socket, 10 s inactivity |

### Bencode, metadata, magnets, file layout

| Known issue / class | Status | Evidence |
|---|---|---|
| libtorrent CVE-2008-0646 (bdecode recursion → stack exhaustion) | safe | engine decoder depth 128 (`bencode/src/deserialize.rs:22,105-111`); core iterative pre-scan with qBittorrent's depth/token limits (`qbt-core/src/bittorrent/bencode.rs:94-155`) |
| Integer overflow in bencode integers / string lengths | safe | checked `atoi`, `split_at_checked` (`deserialize.rs:74-89`) |
| Piece length 0, total-length overflow, piece hash count mismatch | safe | `lengths.rs:79-108`, `torrent_metainfo.rs:596` |
| libtorrent CVE-2009-1760 (`..` in multi-file paths) | safe | engine rejects `..` and separators (`torrent_metainfo.rs:620-633`); core replaces `.`, `..`, `/`, `\`, `:`, NUL and control characters (`qbt-core/src/bittorrent/paths.rs:19-40`); storage re-validates before every join (`storage/paths.rs:72-157`) |
| Absolute paths, drive letters, Windows separators | safe | `Component::Prefix/RootDir` rejected; `:` and `\` replaced |
| Very long names | safe | components cut to 255 bytes (core) |
| Very deep paths | **fixed** #9 | `torrent_metainfo.rs:624` (`BadTorrentPathTooDeep`, 64 like v2) |
| Symlink attribute (BEP 47 `l`), padding files | safe | `symlink path` is ignored (written as a regular file); pad files are never written |
| Pre-existing symlinks inside the save folder | partial (same in every client) | intermediate links are followed; a torrent cannot create links |
| Name collisions after sanitising; case/Unicode collisions on APFS | partial (Low) | a collision is rejected by the engine's duplicate check (torrent refused); case folding is not compared (already listed) |
| Hybrid torrents whose v1 file list and v2 tree disagree (libtorrent 2.0.9 "consistent v1/v2 hashes") | open (Low) | the v2 half is never compared with the v1 list (`torrent_metainfo.rs` hybrid branch, `live/v2_hashes.rs:120-145`); local storage and verification use v1 only, so the impact is serving inconsistent v2 hashes to v2 peers |
| Magnet `xt` / `btmh` lengths, `so` ranges | **fixed** #1, #15 | `hash_id.rs:100` (decoded length checked), `magnet.rs:8,117` |
| Resume / fastresume data (bitfield length, paths) | safe | `initializing.rs:236-242`, validated `file_paths`; migration reads are size-capped (`migrate/libtorrent.rs:322-349`) |

### DHT, trackers, LSD, UPnP / NAT-PMP

| Known issue / class | Status | Evidence |
|---|---|---|
| Malformed KRPC (missing keys, wrong types, bad compact lengths) | safe | `bprotocol.rs:498-600`, `compact_ip.rs:415` |
| DHT amplification / reflection (BitTorrent DRDoS, Adamsky 2015) | **fixed** #8 | `dht/src/query_limit.rs`, `dht.rs:914` |
| `announce_peer` token forgery, third-party `implied_port` announces | safe | token bound to IP/port/node id (`peer_store.rs:121-157`); the stored IP is always the source address |
| Routing-table / lookup poisoning (sybil nodes, fan-out) | **fixed** #2 (fan-out) | `dht.rs:71-73,530-544`, one node per address per lookup in `should_request_node` |
| Routing-table poisoning (sybil) | open (Low) | no BEP 42 node-id check; nodes are added before they answer (already listed) |
| Responses matched by transaction id + address; sequential tids | partial (Low, already listed) | `dht.rs` `inflight_by_transaction_id` |
| Node/peer addresses from replies (loopback, LAN, multicast, port 0) | **fixed** (part of #2) | `dht.rs:1559` `acceptable_remote_addr` |
| Peer-store and peer-list memory | safe / **fixed** #11 | peer store: 1000 peers, 30 min TTL; per-torrent list capped at 4000 (`peers/mod.rs:32,65`) |
| Tracker response memory exhaustion | safe | 1 MiB HTTP body cap (`tracker_comms_http.rs:13,184-193`), ≤ 1000 peers |
| Tracker `interval` 0 / huge (hammering, `Instant + Duration` overflow) | safe | clamped to 300 s … 7 days, retry ≤ 1 h (`tiers.rs:759-806`, `response.rs:149-152`) |
| Tracker redirects to local addresses (SSRF), schemes | safe | `policy.rs:351-383` plus the guarded resolver |
| UDP tracker spoofed replies | safe | random u32 transaction id + source address (`tracker_comms_udp.rs:346-353`) |
| HTTPS tracker certificates | safe | validated by default (`validate_https_tracker_certificate`, `session.rs:1033`) |
| libtorrent CVE-2016-7164 (crash on malformed gzip from a tracker) | safe | gzip via `async-compression` with size limits; parsing errors are errors |
| UPnP SSRF via `LOCATION` / `controlURL` | safe | same-IP http URLs only, 256 KiB / 10 s / depth 32 (`upnp/src/lib.rs`) |
| SSDP reply flood | **fixed** #12 | `upnp/src/lib.rs:327,429,653` |
| NAT-PMP spoofed replies | safe | connected socket to the gateway, opcode/result/length checks |
| LSD parser panics | **fixed** #1 | `librqbit_lsd/src/lib.rs` test `malformed_infohash_header_is_rejected_not_a_panic` |
| LSD from non-LAN sources; receive error ends the task | open (Low) | unicast from the Internet to 6771 can inject the sender's own address; a `recv_from` error stops LSD until restart |

### WebUI / WebAPI (`crates/qbt-core/src/webui`)

| Known issue / class | Status | Evidence |
|---|---|---|
| CVE-2023-30801 (default credentials → RCE via "run external program") | safe | no default password; a random temporary password is shown, never logged (`webui/mod.rs:154-171`) |
| Auth bypass via localhost behind a reverse proxy | **fixed** #3 (partial) | `security.rs:215,223` |
| Path-normalisation auth bypass (`//`, trailing `/`, `%2F`, `..`) | safe | `parse_api_path` is exact; every endpoint except login requires a session (e2e `every_endpoint_requires_authentication`) |
| Brute force / ban evasion | **fixed** #4 | `session.rs:236` (`begin_attempt`), `webapp.rs:604`; per-/64 bans |
| Timing-unsafe comparisons | safe | constant-time username, PBKDF2 output, API key (`password.rs:80`, `webapp.rs`) |
| Weak password hashing | safe (upstream parity) | PBKDF2-HMAC-SHA512, 100 000 iterations, 16-byte salt (`password.rs:13`); OWASP suggests 210 000 for SHA-512 (Info) |
| Session fixation, ID entropy, cookie flags | safe | 192-bit ids created only by the server, stored hashed; `HttpOnly`, `SameSite=Strict`, `Secure` on HTTPS |
| CSRF (qBittorrent CVE-2017-6503 era), cross-site GET | **fixed** #5 | Origin/Referer check (`security.rs:83`), POST-only matrix plus the two GET actions, HEAD refused (`webapp.rs:443`) |
| Transmission CVE-2018-5702 / uTorrent DNS rebinding | safe | Host validation; the bypass is only granted for IP-literal / `localhost` / configured-domain Hosts (`security.rs:243`) |
| Clickjacking (qBittorrent CVE-2017-6504) | safe | `X-Frame-Options` + CSP `frame-ancestors` |
| XSS through torrent/tracker names | safe | API answers are JSON/plain text with `nosniff`; the UI is qBittorrent's own escaping JS; `Content-Disposition` names sanitised |
| CORS | safe | no `Access-Control-*` headers |
| Deluge CVE-2017-9031 (static file traversal) | safe | decoded `..`, `\`, NUL refused; alternative UI root canonicalised, symlinked directories refused (`assets.rs:82-90,185-260`) |
| Slowloris / connection exhaustion | **fixed** #13 (partial) | 7 s head timeout, 500 connections, 10 s pre-auth body deadline; no per-IP connection cap (Low, open) |
| Body / multipart / parameter limits, parser panics | safe | 64 MiB bodies (64 KiB before auth), 512 MiB global budget, 4096 parameters/parts; parsers fuzzed in tests |
| Session churn from Basic auth / bypass requests (each creates a session) | open (Low) | evicts real sessions at 512 (`session.rs`) |
| XFF spoofing inside broad trusted-proxy ranges | partial (Low, configuration) | `resolve_client` skips every trusted entry; the trusted list should name proxy hosts only |
| SSRF via `torrents/add` URLs | by design | authenticated users may fetch any URL (upstream feature) |

### RSS, search, HTTP downloads, migration, macOS front end

| Known issue / class | Status | Evidence |
|---|---|---|
| qBittorrent CVE-2024-51774 (TLS not verified for downloads/updates) | safe | `danger_accept_invalid_*` only with "Ignore SSL errors" (default off) (`net/http.rs:191-192`); GeoIP/plugins/port check over https; no update checker |
| Decompression bombs | safe | only gzip enabled; `max_size` counts decompressed bytes (`net/http.rs:557`); GeoIP `take(max+1)` |
| Unbounded responses, missing timeouts | safe | every caller sets `max_size` and a timeout |
| Redirects to `file:` or other schemes | safe | http(s) only, ≤ 10 hops, magnet ends the request |
| SSRF from feeds / favicons / search URLs to LAN or loopback | open (Low, upstream parity) | no address policy in the core HTTP client (`net/http.rs:495-521`); blind GETs only |
| XXE / billion laughs | safe | quick-xml, no DTD expansion; bounded entity lookup (`rss/parser.rs:567-593`) |
| ReDoS in rules (Rust) | safe | linear-time `regex` with size limits (`rss/rule.rs:24-36`) |
| Quadratic sanitising / ReDoS in the macOS RSS renderer | **fixed** #10 | `RSSDescriptionRenderer.swift:14`, `RSSHTMLSanitizer.swift:51,194` (tested ad hoc with `swiftc`: 64 Ki-character worst cases now 1–700 ms instead of hanging; there is no Swift test target) |
| Command / argument injection (search plugins, external program) | safe | argv only, `python -I`, validated plugin names; placeholders substituted per argument |
| Path traversal in plugin names, nova2dl output, RSS item names | safe | name regex, `lstat` + canonical temp-folder checks, `O_NOFOLLOW`, RSS files named by UID |
| Opening untrusted links (RSS, comments, search `descrLink`) | safe | http(s) (and ftp for comments) only |
| Migration reading qBittorrent files | safe | size-capped reads, `create_new`, validated ids and paths; the WebUI import fetch (`migrate/mod.rs:186-205`) has no body cap and follows redirects (Low, user-supplied local URL) |
| DynDNS IP check over plain HTTP | open (Info, upstream parity) | `net/dyndns.rs:28` |
| `magnet:` handler adds without confirmation when the add dialog is off | open (Info, user setting) | `AppModel+Adding.swift` |

## Open (not fixed) and why

| Severity | Area | Finding | Why not fixed / recommendation |
|---|---|---|---|
| Low | WebUI | Localhost bypass behind a proxy that adds no forwarding headers | Cannot be detected; documented. Recommend refusing the bypass when the "reverse proxy" setting is on, or warning in the UI |
| Low | WebUI | No per-IP connection cap; session churn from Basic auth / bypass | Needs a connection-accounting change in `server.rs`; impact is availability for authenticated or local clients |
| Low | peers | No smart ban after hash failures | Bad data is never accepted (pieces are verified); only bandwidth is wasted. Recommend counting failures per address and banning after N |
| Low | peers | Slow-drip peers after the handshake | Recommend a per-message deadline in `read_buf.rs` (owned by the memory-reduction work in progress) |
| Low | metadata | Hybrid v1/v2 consistency is not checked | Needs a structural comparison of the two file lists (libtorrent rejects inconsistent hybrids); no local data or path impact |
| Low | DHT | No BEP 42; nodes added before answering; sequential tids | Already listed in SECURITY.md |
| Low | LSD | Unicast from non-LAN sources accepted; a receive error stops LSD | Recommend a LAN-source check and the DHT's `is_datagram_error` handling |
| Low | net | Core HTTP client has no private-address policy for feed/search/favicon URLs | Upstream parity; recommend the engine's guarded resolver for untrusted sources |
| Info | peers | Our own bitfield message cannot be sent for torrents with more than ~139k pieces (17 KiB write buffer), and a peer's bitfield over ~262k pieces does not fit the 32 KiB read buffer | Interop / robustness, not attacker-triggered. It lives in `peer_connection.rs` / `read_buf.rs`, which are being changed for memory reduction, so it was left to that work |
| Info | crates | `librqbit-utp` congestion/state machine not reviewed in depth | Third-party crate; parsing was reviewed |

## Commits (oldest first)

1. `4647910` Refuse the WebUI localhost bypass for requests forwarded by a proxy
2. `f0dd5cb` Count WebUI logins in progress against the ban limit
3. `1533c74` Reject base32 info hashes that decode to the wrong length
4. `94a0a0d` Bound DHT lookup fan-out from hostile nodes
5. `79928a8` Bound upload requests waiting in a peer's writer queue
6. `74e5926` Limit preemptive unchokes to one per rechoke interval per peer
7. `3dda48d` Refuse v1 torrents with file paths deeper than 64 components
8. `e9a0980` Make the state-changing WebAPI GET endpoints POST-only and refuse HEAD
9. `bdcee72` Rate-limit incoming DHT queries per source address
10. `ccf94ef` Keep RSS description rendering linear on hostile feeds
11. `8ae470b` Do not slice cookie date tokens inside a UTF-8 character
12. `c2c5402` Cap the file indices of a magnet so= parameter
13. `3218ea6` Bound UPnP device description fetches per discovery
14. `73a4bee` Cap the known peers a torrent takes from untrusted sources
15. `facdf67` Give WebUI login bodies a 10 s deadline

Verification:

* `cargo test -p qbt-core` passes;
* the engine crates' unit tests pass (librqbit, dht, core, lsd, tracker-comms,
  peer-protocol, bencode, upnp);
* the `qbt-e2e` swarm tests pass;
* `cargo clippy -p qbt-core --features ffi` (with `--tests` as well) and
  clippy on the touched engine crates report no warnings.
