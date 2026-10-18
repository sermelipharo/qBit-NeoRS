# Security notes

qBit NeoRS exposes several network-facing services and parses a lot of
untrusted input (torrent metadata, tracker replies, RSS feeds, search plugin
output, WebAPI requests). Every web/network feature gets a threat review and
an independent audit before it is considered done. This file records the
threat model and mitigations per component.

## Embedded tracker (`crates/qbt-core/src/tracker`)

Threats: memory exhaustion via many torrents/peers, slow or oversized
requests, use of the tracker for traffic amplification (announcing a third
party's address so swarms connect to it), malformed input crashing the
process.

Mitigations:
* Torrent and peer tables capped like upstream (10 000 torrents, 200 peers per
  torrent, oldest evicted).
* At most 256 concurrent connections (extra connections are dropped, not
  queued); 8 KiB request-head limit (413); 10 s per-connection deadline;
  `Connection: close` on every response.
* Only `GET /announce*`; anything else is 404/405. All parameters validated
  (20-byte info hash, peer id ≤ 20 bytes, port 1–65535, numwant ≥ 0, known
  events). Parsing is panic-free and covered by tests.
* The self-reported `ip` parameter is honoured only when the request comes
  from loopback/private/link-local/CGNAT/ULA addresses (upstream trusts it
  from anywhere). Public clients are always recorded with their socket
  address.

## RSS reader and auto-downloader (`crates/qbt-core/src/rss`)

Threats: hostile or compromised feeds (XML bombs, external entities, huge or
deeply nested documents, oversized fields, crafted dates and encodings),
feeds that try to make the client fetch or open local resources, crafted
rules (catastrophic or huge regexes, regex injection) and crafted import
files, path tricks in item names, and anything that could crash the
process or exhaust memory.

Mitigations:
* XML is parsed with `quick-xml`, which never processes DTDs: internal and
  external entity declarations are ignored, so there is no XXE and no
  entity-expansion ("billion laughs") attack. Only the five XML entities,
  numeric character references (NUL/surrogates/out-of-range rejected) and
  the fixed XHTML entity table are resolved; anything else stays literal.
* Limits: feed download and parse input ≤ 32 MiB (HTTP `max_size` + check
  before parsing), 60 s timeout, nesting depth ≤ 512, ≤ 8 M XML events,
  ≤ 10 000 articles per document, text per element ≤ 1 MiB, attribute values
  ≤ 64 KiB (longer ones are dropped), Atom `xml:base` ≤ 2 KiB, entity names
  in attributes looked up within 33 bytes (linear time), titles/ids/
  authors/extra fields ≤ 64 KiB (URLs longer than that are dropped, not
  truncated), ≤ 32 extra fields per article, ≤ 64 attributes per element.
  Stored articles are additionally capped by `rss_max_articles_per_feed`.
  Favicons ≤ 1 MiB.
* The parser is iterative (no recursion on document structure) and
  panic-free; tests feed it garbage, truncated, deeply nested, huge and
  pseudo-random documents, DTD bombs and broken encodings.
* Only `http`/`https` feed URLs can be added or set (rejected at
  `addFeed`/`setFeedURL`, checked again before every download); `file:`,
  `data:`, `javascript:` and scheme-less URLs are refused. A feed cannot mark
  its own items as read (`isRead` elements are ignored).
* The auto-downloader only acts on HTTP(S) URLs, magnet URIs and bare info
  hashes (`Article::isSupportedTorrentURL`); other torrent URLs are logged
  and skipped. Front ends must apply the same rule for manual downloads and
  refuse `file:` links for "Open news URL" (qBittorrent behaviour); article
  descriptions are untrusted HTML and must be rendered sandboxed/sanitized.
* Item paths: `\`-separated, non-empty segments, no `.`/`..` segments, no
  control characters, ≤ 16 KiB and ≤ 64 levels; names loaded from
  `feeds.json` go through the same validation. Paths are purely virtual:
  article and icon files are named only from the feed UID
  (`articles/<32 hex>.json|.ico`), never from feed-provided text or names.
  A feed title used for automatic renaming must also be a valid name.
* Rules: patterns are compiled with the linear-time `regex` crate (no
  backtracking, so no ReDoS) with a 2 MiB compiled-size limit, nesting limit
  and a 4 096-byte pattern cap; invalid or oversized patterns never match.
  Wildcard tokens are converted with every regex metacharacter escaped
  (only `*`, `?` and `[...]` sets keep their meaning) and episode-filter
  numbers are escaped before being embedded in a pattern (upstream inserts
  them raw). Rule names must be non-empty, ≤ 1 KiB, without control chars.
* Imports (`download_rules.json`, rules export files) are capped at 10 MiB,
  article files at 64 MiB and `feeds.json` at 10 MiB. The legacy
  `QDataStream` reader is bounds-checked: declared counts can never exceed
  the remaining bytes, nesting ≤ 16, unknown types are rejected.
* Corrupted files never prevent start-up: they are logged and skipped.
  Files are written atomically (temp file + rename) by a single ordered
  writer task.
* Same-host request delay (`rss_fetch_delay`) serializes requests per
  `host:port`, so feeds cannot be used to hammer a host faster than
  configured. Redirect handling, proxies and TLS options are those of the
  shared HTTP client (`net/http.rs`).

## Search engine (`crates/qbt-core/src/search`)

Search plugins are third-party Python programs: installing one is running
arbitrary code as the user, exactly like in qBittorrent. The goal is that no
input *other* than an installed plugin's code can get code executed, files
read/written/deleted, or the process exhausted. Untrusted inputs: WebAPI
parameters (pattern, category, plugin names, plugin sources, result URLs),
downloaded plugin files and `versions.txt`, plugin output (stdout/stderr,
capabilities XML, `nova2dl.py` output), favicons.

Threats: shell/argument injection into the Python command line, path
traversal through plugin names or download file names, deleting or reading
arbitrary files through `nova2dl.py` output or uninstall, memory exhaustion
from runaway plugin output or many searches, orphaned/zombie processes,
XML entity attacks, adding non-torrent URLs (`file:`, `javascript:`) to the
session.

Mitigations:
* Processes are spawned with an argv array only (`tokio::process::Command`,
  never a shell), stdin closed, `-I` (isolated: ignores `PYTHON*` variables,
  user site-packages and the current directory) and `-Xutf8=1`, working
  directory `data/nova3`. The search pattern is passed as separate argv
  entries after control characters (NUL included) were replaced; patterns
  are limited to 1024 characters. Categories must be one of nova2's nine ids.
* Plugin names must match `^[A-Za-z0-9_]{1,128}$` everywhere they reach a
  path or argv (install, uninstall, search, download, capabilities output,
  `versions.txt`, `config/search.json`). Uninstall only removes regular
  files/symlinks named `engines/<name>.*` (never following links).
* Plugin installation: sources must be `http`/`https` URLs, `file:` URLs or
  absolute paths ending in `.py`; other schemes are rejected. The plugin id
  comes from the URL's last path segment and must pass the name check, so a
  URL cannot choose the destination path. Downloads use the general-purpose
  proxy profile and a 2 MiB cap (`HttpRequest::max_size`; local files are
  capped the same way and must be regular files). The file must carry a
  valid `# VERSION: x.y` header and compile (`compile()` in a separate
  Python process, no code executed) before it is staged as
  `engines/.install-<name>-<random>.tmp` (`create_new`, never matched by
  nova2's `*.py` glob) and atomically renamed over `engines/<name>.py`; the
  previous version is kept as `.bak` and restored if nova2 cannot load the
  new one. Install errors only carry the last line of Python's error (type
  and message, not the offending source line), so installing arbitrary
  local files cannot be used to read them.
* Output parsing is panic-free and bounded: stdout lines longer than 16 KiB
  are discarded, at most 50 000 results / 32 MiB of result text per search,
  stderr capped at 64 KiB (4 KiB in logs), capabilities output at 4 MiB.
  Control characters in result fields are replaced. At most 5 searches run
  concurrently and 50 are kept (the oldest finished search is dropped).
  Capabilities XML is parsed with quick-xml, which does not expand DTDs or
  external entities.
* Every child runs in its own process group. Stop, delete, the 3-minute
  search timeout and shutdown send SIGTERM to the whole group (nova2's
  multiprocessing workers included), then SIGKILL after 3 s, and the child
  is reaped. The group is only signalled while the child is unreaped, so its
  id cannot have been recycled; a guard kills the group if a task is
  dropped. Helper processes (capabilities, syntax check, `nova2dl.py`,
  `--version`) have timeouts (30 s / 30 s / 120 s / 10 s).
* `download_torrent`: magnet links are added directly; only `http`/`https`
  URLs are otherwise accepted (no `file:` etc.). Through `nova2dl.py`, the
  returned path must be absolute, `lstat` as a regular file (not a symlink),
  and canonicalize inside the temporary folder (`$TMPDIR`, `/tmp`,
  `/var/tmp`); it is opened with `O_NOFOLLOW|O_NONBLOCK`, size-checked
  against "torrent file size limit" through the descriptor, read, then
  deleted, and handed to the session as bytes (no second path lookup). Files
  failing the location/type checks are neither read nor deleted; files over
  the size limit are deleted unread (by path).
* Favicons: only fetched from `http(s)` plugin URLs, 1 MiB cap, kept only if
  the bytes are PNG or ICO, written atomically as `engines/<name>.png|.ico`.
  Icon paths are not exposed through the WebAPI JSON.
* On macOS, `/usr/bin/python3` is skipped when it is the Command Line Tools
  stub (running it would pop up an installer dialog).

Notes for front ends: result `descrLink`/`siteUrl` values are plugin
controlled; the GUI/WebUI must refuse to open non-`http(s)` description
links (qBittorrent blocks `file:` URLs). A WebAPI user can install plugins
(= run code), as in qBittorrent; WebAPI authentication protects this.

## BitTorrent session (`crates/qbt-core/src/bittorrent`, `ipfilter.rs`)

Threats: hostile torrent metadata (from .torrent files, URLs, watched
folders and magnet peers) trying to write outside the save folder, crash or
exhaust memory; crafted magnet links; renames, category names and
AddTorrentParams paths used for path traversal; command injection through
the "run external program" hooks; watched folders used to read or delete
files elsewhere; oversized or malformed IP filter lists; leaking resume data
to other local users.

Mitigations:
* Bencode input is first checked by an iterative scanner with qBittorrent's
  `bdecode_depth_limit` / `bdecode_token_limit` (no recursion, no stack
  overflow, declared string lengths are bounded by the input) before the
  engine's decoder sees it. .torrent sources are capped at
  `torrent_file_size_limit` (URL downloads, files, bytes, watched folders,
  stored metadata); resume records at 16 MiB. Metadata fetched from peers is
  parsed the same way and must match the requested info hash.
* Every path component from metadata is sanitised (separators, `:`, NUL and
  control characters replaced, `.`/`..`/empty replaced, 255-byte cap,
  collisions after sanitising de-duplicated); the engine additionally rejects
  `..` and separators. User paths (rename file/folder, AddTorrentParams
  `file_paths`) must be relative with valid components (no `..`, `.`, empty,
  `/`, `:`, control characters), cannot turn a file into a folder or the
  reverse, and are applied on disk before being recorded. The core storage
  layer re-validates every relative path right before joining it to the
  save folder, so nothing can escape it even if a bad path got stored.
  Content removal and moves only touch the torrent's own files, do not
  follow a symlink as the last path component (intermediate folders are
  followed) and only remove folders that are empty (apart from junk files).
  Collision de-duplication is exact-match only: names differing in case or
  Unicode normalization (the same file on APFS) are not detected yet.
* Category names follow qBittorrent's rules (no `\`, no leading/trailing
  `/`, no `//`); implicit category folders made from `.`/`..` become `_`.
  Tags must be non-empty, without `,` or control characters.
* URL sources are limited to `http`/`https`, magnet links and bare info
  hashes (`file:` and other schemes are refused). Magnet parsing is done by
  the core with caps (1000 trackers/web seeds/peers, 4096 parameters); the
  `so` parameter is never passed to the engine (its range expansion is
  unbounded). Web seed URLs must be `http`/`https`/`ftp`.
* External programs run without a shell: the template is split into
  arguments first and placeholders are substituted inside each argument, so
  torrent names, comments, tags or tracker URLs cannot add arguments or
  commands; stdio goes to `/dev/null`. A substituted value can still look
  like an option (a torrent named `--exec=...`), and a template such as
  `sh -c "... %N"` re-introduces shell injection: templates should put `--`
  before placeholders and never wrap them in a shell.
* Watched folders: entries are listed with `lstat` semantics — symlinks
  (files and folders) are skipped, only regular files are read (opened with
  `O_NOFOLLOW|O_NONBLOCK` and type-checked through the descriptor, so an
  entry swapped for a link or FIFO after listing is refused; intermediate
  folders are still followed), recursion is
  limited to 8 levels and 1000 files per scan, `.torrent` files are capped at
  the torrent size limit and `.magnet` files at 1 MiB (10 000 lines).
  Unparsable files are renamed to `.qbt_rejected` after 5 attempts.
  Sub-folder names only become category/path components when they are
  valid names.
* Persistence (`BT_backup/*.torrent|.fastresume`, `categories.json`,
  `tags.json`, `stats.json`, `session_state.json`, the engine block list) is
  written atomically (temp file, fsync, rename) with mode 0600; corrupt or
  mismatching records are reported and skipped, never trusted.
* IP filters (`.dat`, `.p2p`, `.p2b`) are streamed: files over 512 MiB are
  refused, lines are capped at 4 KiB, at most 8 M rules are kept, P2B name
  strings are bounded and declared counts never drive allocations; parsing
  is panic-free and covered by tests with malformed input. Ranges are stored
  merged (8 bytes per IPv4 range). Manual bans are normalised IP addresses.
* Reverse DNS for the Peers tab uses a bounded cache (512 names, 32
  lookups in flight); peer client strings are stripped of control
  characters.
* Storage keeps at most 64 open file handles per torrent.
* Torrent ids (and so `BT_backup` file names) are 40 hex digits computed
  from verified hashes: a hybrid's switch to its v2-based id happens only
  when the metadata's v1 or truncated v2 hash equals the id it was added
  under, and never onto an id already in the list; the migration of old
  resume data renames only records whose `.torrent` parses and whose v1
  hash is the record's own id (or magnet records with a valid 64-hex v2
  hash). New files are written before old ones are removed.

## WebUI / WebAPI (`crates/qbt-core/src/webui`)

The WebUI exposes the whole application over HTTP(S). An authenticated
WebUI user is equivalent to the local user: preferences such as "Run
external program", search plugin installation, the alternative WebUI folder,
save paths and `app/getDirectoryContent` give code execution and file
system access by design (as in qBittorrent). The security goal is therefore
that nobody gets in without credentials, that a browser cannot be abused by
other sites (CSRF, clickjacking, DNS rebinding), and that unauthenticated or
hostile input can neither crash the process nor exhaust its memory.

Threats: credential guessing and brute force, session theft/fixation, cookie
leakage, spoofed client addresses (to bypass bans or localhost/whitelist
bypass), cross-site requests and DNS rebinding, clickjacking, path traversal
and symlink escapes in static files, reflected content and MIME sniffing,
request smuggling, slow-loris and memory exhaustion (huge bodies, many
connections/sessions/uploads), downgrade to plain HTTP, panics on malformed
input.

Mitigations:
* Authentication: every `/api/v2` endpoint except `auth/login` requires a
  session or a valid API key; unknown endpoints also answer 403 before
  authentication (a test iterates every endpoint). Private UI files are
  only served with a session.
* Passwords are stored as PBKDF2-HMAC-SHA512 (100 000 iterations, 16-byte
  random salt, qBittorrent's `salt:hash` format). Usernames are compared in
  constant time; PBKDF2 always runs (also for unknown users) and the
  derived key is compared in constant time. At most two verifications run
  at once, off the async workers, so login floods cannot stall the server.
  API keys (`qbt_` + 28 chars) are compared in constant time and only
  accepted in `Authorization: Bearer`; they cannot be used as a cookie.
* Sessions: 192-bit IDs from the OS CSPRNG, only ever created by the server
  (unknown/forged IDs never create a session, login always issues a new
  ID); stored by SHA-256 of the ID so lookups leak no timing; idle expiry;
  logout deletes the session server-side; at most 512 sessions (expired ones
  swept, least recently used evicted). Changing the WebUI username or
  password logs every cookie session out. Rotating/deleting the API key revokes
  its session immediately. A temporary password generated at start (no
  password configured) is never written to the log file; it is shown by the
  app / printed by the daemon only.
* Cookies: `QBT_SID_<port>`, `HttpOnly`, `Path=/`, `SameSite=Strict` when
  CSRF protection is on (upstream uses `Lax`), `Secure` when enabled and the
  origin is HTTPS (directly or `X-Forwarded-Proto: https` from a trusted
  proxy).
* Brute force: failed logins are counted per client address (per /64 for
  IPv6, so rotating addresses inside a prefix does not help) and the client
  is banned after `web_ui_max_auth_fail_count` failures for
  `web_ui_ban_duration` (Basic auth included). The ban list is bounded.
* Client address: forwarded headers (`X-Forwarded-For`, `-Host`, `-Proto`)
  are only honoured when the socket peer is in the trusted reverse-proxy
  list, and `X-Forwarded-For` is read right to left skipping trusted proxies
  (upstream takes the first global entry, which the client controls and can
  rotate to evade bans). The localhost bypass requires both the socket peer
  and the resolved client to be loopback (IPv4-mapped IPv6 handled) and no
  forwarding header (`X-Forwarded-*`, `X-Real-IP`, `Forwarded`, `Via`) in
  the request. A reverse proxy on the same host that adds none of these
  headers is indistinguishable from a local client: do not enable the
  localhost bypass behind such a proxy.
* CSRF and DNS rebinding: Origin/Referer must match the target host and port
  (qBittorrent rules; missing both is allowed for non-browser clients);
  Host header validation checks the port and the local address or the
  "Server domains" list. Domain patterns are matched against the whole host
  name (upstream searches unanchored, so `example.com` also matched
  `example.com.evil.net`). The default domain list `*` accepts any host:
  set real domains when the WebUI is reachable from a network. The
  authentication bypass (localhost bypass, subnet whitelist) is only granted
  when the Host header is an IP literal, `localhost` or matches an explicitly
  configured, non catch-all server domain, so a DNS-rebinding page cannot
  ride it (upstream grants it for any Host); users reaching a bypassed
  WebUI through a host name must list that name in "Server domains". State
  changing actions are POST-only per qBittorrent's method matrix (405
  otherwise); unlike upstream, `search/downloadTorrent` and
  `rss/setFeedRefreshInterval` are POST-only too (a cross-site `<img>` GET
  could add a torrent through the localhost bypass), and HEAD is refused
  on the API (it ran the action).
* Response headers on every response (errors included):
  `X-Content-Type-Options: nosniff`, `X-XSS-Protection`, `X-Frame-Options:
  SAMEORIGIN` and CSP `frame-ancestors 'self'` (clickjacking protection),
  qBittorrent's Content-Security-Policy, `Cross-Origin-Opener-Policy` and
  `Referrer-Policy` for the built-in UI, explicit `charset=UTF-8` on text.
  Errors and API messages are `text/plain`, never HTML. Custom headers from
  the preferences are validated and cannot set cookies or hop-by-hop /
  framing-of-the-message headers (`Set-Cookie`, `Content-Length`,
  `Transfer-Encoding`, ...). They can replace security headers such as
  `X-Frame-Options` or `Content-Security-Policy` (as in qBittorrent; only an
  authenticated user can set them). Download file names are sanitized.
* Static files: the built-in UI is embedded (no file system access);
  decoded paths containing `\`, NUL or `.`/`..` segments are rejected (so
  `%2e%2e`, `%2f` and `%5c` tricks are refused). The alternative UI folder
  is canonicalized; files must resolve inside it, be regular files, have no
  symlinked directory between them and the root, and be ≤ 10 MiB. Note
  that the alternative folder's `public/` part is served without
  authentication, like upstream. `${LANG}` is restricted to two ASCII
  letters before being substituted into HTML; translation markers are
  resolved at build time for the embedded UI.
* Transport and limits: HTTP/1.1 via hyper (no request smuggling through
  ambiguous lengths); 500 concurrent connections (extra ones are closed);
  7 s to send a request head, also the keep-alive idle timeout
  (slow-loris); 64 KiB head and 100 headers; bodies ≤ 64 MiB (≤ 64 KiB
  before authentication, and no body is read at all for unauthenticated
  requests to protected endpoints); 120 s body timeout; a global 512 MiB
  budget for in-flight bodies; ≤ 4096 parameters and ≤ 4096 multipart parts;
  uploaded `.torrent` files ≤ `torrent_file_size_limit`; JSON nesting
  limited by serde_json (128). Per-session state is bounded (metadata
  caches and search jobs: 64 entries; sync snapshots: one per session).
  `getDirectoryContent` returns at most 50 000 entries.
* HTTPS: rustls (TLS 1.2/1.3, AEAD suites only, aws-lc-rs), certificate and
  key ≤ 1 MiB each. If HTTPS is enabled and the certificate or key cannot be
  loaded, the WebUI does not start and reports the error (upstream silently
  falls back to plain HTTP).
* Files written on request: the torrent creator only writes absolute
  `*.torrent` paths that are not symlinks or special files; `export` and
  `saveMetadata` return bytes and write nothing. `setLocation` /
  `setSavePath` create directories like upstream (authenticated users only).
* Robustness: parameter, multipart, cookie, header, URL and info-hash
  parsers are panic-free and fuzzed with pseudo-random input in unit tests;
  durations from preferences are capped so time arithmetic cannot overflow.
  Log lines built from request data are truncated and stripped of control
  characters.
* Server-side requests: `torrents/add` and `torrents/fetchMetadata` download
  URLs given by authenticated users (qBittorrent feature); downloads go
  through the shared HTTP client with its size limit and proxy settings.

## Networking (`crates/qbt-core/src/net`)

Threats: hostile or compromised web servers (RSS feeds, .torrent URLs,
search plugin downloads, GeoIP mirror) sending huge or compressed bodies,
redirect loops or redirects to local schemes, cookie injection across
sites, credential leaks (proxy, SMTP, DynDNS passwords), mail header
injection through crafted torrent names, tampered GeoIP databases, and
local attackers racing on predictable temporary files.

Mitigations — HTTP client (`net/http.rs`):
* Only `http`/`https` URLs. Redirects are followed manually, at most 10
  (qBittorrent: 20); a redirect to any other scheme fails ("Redirect to
  unsupported or dangerous protocol"), a redirect to `magnet:` ends the
  request and returns the URI to the caller.
* `max_size` (default 64 MiB) is enforced up front from `Content-Length`
  and again while streaming, counting decompressed bytes (gzip is decoded
  by the client), so neither a lying length nor a zip bomb can exceed it.
* Timeouts: 30 s connect, 60 s to the response headers, 30 s without data,
  plus the caller's total `timeout_secs`.
* TLS certificates and host names are verified by the platform verifier;
  only "Ignore SSL errors" (`ignore_ssl_errors`) disables verification.
* Proxies apply per purpose (`proxy_rss`, `proxy_misc`, `proxy_bittorrent`).
  An enabled proxy with an invalid host/port fails the request instead of
  silently connecting directly; environment proxy variables are ignored.
  SOCKS4 is honoured (qBittorrent connects directly), so enabling a proxy
  never leaks traffic. Proxy credentials are percent-encoded in the proxy
  URL and scrubbed from error messages; URL user info is removed from every
  message and from the `Referer` header.
* Caller-supplied credentials (DynDNS basic auth, the add-torrent `Cookie`
  value) are only sent to the original scheme/host/port, never to a
  redirect target on another origin or over an https→http downgrade.
* Cookie jar (`net/cookies.rs`): RFC 6265 parsing with Qt semantics —
  host-only cookies match their host exactly; a `Domain` attribute must
  cover the request host, may not be a bare top-level name (there is no
  public-suffix list yet, so `Domain=co.uk` is accepted, unlike Qt) and is refused
  for IP hosts unless equal; `Secure` cookies only go over https; path
  scoping. Names/values with control characters or `;` are rejected (no
  header injection). Bounded: 4 KiB per cookie, 50 per domain (oldest
  evicted), 3 000 in total. Persisted to `config/cookies.json` with mode
  0600 through an exclusively created temporary file + rename; session and
  expired cookies are never written.
* `fetch_to_file` streams into an `O_EXCL` temporary file with a random
  name and renames it only for a complete 2xx response (quarantined when
  "mark of the web" is on).

Mitigations — SMTP (`net/smtp.rs`): sender/recipient addresses are parsed
as RFC 5322 addresses and refused when they contain line breaks; subjects
with line breaks are refused, and the completion mail replaces control
characters in torrent names. Bodies are base64 encoded. With "requires a
secure connection" the client uses implicit TLS (465) or mandatory
STARTTLS, otherwise opportunistic STARTTLS; certificates are always
verified. Passwords are only handed to the SMTP library and never logged.

Mitigations — DynDNS (`net/dyndns.rs`): fixed update hosts over https,
domain validated with qBittorrent's rule, query values percent-encoded,
credentials only in the `Authorization` header (never in the URL or logs);
replies are capped at 64 KiB; fatal service answers stop updates for the
session; state file written with mode 0600.

Mitigations — GeoIP (`net/geoip.rs`): downloads only from the fixed
`https://download.db-ip.com/free/dbip-country-lite-YYYY-MM.mmdb.gz` URL
(compressed cap 32 MiB, decompressed cap 128 MiB), streamed to a file and
unpacked in 64 KiB chunks into an exclusively created file next to the
database (never buffered in memory), then the whole database is verified
through a mapping (`Reader::verify`: tree, data section and metadata), its
type must be a country database and its build time newer than the loaded
one before it is renamed over `data/GeoDB/dbip-country-lite.mmdb`. The
file is memory-mapped and only ever replaced by rename (never modified in
place); lookups are bounds-checked.

Other: `utils::os::set_quarantine` refuses symlinks/non-files
(`XATTR_NOFOLLOW`) and `apply_mark_of_the_web` logs every failure;
`move_to_trash` only accepts absolute, existing paths.

### Test Port (`net/portcheck.rs`, Settings › Network)

What it does: `GET https://portcheck.transmissionbt.com/<port>`, the
service Transmission uses. It connects back to the address the request came
from on `<port>` and answers `1` (reachable) or `0` (not reachable).
Verified 2026-10-15: the endpoint serves https and the body is a single
digit. The Transmission project publishes no privacy policy for it.

Threats: disclosing the user's public IP address and listening port to a
third party; a hostile or hijacked endpoint (huge, slow or malformed
replies, redirects); the check being triggered without the user knowing.

Mitigations:
* Privacy: the check runs only when the user clicks **Test Port**. Nothing
  calls it on launch, on a timer or when settings change. The row's
  footnote says that the IP address and port are sent to
  portcheck.transmissionbt.com. No other data is sent (no torrent or
  profile data, the browser user agent like every other request).
* The URL is a constant with https; certificates are verified unless the
  user enabled "Ignore SSL errors". Only the port number is appended, and
  it is validated as 1–65535 in the core.
* The shared HTTP client applies: redirects only to http(s) and at most
  10, the general-purpose proxy when "Use proxy for general purposes" is
  on (the service then tests the proxy's address, which the footnote
  doesn't hide; an invalid enabled proxy fails the request instead of
  connecting directly).
* Response size is capped at 16 bytes (enforced while streaming, gzip
  counted after decompression) and the request is limited to 20 s in
  total. Anything other than a 2xx status with `0`/`1` is an error; the
  body is never shown to the user or logged.
* The shared cookie jar is used as for every request; the service sets
  no cookies today, and the jar's per-domain limits apply.
* Tests (`net::portcheck::tests`) run against a local server only.

## Engine networking (`engine/librqbit/src/net`, `session/networking.rs`, `i2p*.rs`)

Threats: hostile peers (malformed or endless encrypted handshakes, floods of
incoming connections), hostile trackers and torrents (I2P peer lists, web
seed URLs), local programs abusing loopback services, de-anonymisation when
a proxy or I2P is configured.

* MSE: DH keys, pad and initial-payload lengths are bounded by
  `librqbit-mse`; the receiver handshake runs under the read/write timeout and
  only after the IP filter accepted the address; the incoming torrent is found
  through a req2 → info-hash map (no per-torrent hashing per connection) and
  the encrypted info hash must match the BitTorrent handshake. Read-ahead
  buffers are bounded by the handshake limits; the encrypting writer holds at
  most 32 KiB.
* Incoming handshakes: at most 256 pending per listener, at most 4 per
  address (IPv6: per /64), each with an overall 30 s deadline; an accept
  error pauses accepting without stalling pending handshakes. The IP filter
  is checked again when the handshake completes.
* Uploads: a peer may have at most 500 chunk requests outstanding (advertised
  as `reqq`); excess requests are dropped and a peer that keeps sending them
  is disconnected. `Cancel` removes a request not sent yet (no rate limit
  tokens are spent on it); at most 32 `ut_metadata` answers are queued per
  peer. Accepted requests wait for the upload limits in the peer's writer
  (one at a time, FIFO across peers) and a peer cannot buy bandwidth ahead
  of others; choke, have and disconnect messages are not held behind
  waiting uploads. A cancelled request stays in the writer's queue until
  its turn, so at most 1000 requests (cancelled or not) may wait there;
  more count as dropped requests. A peer is unchoked preemptively (on
  becoming interested) at most once per rechoke interval, so toggling
  interest cannot queue choke/unchoke messages without bound.
* DHT: incoming queries are answered at most 25 times a second per source
  address (IPv6: /64; at most 4096 sources a second; loopback exempt), so
  spoofed queries cannot make us flood a third party. A lookup takes the 16
  closest nodes of each reply, asks each address once and queues at most
  256 requests per round; node and peer addresses from replies are
  filtered like PeX. A torrent takes at most 4000 known peers from
  trackers, DHT, PeX and LSD.
* LSD: malformed `Infohash` headers (base32 of the wrong decoded length)
  are rejected; they used to abort the process.
* UPnP: one discovery round fetches each SSDP location once and at most
  16 of them, at most 8 device descriptions at once.
* Download limits throttle the reading of piece data from the socket: a peer
  that sends faster than the limit (including data we requested before the
  limit was set, or data we never requested) only fills its own TCP window.
  While a download limit applies, our outstanding requests per peer shrink to
  about 3 s of that peer's rate (at least 2).
* Proxies: credentials go only to the configured proxy (Basic auth for HTTP
  CONNECT, SOCKS user/password) and are redacted from `Debug` output; with
  "proxy peer connections" incoming connections are refused, uTP (no UDP
  through proxies) and the extended handshake listen port are not used, and
  UDP trackers are skipped instead of bypassing the proxy. Web seeds use the
  proxy too. Proxy settings are validated (including building the proxy URL)
  before anything changes, and the tracker and web seed clients are built
  before the peer connector switches; if the core can't apply a proxy or
  network interface live, it restarts the engine, which fails closed (no
  session) while the setting can't be used. DHT and LSD are not proxied
  (upstream parity).
* Anonymous mode hides the client name, listen port and real peer id from
  peers and the user agent / `ip=` from trackers and the user agent from web
  seeds; it does not hide the IP address (use a proxy or I2P).
* IP filter and bans are applied before any byte is read from a connection
  and before an outgoing connection slot is used; IPv4-mapped IPv6 addresses
  can't bypass IPv4 rules (also for the URL block/allow lists). Filter
  changes disconnect matching peers at once. The "Blocked IPs" log records an
  address at most once a second.
* I2P: SAM is spoken only to the configured bridge; destinations and names
  from trackers or the API are validated (alphabet, length ≤ 1024) before use.
  The address book holds at most 65 536 destinations and then forgets the
  oldest; synthetic addresses start at a random index per session and are
  never reused (a persisted ban never names another destination). Synthetic
  (100::/64) addresses are only accepted from `.i2p` trackers, incoming I2P
  streams and manual peers, so clearnet trackers, DHT, LSD and PeX can't make
  us dial an I2P destination of their choice. At most 64 incoming I2P streams
  are checked at once. `.i2p` trackers go through a loopback proxy bound to
  127.0.0.1 that requires a random per-session password (compared in
  constant time, at most 32 connections) and only connects to `*.i2p` hosts
  through SAM (never clearnet, no DNS), so `.i2p` hosts are never resolved by
  the system resolver and local programs can't use our I2P session. With
  mixed mode off, I2P torrents refuse clearnet peers both ways and use no
  DHT/LSD, clearnet trackers or web seeds; synthetic I2P addresses never leak
  into PeX, and neither `yourip` nor our listen port is sent to I2P peers.
* Private torrents (BEP 27) never use DHT, LSD or PeX: no lookups or
  announces, `ut_pex` is not offered and PeX messages from peers are dropped,
  `ut_metadata` requests are refused; per-torrent DHT/PeX/LSD switches stop
  running lookups at once.
* uTP/TCP mixed mode: the throttles are derived from our own measured
  per-peer rates and never go below 5 kB/s up / 30 kB/s down, so a swarm of
  (possibly hostile) uTP peers can slow TCP peers down in peer-proportional
  mode but not starve them; LAN peers are not counted or throttled.
* Port mapping only maps the listen port and ports the core asks for; NAT-PMP
  answers are accepted only from the gateway address (connected UDP socket);
  UPnP mapping descriptions are sanitized.
* Trust (SSRF mitigation): only the trackers and web seeds of a .torrent file
  handed to the core directly (file, upload, creator, watched folder) and
  those the user adds or edits are trusted; torrents from URLs, RSS, search
  plugins, fetched metadata and magnets are not. The trusted URLs are
  persisted per torrent (`trusted_urls` in the resume data; older records
  trust nothing).
* Web seeds: see `docs/ENGINE.md` (SSRF rule for torrent-supplied URLs, a
  separate client for untrusted seeds that never resolves or redirects to
  local addresses, bounded bodies and concurrency, limited redirects, URLs
  never logged). The core keeps at most 100 single-line URLs per torrent.

## Torrent creator (`crates/qbt-core/src/creator`)

Threats: publishing files from outside the selected folder through
symbolic links, hanging on special files, unbounded memory on huge trees or
tiny piece sizes, the WebAPI (`torrentFilePath`) being used as an arbitrary
file write primitive, partial output files.

Mitigations:
* Symbolic links: libtorrent/qBittorrent read through them. Here a link is
  followed only when its canonical target is a regular file inside the
  (canonical) source folder; links to directories are never traversed (no
  loops) and links leaving the folder are skipped and listed in a warning
  log line. Files are opened with `O_NOFOLLOW|O_NONBLOCK` (resolved links via
  their checked target) and must still be regular files, so a file swapped
  for a link or FIFO during creation is refused; a file that shrinks fails
  the task.
* Hidden entries (`.` names, macOS hidden flag), FIFOs, sockets and device
  nodes are skipped as in qBittorrent. Non-UTF-8 names fail the task.
* Bounded resources: at most 1 000 000 files; the resulting .torrent size is
  estimated before hashing and must not exceed "torrent file size limit"
  (`torrent_file_size_limit`, default 100 MiB); hashing streams 1 MiB chunks
  and keeps only per-piece hashes. At most 256 tasks (finished ones evicted
  first); one task runs at a time; deleting a task interrupts it. Results up
  to 1 MiB are cached in memory within a 16 MiB budget.
* Output: `torrent_file_path` must be absolute and end with `.torrent`
  (refuses e.g. `~/.ssh/authorized_keys`); `source_path` must be absolute.
  The file is written atomically (exclusive random temporary file + rename,
  which replaces rather than follows a symlink at the destination). Without
  a path the result goes to `cache/torrent_creator/<id>.torrent` (mode 0600),
  deleted with the task and cleared at start-up.

## BitTorrent v2 (BEP 52) (`engine/librqbit_core/src/v2.rs`, `peer_info_reader/v2_layers.rs`, `torrent_state/live/v2_hashes.rs`)

Threats: v2 metadata (.torrent files, magnet metadata from peers) and BEP 52
hash messages from peers are attacker controlled: deep or huge file trees,
path tricks, piece counts far beyond the metadata size, piece layers that
don't match the files, forged hashes, request floods, panics on malformed
input.

Mitigations:
* File tree: bencode nesting is capped by the parser (128) before the tree
  is walked; paths ≤ 64 components, ≤ 2^20 files; components must not be
  empty, `.`, `..`, or contain `/`, `\` or NUL (and are sanitised again by
  the storage layer); a file node may hold nothing but its `""` entry;
  lengths are non-negative and summed with overflow checks; non-empty files
  need a 32-byte pieces root; duplicate paths are refused; piece length must
  be a power of two ≥ 16 KiB.
* Piece count: pure v2 torrents are limited to 2^21 pieces (identical files
  share one layer, so the metadata size alone wouldn't bound the 32-byte
  per-piece hash table).
* Piece layers: each layer must be exactly `pieces × 32` bytes and hash up to
  its file's pieces root before it is used (each distinct root once, so
  repeated files can't multiply the work); a pure v2 torrent with a bad
  layer (wrong size, wrong root, not a byte string) is refused. A missing
  layer is fetched from peers (below) while the torrent runs; until it is
  known and verified, no piece of that file is requested, counted as had
  or served (see "Pure v2 torrents missing piece layers" below).
* Verification never trusts sizes from peers: a v2 piece hash is computed
  over the real file bytes of that piece only and must also cover exactly
  the expected number of bytes.
* Magnet metadata of a `btmh` link is checked with SHA-256 against the full
  v2 hash. Piece layers fetched afterwards (and the missing layers of a
  pure v2 .torrent, whose info dict is already verified by its hash): only
  replies to requests we sent (exact match of root, layer, index, length,
  proof layers) are accepted, with exactly `length` + the uncle count
  (libtorrent's proof layer counting, `v2::proof_uncles`) hashes, and they
  must verify against the pieces root from the verified metadata; at most
  4 requests in flight, at most 512 hashes per request; any reject or bad
  reply drops the peer. A magnet's layers are validated again as a whole
  dictionary before use; a running torrent's are checked again file by
  file against the root when added (`V2PieceHashes::set_layer`). Layers
  are spliced into the stored .torrent keeping every other key's raw
  bytes, so the info hash can't change.
* Memory of fetches from untrusted peers (`peer_info_reader/budget.rs`;
  a hostile torrent or swarm must not push us to gigabytes: metadata can
  be 32 MiB, one layer 64 MiB, and up to 128 peers are tried per magnet):
  * One session-wide budget (`FetchBudget`, 192 MiB) for BEP 9 metadata
    pieces and BEP 52 piece layers being downloaded. Every byte kept
    (data and bookkeeping) is reserved first and returned when the
    download ends or is dropped; a download that would go over it fails
    instead of allocating (a magnet's peer is dropped; a hybrid's layer
    fetch starts over later without blaming the peer).
  * Metadata (BEP 9): the announced `metadata_size` (1..=32 MiB, else the
    peer is dropped) allocates nothing but a table of at most 2048 piece
    slots; each piece is checked (index, exact size, not a duplicate)
    before it is charged and kept, the info hash (SHA-1, or SHA-256 for
    `btmh`) is computed over the pieces when the last one arrived, and
    only verified metadata is joined into one buffer. Unsolicited pieces
    (before we asked) drop the peer. At most 2 peers per magnet download
    the metadata at once (`MAX_METADATA_FETCHES_PER_TORRENT`); the others
    wait for a slot with nothing requested or allocated.
  * Piece layers: requests are generated as the fetch goes (nothing per
    file up front), a file's hashes are kept as verified chunks arrive (a
    bad reply is refused before anything is kept), at most 2 peers per
    torrent fetch at once (`MAX_LAYER_FETCHES_PER_TORRENT`), the others
    wait.
  * Tests: concurrent reservations never exceed the limit; 20 peers
    sending 1 MiB of metadata and 50 fetching one large layer stay within
    budgets of 1.5× (the excess fails, everything is returned); the largest
    announced size allocates only the slot table; malformed metadata
    pieces and garbage hash replies are never charged; forged metadata is
    caught at the end and released; a third peer waits for a metadata or
    layer slot; a hybrid fetch stops at the budget; e2e checks that the
    budget is back to 0 after real fetches.
* Hybrid torrents without layers (added by a magnet link) fetch them while
  they run, from one v2 peer at a time (a connection made with the v2 hash
  or upgraded to it): same request and verification rules as above; replies
  from any other peer are ignored, an unsolicited, malformed or unverifiable
  reply from the fetching peer drops it, a reject ends its turn. A peer
  that failed isn't asked again (at most 64 remembered, then forgotten all
  at once) and a fetch that stalls for 60 s is handed to another peer, so a
  silent or hostile peer can't block it. The layers are only used to answer
  hash requests (pieces are still verified with SHA-1); the piece table
  is built once, off the async thread, after the whole dictionary verified.
* Pure v2 torrents missing piece layers (`torrent_state/missing_layers.rs`,
  `live/v2_missing.rs`, `live/v2_hashes.rs`; started at once like
  libtorrent, self-review 2026-10-16). Threats: unverifiable data getting
  into the have bitfield (and served on to others), a peer feeding us
  pieces or hashes of a file we can't check yet, a swarm or crafted
  .torrent making us spend memory, CPU or disk reads without bound.
  * No unverifiable data is ever had: pieces of a file without its layer
    are never requested (held back from peers, steals, streaming
    read-ahead and web seeds); a block of one that arrives anyway is
    unsolicited and drops the peer (as before). The initial check, a
    recheck, "skip hash check" and fast-resume bits never count them as
    had: claimed bits are taken out at start (a tampered fast-resume file
    gains nothing). libtorrent instead downloads them and keeps them
    "finished but unverified" until the hashes come; we chose not to hold
    data we can't check (same result once the layer is there).
  * A layer is used only once the whole file's layer verified against the
    `pieces root` from the hash-verified metadata (each chunk with its
    proof, then the whole layer again in `set_layer`), from the one peer
    fetching (one per torrent at a time, so within the
    `MAX_LAYER_FETCHES_PER_TORRENT` limit; replies from other peers are
    ignored), from a whole file on disk that hashes up to the root, or
    from a .torrent the user adds again (`add_piece_layers`: every entry
    checked, all or nothing). Known hashes are never replaced or removed;
    only known layers are served.
  * Memory: the piece hash table is allocated once, like a complete
    torrent's (32 bytes per piece, ≤ 2^21 pieces); one bit per file
    ("known") and two per piece (held back, unverified data on disk); a
    fetch keeps only verified chunks charged to the session `FetchBudget`
    and hands each finished file's layer out at once (its reservation is
    returned), so a failed fetch keeps the files it finished; the check's
    `LayerFinder` holds one file's piece roots at a time (at most that
    file's layer). A lookup index from root to files (4 bytes per file,
    built once) keeps adding a layer from walking every file (a crafted
    .torrent may have 2^20 of them).
  * CPU and disk: a file's data on disk is hashed again only when its
    layer first becomes known (`set_layer` reports only newly known
    files), in one task per arrival that ends with the torrent and uses
    the session's limit on concurrent checks; a peer can't make us rehash.
    Hashing whole files to find their layer happens during the check we
    run anyway (one read) or, on resume, only for files the fast-resume
    data claims complete. No per-torrent timers: the 60 s stall of a fetch
    is checked on the next message.
  * Tests: hostile, unsolicited and tampered hash replies; budget returned
    after file-by-file fetches and failed fetches; bad layers refused
    (`set_layer`, `add_piece_layers`); fast-resume bits of unverifiable
    pieces dropped; damaged data on disk found by the check and downloaded
    again; e2e and libtorrent interop with a layer arriving late.
* Connection upgrade (hybrid): a peer that uses our v1 hash with the v2 bit
  is answered with the v2 hash, and a peer may answer our v1 handshake
  with the v2 hash; no other hash is accepted (`accepts_other_info_hash`
  only knows the torrent's own two hashes, both from its metadata), and an
  upgraded connection gains nothing but being asked for and served hashes.
* Serving hash requests: only the piece layer of a file we know, request
  length a power of two in 1..=512, aligned index within the padded width,
  `proof_layers` below the tree height (so a proof never goes past the
  root and a reply is at most 512 + 32 hashes); anything else is rejected.
  Each request costs at most a few hundred hashes (the upper tree of a file
  is built once, about 1/256 of its layer, off the async thread); at most 8
  replies (~17 KiB each) queue per peer and further requests are dropped.
  For a hybrid torrent the v2 half is parsed from its own .torrent on the
  first request (same file tree limits, layers verified against their
  roots; once per torrent, off the async thread, a failure is remembered as
  "reject everything"), so a peer can't make it repeat the work.
* Short block requests (libtorrent asks for the last block of a file's data
  without the padding): accepted only for pure v2 torrents, only for the
  block that ends exactly at the piece's real data, shorter than the
  block; the reply is read from the file's data only and counts against
  the upload limits with its real size. Every other length is still a
  protocol error.
* Hybrid torrents under two hashes: the second hash comes from the
  metadata (SHA-256 of the info dict), never from peers; lookups of
  incoming handshakes (plain and MSE `req2`) accept either hash of a torrent
  and the index entries go when the torrent is removed. A torrent can't
  claim another one's hash: adding checks both hashes against both hashes
  of every torrent. Announcing under both hashes doubles tracker requests
  and DHT/LSD lookups per hybrid (as libtorrent), with the same per-tracker
  timeouts and response limits, in the same per-torrent job (no new
  tasks). Addresses learned under the v2 hash are kept in a set bounded
  like the peer source registry (16 Ki addresses per torrent, cleared on
  restart); a peer gains nothing by being in it but the hash we greet it
  with.
* Wire parsing: hash messages have a fixed 48-byte header, the hashes part
  must be a multiple of 32 bytes and fit the 32 KiB read buffer; hash
  messages from a peer that isn't fetching are ignored on live torrents and
  drop metadata connections.
* Malformed input is an error, never a panic (unit tests cover malformed
  trees, deep nesting, overflowing lengths, too many pieces, tampered or
  truncated layers, forged and unsolicited proofs).

## Widgets (`apps/macos/QBTNeoWidgets`, `apps/macos/WidgetShared`)

Threats: other local processes driving the app through the widgets'
channel, a tampered snapshot file crashing or confusing the widget, torrent
names leaking while the UI is locked.

Mitigations:
* The extension is sandboxed and only reads the App Group container
  (`~/Library/Group Containers/YWA2X8G6VY.mk.lex.qbitneors`); it never
  touches the core, the profile or the network.
* Widget buttons post Darwin notifications, which carry no payload: only
  six fixed requests exist (start all, stop all, alternative limits on or
  off, "a widget exists", "a torrent command is queued"). Any local process
  can post them, so the app acts on them only while widgets are installed,
  "Update widgets" is on and the UI is not locked, and at most once per
  second per request. A local process could already do the same through the
  app's own preferences or the WebUI.
* Torrent widget commands (start or stop one torrent) are files in
  `<group>/commands/`, one JSON object per file, written atomically by the
  extension, which stops at 16 pending files. On the notification the app
  drains the folder: it deletes every entry, reads at most 16 regular files
  of ≤ 1 KiB (anything else is deleted unread, symbolic links are not
  followed as regular files), and `WidgetCommandValidator` accepts a command
  only if its hash is well formed (1–64 lowercase hex characters), names a
  torrent that exists **and** is pinned by an installed Torrent widget, was
  issued within the last 60 s (and not more than 5 s in the future), and is
  the newest for that torrent; each torrent accepts one command per second.
  Commands are drained and dropped while the UI is locked, updates are off
  or no widget is installed. The only effect is start or stop of a torrent
  the user put on a widget.
* `qbitneors://torrent/<hash>` links only select an existing torrent in the
  main window; anything that is not exactly that form (hex hash, one path
  component) is ignored, and nothing is added or changed.
* The app never reads the snapshot back. The widget decodes it only up to
  256 KiB, rejects other format versions, and every field is bounded when
  written (at most 90 listed torrents, 12 pinned torrents with 60 samples
  each, 200 picker entries with names ≤ 48 characters, names ≤ 64
  characters, tags, locations and error messages truncated, 60 speed
  samples, 16 categories); the worst case is about 105 KB.
* While the UI is locked the snapshot carries speeds and counts only, no
  torrent or category names, no pinned torrents and no picker entries, and
  the widgets' buttons are disabled.

## Audit (2026-10-15)

Independent review of every network-facing component on `main`: WebUI
(auth, sessions, CSRF/Host, proxies, limits, static files, parsers, all
controllers), preferences, embedded tracker, RSS, search, `net/*`, creator,
BitTorrent session, the engine (peer protocol, metadata, bencode, trackers
and SSRF policy, DHT, UPnP, LSD, storage paths) and the macOS front end
(RSS HTML, link opening, URL/document handlers). Fixed findings have a
regression test unless noted. Note: `panic = "abort"` is set for every
profile, so any panic reachable from network input terminates the app.

### Fixed

| Severity | Area | Finding | Fix |
|---|---|---|---|
| Critical | engine peers | `ut_metadata` request for piece `total_pieces` sliced past the info dict: any peer crashed the app | bounds check (`metadata_piece_range`) |
| High | engine metadata | `ut_metadata` data before the extended handshake hit an `unwrap` (crash) while resolving magnets | protocol error instead |
| High | engine metadata | repeated extended handshakes each queued 2048 requests on an unbounded channel | first handshake only; size 0 refused |
| High | engine metainfo | piece length 0 (divide by zero), file-length overflow, tiny piece length with huge size (GBs of per-piece state) from .torrent or magnet metadata | piece length > 0, checked sums, `pieces` must hold exactly one hash per piece |
| Medium | WebUI | DNS rebinding + localhost bypass / subnet whitelist = unauthenticated WebAPI (and code execution) from any web page | bypass requires an IP-literal / `localhost` / configured-domain Host |
| Medium | engine bencode | unchecked integer parsing: overflow panic (debug) or silent wrap (release) | checked parsing |
| Medium | engine PEX | one sender task per repeated extended handshake; unlimited, unfiltered PEX addresses (loopback/LAN/multicast), dropped peers added | one task, 1 msg/45 s, ≤ 50 added, special/more-local addresses refused, dropped ignored |
| Medium | RSS | Atom `xml:base` copied into every link (400 KB feed → GBs); quadratic attribute unescaping | caps on base and attributes, bounded entity lookup |
| Medium | macOS RSS | blacklist sanitizer bypassable (`<im<img>g`, unclosed `<style>`, `<p/style=...>`): remote loads from article HTML | allowlist rebuild (`RSSHTMLSanitizer`); tested ad hoc with `swiftc` (no Swift test target exists) |
| Medium | engine trackers | SSRF vetting cached per host name: one allowed LAN address let every local address (127.0.0.1, rebinding) through | vetting pinned to the checked addresses; multicast/broadcast refused; site-local, IPv4-compatible and NAT64 forms count as local |
| Medium | engine DHT | `get_peers` returned up to 1000 peers (60-180x reflection); stored peers never expired | ≤ 100 random fresh peers, one entry per IP, 30 min expiry |
| Medium | engine UPnP | LAN host could point Location/controlURL anywhere (incl. localhost), stream endless or deeply nested XML (stack overflow), spawn unlimited service tasks | same-IP http URLs only, 256 KiB / 10 s / depth 32, no redirects, ≤ 8 services |
| Low-Med | engine LSD | announces outlived their torrent (removed/stopped/private torrents kept being multicast) | stream drop cancels the task |
| Low | WebUI | ban evasion by rotating IPv6 addresses | bans per /64 |
| Low | WebUI | sessions survived a password/username change | cookie sessions cleared |
| Low | macOS | torrent-comment links opened any scheme (`file:`, `smb:`, app schemes) | http/https/ftp only (tested ad hoc) |
| Low | session | watched-folder entry swapped for a FIFO blocked the session timer; symlink followed | `O_NONBLOCK`, `O_NOFOLLOW` |
| Low | session | newlines in torrent names / magnet `dn` forged log lines | names single-line |
| Low | net | feed URL credentials logged via `HttpResponse.url` | URL redacted |
| Low | session | `.torrent` URL downloads had no total timeout | 300 s |
| Low | engine peers | incoming peer's handshake chose the IP we reconnect to (reflection) | only the port is used (no test) |
| Low | utils | `write_atomic`: predictable temp name, follows planted links, preferences with passwords 0644 | exclusive random temp file, 0600 |
| Info | engine | `create_torrent` added a bogus extra piece hash for piece-aligned content (found by the new validation) | fixed |

### Open (recommendations)

| Severity | Area | Finding | Recommendation |
|---|---|---|---|
| Low | engine trackers | vetting is per host and address, not per URL: a query-less tracker URL can vet a LAN address later used by a URL with a query on the same host | pin resolved addresses per request (`resolve_to_addrs`) |
| Low | session | macOS quarantine applied when the whole torrent completes, not per file (upstream: per file) | quarantine each file on completion |
| Low | net | cookie jar has no public-suffix list (`Domain=co.uk` accepted) | add a PSL check |
| Low | RSS | rule limits are per pattern; many alternatives/tokens per rule and O(n²) imports | cap tokens and expression bytes per rule; batch imports |
| Low | search | oversized `nova2dl.py` files are deleted by path; dev/ino not compared with the `lstat` | compare dev/ino, do not delete on size failure |
| Low | engine | PEX sends LAN peer addresses to Internet peers | filter before building the message |
| Low | engine DHT | sequential 16-bit transaction IDs; nodes added to the table before they answer | random IDs; add on response only |
| Low | session/engine | case/Unicode-normalization collisions of file names (one disk file on APFS) | compare a case-folded NFC key |
| Low | engine | peer client names unbounded (~32 KB, bidi controls); per-message `warn!` log flooding from peers | cap/strip names, rate-limit logs |
| Low | logger | control characters in RSS titles and plugin stderr reach the log unescaped | escape in the logger |
| Info | net | SMTP uses opportunistic STARTTLS without "requires secure connection" (upstream parity): credentials can be stripped | refuse AUTH on an unencrypted connection |
| Info | search | proxy credentials are not percent-encoded in the Python environment (upstream parity; only a robustness issue) | encode for `http_proxy`, keep raw for the SOCKS URL nova reads |

The two Medium items of this list (upload request cap, torrent source
trust) and the Info item about the then unlinked engine-net crates were
handled in the second pass below.

### Second pass: engine-net (2026-10-15)

Review of the code merged after the first pass: MSE wiring (TCP, uTP,
proxies, I2P; req2 map, plaintext fallback, handshake limits), the RC4
stream wrappers, peer proxies and the tracker/web seed `reqwest` proxies,
anonymous mode, live listener rebinding, the IP filter and bans on every
path and the blocked-peer callback, NAT-PMP/UPnP and extra port mappings,
web seeds as pseudo-peers, I2P (SAM, loopback `.i2p` proxy, synthetic
addresses, mixed mode, accept loop) and the qbt-core preference wiring and
web seed persistence. Every fix has a regression test.

#### Fixed

| Severity | Area | Finding | Fix |
|---|---|---|---|
| Medium | engine peers | (first pass) upload requests queued without a per-peer cap, `Cancel` ignored | ≤ 500 outstanding (`reqq`), excess dropped, flooders disconnected, `Cancel` honoured, ≤ 32 queued `ut_metadata` answers |
| Medium | session/engine | (first pass) every torrent trusted (`trackers_local_source = true`): the SSRF rule never applied to torrents from URLs, RSS, search or magnets | trust only local .torrent sources and user additions, per URL, persisted (`trusted_urls`) |
| Medium | engine web seeds | torrent-supplied web seeds were always trusted by the core path; the SSRF check was name-based only (a public name redirecting to `127.0.0.1/api?x`, or resolving to a LAN address, got through) | untrusted seeds use a client whose resolver drops local addresses and whose redirects can't reach local addresses |
| Medium | engine web seeds | 100 seeds × full-file replies: up to 64 MiB buffered (16 MiB preallocated) per fetch, all seeds at once (GBs per torrent) | ≤ 4 fetches per torrent, ≤ 16 per session, skipped bytes discarded while streaming, overall fetch deadline |
| Medium | engine I2P | clearnet peers (PeX), trackers, DHT or LSD could hand out synthetic I2P addresses and make us dial an I2P destination of their choice (de-anonymisation) | 100::/64 only from `.i2p` trackers, incoming I2P and manual peers |
| Medium | engine I2P | with mixed mode off, I2P torrents still announced/scraped clearnet trackers and fetched web seeds over clearnet | skipped ("I2P mixed mode is off") |
| Medium | engine I2P | address book filled permanently by throwaway destinations (every later I2P peer refused, ~64 MB) | oldest entry forgotten, indices never reused and random per session |
| Medium | engine listener | one host could hold all 256 pending incoming handshakes with silent connections | ≤ 4 per address (/64), 30 s overall deadline, accept errors don't stall pending handshakes, filter rechecked after the handshake |
| Medium | core/engine proxy | a proxy host accepted by validation but not by URL parsing (e.g. `host:port` typo) switched peers to the proxy but left trackers and web seeds direct (IP leak), and the core never retried | proxy URL validated first, clients built before anything changes; core restarts the engine on failure (fails closed) |
| Medium | core | a missing network interface (VPN down) on a live change left traffic on the default route and was recorded as applied | same fail-closed restart |
| Low-Med | engine I2P | clearnet listen port sent to I2P peers in the extended handshake | not sent |
| Low | engine I2P | unbounded I2P accept tasks; unbounded loopback proxy connections; non-constant-time password check | 64 pending, 32 connections, constant-time compare |
| Low | engine I2P | a persisted ban on a synthetic address hit a different destination after restart | random per-session index base |
| Low | engine web seeds | anonymous mode still sent the client name as web seed User-Agent | no User-Agent; clients rebuilt on change |
| Low | engine web seeds | `.`/`..` path components from the metadata left the seed directory after URL normalization; IP filter not applied to seeds | dot segments refused; filter checked for literal addresses |
| Low | core | unbounded web seed lists with embedded newlines (log forging, resume bloat) | single-line, ≤ 4096 bytes, ≤ 100 per torrent (also on resume load) |
| Low | core | blocked-peer log flooded by one reconnecting host | once per address per second |
| Info | engine | `ProxyConfig` `Debug` included the password (latent); outgoing URL block/allow lists not checked on IPv4-mapped form | redacted; canonical address |

#### Open (recommendations)

| Severity | Area | Finding | Recommendation |
|---|---|---|---|
| Low | engine MSE | each encrypted incoming attempt costs two 768-bit `modpow`s on an async worker (96 bytes from the attacker) | parse the prime once, run DH in `spawn_blocking` or rate-limit encrypted accepts |
| Low | engine I2P | mixed mode is evaluated when a torrent's DHT/LSD streams start: switching it off, or adding an `.i2p` tracker, keeps DHT/LSD announces until the next restart of those streams (peers are still refused) | restart discovery streams on `set_i2p` and tracker-list changes |
| Low | engine I2P | the same peer id is used on clearnet and I2P unless anonymous mode is on | use the anonymous peer id (and no `v`) on I2P connections |
| Low | engine web seeds | IP filter only checked for literal seed addresses; untrusted seeds on names resolving to LAN addresses are refused even without a query string (stricter than libtorrent); with a proxy, names are resolved by the proxy (only literal checks apply) | filter in the resolver; document |
| Low | engine web seeds | a banned seed (3 hash failures) is retried after a restart | persist bans |
| Info | engine MSE | `MseWriter` relies on callers retrying a pending write with the same data (true for `write_all` today) | document / `debug_assert` |
| Info | engine NAT-PMP | external IP and lifetimes from the gateway are trusted (inherent to NAT-PMP) | none |
| Info | core | a non-loopback SAM host carries the I2P session (and private destination) in clear (user configuration, upstream parity) | UI hint |
| Info | core | resume records from before this pass have no `trusted_urls`: their trackers and web seeds become untrusted (LAN trackers with query strings must be re-added by the user) | intended |

Dependencies: `cargo audit` is not installed; `Cargo.lock` was checked by
hand against known advisories (hyper 1.12, h2 0.4.20, rustls 0.23.45,
rustls-webpki 0.103.15, ring 0.17.14, aws-lc-rs 1.18.2, openssl 0.10.81,
quick-xml 0.41, lettre 0.11.23, reqwest 0.13.5, tokio 1.53.2, idna 1.1,
url 2.5.8, crossbeam-channel 0.5.17, slab 0.4.12, tracing-subscriber 0.3.23,
quinn-proto 0.11.19, time 0.3.55): none is in a known-vulnerable range.

## Audit (2026-10-17)

Third review of every network-facing component, against a list of known
vulnerability classes in torrent clients: see `docs/security-audit.md`.
