# qBittorrent 5.3.0 gap checklist

Date: 2026-11-01. This lists every entry of the qBittorrent v5.3.0 changelog
(plus the WebAPI changes up to 2.16.2 and two unlisted changes: the Lua
plugins engine and the revised search plugin install procedure) and says
whether qBit NeoRS already covers it. NeoRS targets parity with 5.2.4
(`docs/PARITY.md`); this file is the delta to 5.3.0.

Legend: ✅ done (evidence cited) · ⬜ todo · ➖ not applicable.

Things to know before reading:

* The bundled Web UI (`crates/qbt-core/webui/src`, vanilla TypeScript built
  into `webui/www`) is NeoRS's own code, not a copy of qBittorrent's
  `src/webui/www`. Upstream `WEBUI:` changes cannot be copied over as files.
  The ones marked todo need the same feature written in the NeoRS sources;
  the upstream diff (`git show <hash>` in the qBittorrent repo) is the
  behavioural reference. Only the optional alternative-WebUI folder runs
  qBittorrent's own WebUI unchanged.
* `WEBAPI_VERSION` is `2.15.1` (`crates/qbt-core/src/model/mod.rs:33`);
  bump to `2.16.2` once the WebAPI rows below are done.
* The bundled search engine (`crates/qbt-core/resources/nova3`) is a copy of
  upstream's `src/searchengine/nova3`, so search script fixes are ports of
  upstream diffs.
* Paths below are relative to `crates/qbt-core/src` unless they start with
  `apps/`, `engine/` or `crates/`.

## Security

| PR | Change | Status | Notes |
|---|---|---|---|
| #24792, #24970 | Block cross-site requests by `Sec-Fetch-Site` | ✅ | Handled earlier (`webui/webapp.rs:455`, `webui/security.rs:76-82,404`) |
| #24889 | `X-Forwarded-For` spoofing auth bypass | ✅ | Handled earlier (right-to-left walk) |
| #24458 | Anchored server-domain matching | ✅ | Handled earlier |
| #24791 | POST-only `search/downloadTorrent`, `rss/setFeedRefreshInterval` | ✅ | Handled earlier (`webui/api/mod.rs` lists both as `P`) |
| #24793 | SMTP header injection | ✅ | Handled earlier (`net/smtp.rs:259` test) |
| #24805 | HTTPS DynDNS, no https-to-http redirects | ✅ | Handled earlier |
| #24869 | Unauthenticated announce created a torrent entry | ✅ | Handled earlier |
| #24720 | WebUI sessions count limit | ⬜ | Core pref `web_ui_sessions_count_limit` (default 15) replacing hardcoded `MAX_SESSIONS = 512` (`webui/session.rs:24`); WebAPI get/set; field in the Swift remote-access settings and the WebUI settings page |
| #24379 | Allow cross-origin links to the webroot | ⬜ | Skip the CSRF check for `/` and `/index.html` only (`webui/webapp.rs:451`); today a cross-site link gets 401 |
| #23838 | Fix STARTTLS; replace the SSL checkbox by an encryption type (None / STARTTLS / SMTPS) | ⬜ | Replace `mail_notification_ssl_enabled` (`prefs/api_json.rs:229,542`) with `mail_notification_encryption_type`, migrate the old value, adjust `net/smtp.rs` (its header comment describes the old rule), Swift picker. WebAPI 2.16.0 |
| #24512 | Drop unsupported tracker URLs from magnets | ⬜ | `parse_magnet` (`bittorrent/metainfo.rs:525`) keeps any `tr`; keep only `http`, `https`, `udp` with a host. Unit test |
| #24087 | Do not load a zero-size GeoIP database | ✅ | `net/geoip.rs:95-110` turns a failed open into a warning; add a test with an empty file to be sure |

## Core/engine

| PR | Change | Status | Notes |
|---|---|---|---|
| #24043 | Option to match all share limits (`MatchAny`/`MatchAll`) | ⬜ | Not present (no `share_limits_mode` anywhere). Core: per-torrent, per-category and global mode in `bittorrent/share_limits.rs`; prefs; WebAPI; Swift picker in the Share limits dialog and Downloads prefs |
| #24266 | "Manage content" (Torrent Content Layout) with batch editing of file names | ⬜ | New Swift dialog from the transfer-list menu: edit many file/folder names at once. Core rename exists (`bittorrent/engine.rs:848`) |
| #23386 | Enhance file name validation | ⬜ | Small: `rename_path` only checks the last component (`webui/api/torrents.rs:1114-1125`); validate every component of the new path, reject `.`/`..`. Category paths are already safe (`bittorrent/paths.rs` `to_valid_path`) |
| #23157 | Allow banning an IP range | ⬜ | `parse_banned` accepts single IPs only (`ipfilter.rs:412`); accept ranges, log rejected entries; Swift `IPAddressParser` and `AddressListSheet` |
| #24195 | IPv6 `/128` is a single IP | ✅ | `webui/subnet.rs:74` (mask math handles 128); `IPAddressParser.swift:29` |
| #24680 | P2P filter parsing for IPv6 | ✅ | `ipfilter.rs:288-300` falls back to earlier colons |
| #24734 | Log IP ban alerts | ⬜ | Small: log a line when the engine bans a peer on its own; check the engine reports automatic bans (only the manual path, `bittorrent/engine.rs:556`, was found) |
| #24158 | `seeding_outgoing_connections` setting | ⬜ | Pref (default true); needs engine support to stop dialling peers while seeding (`engine/librqbit`), otherwise inert |
| #24253 | `max_outstanding_block_requests` (libtorrent `max_allowed_in_request_queue`) | ⬜ | Low: pref + cap on queued incoming requests per peer in the engine. NeoRS only has `request_queue_size` (`prefs/model.rs:582`) |
| #24684 | `allow_multiple_connections_per_pid` | ⬜ | Low: pref + engine check on duplicate peer IDs; only `enable_multi_connections_from_same_ip` exists (`prefs/api_json.rs:404`) |
| #24891 | I2P inbound/outbound length variance | ⬜ | Small: add `i2p_inbound_length_variance`/`i2p_outbound_length_variance` to prefs and the SAM options (`engine/i2p/src/lib.rs:207` sends `inbound.length`, no variance) |
| #24870 | I2P PeX setting | ➖ | No I2P PeX in the engine (`docs/PARITY.md`, I2P row). Optionally accept the key as inert for client compatibility |
| #24837 | WebTorrent STUN server setting | ➖ | No WebTorrent in the engine. Optionally accept the key as inert |
| #24641 | Revamp .torrent backup management | ⬜ | Replace `export_dir`/`export_dir_fin` (`prefs/api_json.rs:220`) with `torrent_files_backup_enabled`, `torrent_files_backup_dir`, `torrent_files_finished_backup_dir_enabled`, `torrent_files_finished_backup_dir`, `remove_torrent_file_backup`; migrate old keys; the export code is `export_to` in `bittorrent/adding.rs`; Swift Downloads prefs |
| #24724 | `seedMode` replaces `skip_checking` | ⬜ | Rename in `torrents/add` (`webui/api/torrents.rs:772`), accept `seedMode`, drop `skip_checking`; the core field (`bittorrent/adding.rs:284`) can keep its name; update Swift label |
| #24336 | Magnet `so` (select only) parameter | ⬜ | `parse_magnet` ignores `so` (`bittorrent/metainfo.rs:488`); map indexes/ranges to file priorities (rest "do not download") on add; Add Torrent dialog preselection |
| #24346 | Torrent Creator "Ignore dotfiles" (default on) | ⬜ | Creator always skips hidden names and the macOS hidden flag (`creator/layout.rs:96,132`); add the option (`ignoreDotfiles` in `torrentcreator/addTask` and `status`); when on, filter by name only; when off, include hidden files |
| #24173 | Dotfiles filtered on all platforms | ✅ | `creator/layout.rs:65,96,132` |
| #24159 | "Force reannounce" really reannounces | ✅ | `engine/tracker_comms/src/tiers.rs:471-486` clears the timers, DHT too (`engine/librqbit/src/torrent_state/trackers.rs:304`) |
| #24605 | No failed add result after a tracker merge | ✅ | Duplicates return a conflict plus a `DuplicateTorrent` event (`bittorrent/adding.rs:394-453`); RSS treats it as already added (`rss/tests.rs:166`) |
| #24866 | Completion with ignored files | ⬜ | Verify: `is_finished` is `rt.finished` (`bittorrent/torrent.rs:455`); check the engine marks a torrent finished when only "do not download" files remain |
| #24670 | Update share limits timer when limits change | ✅ | Limits are evaluated by the session tick (`bittorrent/policy.rs:865`); no stale timer to refresh |
| #25040 | Free space of the parent path when the path is missing | ✅ | `utils/fs.rs:16-20` walks up to an existing parent |
| #24304 | Several recipients for e-mail notifications | ✅ | `net/smtp.rs:90` splits on `,`/`;`, test at `:244` |
| #24000 | Session speed = sum of per-torrent payload rates | ⬜ | Verify how the global rate is computed (`bittorrent/speed.rs`, `transfer/info`); sum per-torrent smoothed rates if it comes from raw byte deltas |
| #23817 | Do not overwrite config without write permission | ⬜ | Low, verify: profile files go through `write_atomic` (`utils/fs.rs:35`); check a read-only profile does not lose data or loop on errors |
| #24671 | Relative logs folder | ⬜ | Low, verify: resolve a relative file-log path against the profile (`logger.rs`) |
| #24505 | "Resolve peer countries" off by default | ⬜ | Trivial: default is `true` (`prefs/model.rs:607`); change for new profiles only |
| #24711 | Disk queue default 100 MiB | ⬜ | Trivial: default is 1 MiB (`prefs/model.rs:625`); only matters if the engine uses it, else ➖ |
| #24084 | "Queued tracker announces" session metric | ⬜ | Count announces waiting on `max_concurrent_http_announces` (`prefs/model.rs:573`); needs a counter in `engine/tracker_comms`; show in Statistics and `server_state` |
| #24152 | "Request latency" metric | ➖ | libtorrent session counter; the engine does not measure it. Optionally emit 0 in `server_state` for client compatibility |
| #24675 | Fix IP address comparison | ➖ | C++ `lt::address` bug; Rust `IpAddr` ordering is correct |
| #25047 | Avoid heavy work in a callback | ➖ | Qt/libtorrent callback internals |

## macOS app UI

| PR | Change | Status | Notes |
|---|---|---|---|
| #23829 | Filter the torrent list by info hash | ⬜ | Search scopes are name, save path, tracker (`apps/macos/QBTNeo/MainWindow/TransferList/TransferListModel.swift:215-221`); match the hash (v1/v2, prefix) in the name scope |
| #23973 | "Copy path" in the Content tab | ⬜ | `Properties/Content/ContentContextMenu.swift` has Open, Open Containing Folder, Rename only; add Copy Path |
| #24090 | "Open Destination Folder" toolbar button | ⬜ | Exists in the context menu (`TransferContextMenu.swift:73`), missing from `MainToolbar.swift`; add an optional toolbar item |
| #24260 | "Force start" in filter context menus | ⬜ | `MainWindow/Sidebar/SidebarMenu.swift:35-37` has Start/Stop/Remove only; add Force Start Torrents |
| #23989 | "Contribution" column in the peers list | ⬜ | Core computes `contribution` (also for `sync/torrentPeers`), new column in `Properties/Peers/PeerTable.swift` |
| #24023 | Clickable "speed graphs disabled" warning | ⬜ | Trivial: button in `Properties/Speed/SpeedTab.swift:29` opening the matching preference |
| #24117 | Better "Deletion confirmation" dialog | ⬜ | Trivial: confirm button says "Remove torrent" / "Remove torrent and content" and the text states the count; check the sheet behind `AppModel+Actions.swift:66` |
| #24084, #24152 | Statistics rows for the new metrics | ⬜ | After the core metrics: `Features/Statistics/StatisticsWindow.swift:23` (queued announces; latency only if measured) |
| #24776 | Option: close search tabs with middle click | ⬜ | Low: pref + gesture in `Features/Search/SearchTabChip.swift` |
| #24740 | Detect redundant quotes in the Python path | ⬜ | Small: strip surrounding quotes from the configured path and warn (`search/python.rs`, Search prefs) |
| #24279 | Python minimum becomes 3.13 | ⬜ | Trivial decision: `PythonVersion::MINIMUM` is 3.9 (`search/python.rs:31`); follow upstream or keep 3.9 on purpose |
| #23825 | "North Macedonia" instead of FYR Macedonia | ⬜ | Trivial: `net/geoip/countries.rs:148` |
| #25037 | Re-sort when Remaining changes | ⬜ | Verify: `TransferSort.swift:51` compares `amountLeft`; make sure the table re-sorts as the value changes |
| #23949 | E-mail examples as placeholder text | ⬜ | Trivial: prompts in the Downloads prefs e-mail fields |
| #24061 | Tooltips on e-mail option fields | ⬜ | Trivial: `.help()` on the same fields |
| #23908 | macOS delete shortcuts | ➖ | Qt keyboard fix; AppKit/SwiftUI use native Delete handling |
| #23905, #23909, #23916 | Toolbar separators, dark line-edit corners, comment background (Qt styling on macOS) | ➖ | Qt-only styling |
| #24775, #24083, #24127 | Status bar / tray icon follows appearance or colour scheme | ➖ | `MenuBarExtra` (`apps/macos/QBTNeo/QBTNeoApp.swift:50`) is drawn by the system and adapts to appearance |
| #24122 | Scroll bars in the Statistics dialog | ➖ | SwiftUI window resizes and scrolls natively |
| #24150 | Group for status-bar settings | ➖ | Qt Options layout; NeoRS pages are organised its own way |
| #24644 | Scrollable Torrent Options dialog | ➖ | Qt dialog size issue |
| #24735 | "Resize columns" in the Content Layout header | ➖ | Qt header action; `NSOutlineView` has its own column menu |
| #23917 | Clickable RSS settings warning | ➖ | Qt-only warning label |
| #24403 | "Already present" dialog position | ➖ | Qt window placement |
| #25012 | Theme tooltip colours | ➖ | Qt palette |

## WebAPI

| PR | Change | Status | Notes |
|---|---|---|---|
| #24918 (2.16.2) | Category share limits in `createCategory`/`editCategory` (`ratioLimit`, `seedingTimeLimit`, `inactiveSeedingTimeLimit`, `shareLimitsMode`, `shareLimitAction`); `editCategory` no longer needs `savePath` | 🟡 | Done except `shareLimitsMode` (comes with #24043): limits parsed and validated in `apply_share_limit_params`; `torrents/categories` now reports the stored limits instead of -2 |
| #24798 | `editCategory` must not overwrite unrelated options | ✅ | Edits start from the stored category; `savePath` optional; empty `downloadPathEnabled` = default (`webui/api/torrents.rs` `edit_category`, e2e `edit_category_keeps_what_the_call_leaves_out`) |
| #24920 | New prefs `confirm_remove_all_tags`, `confirm_remove_tracker_from_all_torrents` | ⬜ | Add to prefs and `prefs/api_json.rs`; the Swift app already confirms, wire it to the keys |
| #24891, #24870 | `i2p_*_length_variance`, `i2p_pex_enabled` keys | ⬜ | Variance: see Core/engine; the PeX key may be inert |
| #24837 | `webtorrent_stun_server` key | ⬜ | Inert key for clients (see Core/engine) |
| #24842 | `rss/exportRules`, `rss/importRules` (POST) | ⬜ | Core already exports rules (`rssExportRules`, `apps/macos/Generated/qbt_core.swift:804`; UI `Features/RSS/Downloader/RulesExport.swift`); add the two endpoints in `webui/api/rss.rs` |
| #24269 | `transfer/pauseSession`, `transfer/resumeSession`, `session_state` in `server_state` | ⬜ | Needs a core "session paused" state (stop all engine activity without changing torrent states) |
| #24249 | Prefs `start_paused`, `shutdown_timeout` | ⬜ | `shutdown_timeout` exists (`prefs/model.rs:523`); add `start_paused` |
| #24791 | POST-only endpoints | ✅ | See Security |
| #24724 | `seedMode` replaces `skip_checking` | ⬜ | See Core/engine |
| #24720 | `web_ui_sessions_count_limit` | ⬜ | See Security |
| #24641 | New backup prefs, `export_dir*` removed | ⬜ | See Core/engine |
| #24684 | `enable_multi_connections_from_same_peer_id` | ⬜ | See Core/engine |
| #23989 | `contribution` in `sync/torrentPeers` | ⬜ | See macOS app UI |
| #24346 | `ignoreDotfiles` in `torrentcreator/addTask` and `status` | ⬜ | See Core/engine |
| #24246 | File `priority` in `torrents/fetchMetadata`/`parseMetadata` when excluded names apply | ⬜ | Check the handlers in `webui/api/torrents.rs` return priority 0 for excluded names (the excluded-names list is in the core) |
| #24210 | `torrentcreator/status` times as Unix timestamps | ⬜ | `webui/api/creator.rs:54` emits a Qt date string via `qt_date`; emit epoch seconds for `timeAdded`, `timeStarted`, `timeFinished`; update `webui/src` and any Swift reader |
| #24158 | `seeding_outgoing_connections` key | ⬜ | See Core/engine |
| #24152 | `request_latency` in `sync/maindata` | ⬜ | Emit (0 if not measured) in `server_state` |
| #24084 | `queued_tracker_announces` in `sync/maindata` | ⬜ | See Core/engine |
| #23838 | `mail_notification_encryption_type` | ⬜ | See Security |
| #24134 | `transfer/getSpeedLimits`, `transfer/setSpeedLimits` | ⬜ | Not in the route table (`webui/api/mod.rs:182` has only the mode toggles and the single limits); read/write `up_limit`, `dl_limit`, `alt_up_limit`, `alt_dl_limit` |
| #24253 | `max_outstanding_block_requests` | ⬜ | See Core/engine |
| #24135 | `torrents/downloadFile` (`hash`, `file` as index or relative path) | ⬜ | Serve a completed file from torrent content. Security: resolve against the torrent's content root only, reject `..` and symlink escape, require the file complete, stream with `Content-Disposition` |
| #24056 | `rss/cloneRule` (`sourceName`, `cloneName`) | ⬜ | Not in `webui/api/mod.rs`; see RSS |
| #24043 | `share_limits_mode` in maindata, prefs and `setShareLimits` | ⬜ | See Core/engine |
| #23856 (2.15.2) | `app/getFreeSpaceAtPath` | ✅ | `webui/api/app.rs:92`, route `webui/api/mod.rs:173` (the changelog line's `Action` suffix is the C++ method name; the real route is `getFreeSpaceAtPath`) |
| #23784 (2.16.1) | `search/status` has `pattern`, `category`, `plugins`; prefs `store_search_jobs`, `store_search_job_results` | ⬜ | Add the fields in `webui/api/search.rs` and two prefs; Swift already restores tabs (`Features/Search/SearchSessionStore.swift`) |
| #24341 | Use torrent names for exported .torrent files | ⬜ | `webui/api/torrents.rs:560,608` names the download after the hash (`safe_file_stem`); use `to_valid_file_name(name)` (`bittorrent/paths.rs:212`) |
| #24875 | Correct sync data when the client missed the last response | ✅ | `webui/sync.rs:257-265` keeps the change buffer until the client acknowledges the last `rid`; the peers sync (`:443`, `generate_sync_data`) has its own logic, verify it the same way |
| #24700 | Construct controllers on demand | ➖ | Internal C++ optimisation; NeoRS routes by table |

## Bundled Web UI

NeoRS-specific implementation (see intro). "Port" means: write the feature in
`webui/src/*.ts` with the upstream commit as reference, then run
`scripts/build-webui.sh`.

| PR | Change | Status | Notes |
|---|---|---|---|
| #23887 | Virtual list by default | ✅ | `webui/src/vlist.ts`, used by transfers, files, RSS, search |
| #24838 | Hide decorative elements from screen readers | ✅ | `webui/src/icons.ts:70` sets `aria-hidden` on icons |
| #24792, #24970, #24889, #24458 | Security items | ✅ | See Security |
| #23975 | "Select all" and "Invert selection" | ⬜ | Port: there is a select mode but no select-all or invert (`transfers.ts:219`) |
| #23856 | Free disk space in the add-torrent window | ⬜ | Port in `add.ts`; the endpoint exists |
| #24109 | "Pread/pwrite" disk IO type option | ⬜ | The core enum has it (`prefs/enums.rs:64`); `settings.ts` has no `disk_io_type` row |
| #24134 | Unified global speed limit window | ⬜ | `speedLimits()` exists (`main.ts:33`); switch it to the new endpoints when added |
| #24135 | Download completed files | ⬜ | Port after `torrents/downloadFile` exists (`details.ts` file menu) |
| #24246 | Respect excluded file names in Add torrent | ⬜ | Port after the WebAPI priority field |
| #24283 | Test e-mail notice instead of alerts | ⬜ | Endpoint exists (`webui/api/app.rs:49`); `settings.ts` has no e-mail test button, add one with an inline notice |
| #24404 | Skip the "Add new torrent" dialog | ⬜ | Port in `add.ts` with the matching pref |
| #24427 | Reuse category and plugin when refreshing a search | ⬜ | Port in `search.ts` (no refresh action yet); relies on the `search/status` fields |
| #24376 | Secondary sort column | ⬜ | `transfers.ts` sorts by one key |
| #24678 | "Copy path" in the content menu | ⬜ | Port in `details.ts` |
| #24692 | Non-torrent RSS link handling | ⬜ | Port in `rss.ts`; mirror `RSSLinkPolicy.swift` (only http(s), magnet and hashes are downloadable) |
| #24720 | Sessions limit setting | ⬜ | Settings row once the pref exists |
| #23784 | Keep search tabs across restarts and sessions | ⬜ | Port in `search.ts` after the WebAPI/pref change |
| #24249, #24269 | Start paused, shutdown timeout, pause/resume session | ⬜ | Port after the core state exists |
| #24842 | Export/import RSS rules | ⬜ | Port in `rss.ts` after the endpoints exist |
| #24848 | Cap path autocompletion at 25 | ⬜ | Cap server side in `app/getDirectoryContent` (`webui/api/app.rs`), not only in the client |
| #24920 | Missing Options page settings | ⬜ | `settings.ts` is a subset of the prefs; add the new keys together with the ones above |
| #24984 | Shared download window for RSS articles | ⬜ | Low; `rss.ts` downloads one article at a time |
| #24379 | Cross-origin links to webroot | ⬜ | Server side, see Security |
| #24210 | Creator dates as epoch seconds | ⬜ | See WebAPI |
| #23878, #23837, #23818, #24708, #24710, #24804, #24816, #24879, #24686 | Folder collapse, in-place array filtering, set-rows speed, disabled instead of hidden menu items, header click errors, category indentation, version table layout, sortable properties panel, RSS refresh back-off | ➖ | Behaviour of upstream's MooTools/Mocha code; the NeoRS UI is different code. Spot-check folder collapse and disabled-vs-hidden menu items when touching `details.ts` |

## RSS

| PR | Change | Status | Notes |
|---|---|---|---|
| #24056 | "Clone rule" in the auto-downloader | ⬜ | Swift action in `Features/RSS/Downloader/RSSRuleListPane.swift` and a core `clone_rule`; WebAPI `rss/cloneRule` |
| #23963 | Sticky "All" RSS item | ⬜ | `RSSFeedList.swift` has only the Unread node (`.unread`); add "All" showing every article |
| #24120 | Add torrents from links in the article viewer | ⬜ | Verify `RSSArticleView.swift` / `RSSDescriptionRenderer.swift` make description links downloadable via `RSSLinkPolicy.isSupportedTorrentURL` |
| #24519 | Persist feed list sort order | ⬜ | Low: `RSSFeedList` has no sort order to persist; add sorting and remember it |
| #24806 | Clamp max articles per feed to non-negative | ✅ | `rss/mod.rs:179` clamps to `0..=i32::MAX` |
| #24082 | Added rule follows the "enabled" state | ➖ | Qt widget bug; check once while building clone rule |
| #24842 | RSS rules export/import over WebAPI | ⬜ | See WebAPI |

## Search

| PR | Change | Status | Notes |
|---|---|---|---|
| #24912 | Keep cookies across HTTP redirects | ⬜ | Port `git show 9b0e9311f` into `crates/qbt-core/resources/nova3/helpers.py` |
| #24933 | Escape pipe characters properly | ⬜ | Port `git show 4ab1a06d1` into `resources/nova3/novaprinter.py` and the result parser in `search/nova.rs` if it splits on `|` |
| #25017 | Relaxed plugin versioning | ⬜ | `PluginVersion` accepts exactly two components (`search/nova.rs:29-52`) and a short header line; accept 1 to 3 components and a line up to 23 chars (`# VERSION: 999.999.999`) |
| #24548 | Revised plugin install procedure (unlisted: more checks, new log format) | ⬜ | Compare `search/plugins.rs` with `git show d3e8abede`: unknown file format, same or newer already installed, post-install verification, new log lines |
| #24776 | Middle-click closes search tabs | ⬜ | See macOS app UI |
| #23911 | Search docs format | ➖ | Docs only |

## Plugins (Lua)

| PR | Change | Status | Notes |
|---|---|---|---|
| #24485 (unlisted) | Lua plugins engine: plugins dialog, event handlers (`onTorrentAdded`, `onTorrentMetadataReady`, `onTorrentFinished`, `onTorrentStarted`, `onTorrentStopped`, `onTorrentAboutToBeRemoved`), torrent object, `qBittorrent.exec/log/debug/...` API, version rule | ⬜ | Large, optional, do last. Needs an embedded Lua (a Rust Lua binding or the C sources), a sandboxed API mapped to NeoRS events (`events.rs`), a plugins folder in the profile, a Plugins window in the app, WebAPI if wanted. Examples: `examples/plugins/*.lua` at tag `release-5.3.0` |

Size and security notes: upstream added about 2,800 lines (engine, class
bindings, stack helpers, dialog). The risk is the largest of this whole list:
`qBittorrent.exec` runs arbitrary commands, so a plugin is full code
execution as the user. If built, follow the existing hardening rules
(`docs/SECURITY.md`): load plugins only from the profile folder and only if it
is owner-writable; no network or file API beyond narrow calls; a hard
instruction and time budget per callback and a memory cap on each Lua state;
callbacks must stay off the core's hot paths; no auto-install from a URL; show
every plugin and what it can do in the UI; a global off switch, off by default.
The app's sandbox and entitlements also need review before `exec` is allowed.

## Not applicable

Entries not already listed in the tables above.

| PR | Change | Status | Notes |
|---|---|---|---|
| #23953 | Build against the latest libtorrent | ➖ | NeoRS uses its own engine |
| #24007 | Stop using deprecated libtorrent features | ➖ | Same |
| #23952 | Remove outdated setting keys | ➖ | Upstream-internal keys; NeoRS migration reads old profiles in `migrate/` |
| #24334 | Windows ARM build | ➖ | Windows |
| #24673 | NSIS Thai translation | ➖ | Windows installer |
| #24707 | Open Default Apps (Windows) | ➖ | Windows |
| #24280 | Python on ARM64 Windows | ➖ | Windows |
| #24456 | Translation-change memory leak | ➖ | Qt |
| #24611 | FOSSHub update source | ➖ | NeoRS has no such source |
| #24771 | Man pages for all CLI options | ➖ | Upstream packaging; `qbit-neors-nox` documents its own flags |
| #24905 | CET shadow stack flag | ➖ | Windows linker flag |
| #25019 | Documentation link fix | ➖ | Upstream docs |

## Suggested order

Small and valuable first.

1. Security and correctness fixes (each under a day):
   * #24798 `editCategory` overwrites share limits (an existing bug).
   * #24720 sessions count limit.
   * #24379 webroot exempt from the cross-site check.
   * #24512 filter unsupported tracker URLs from magnets.
   * #24341 exported .torrent file names.
   * #24848 cap path autocompletion at 25 (server side).
2. Quick compatibility wins: #24724 `seedMode`, #24210 epoch dates in the
   creator status, #24134 `getSpeedLimits`/`setSpeedLimits`, #24920 new
   confirm prefs, inert keys (#24837, #24870, #24152 `request_latency`).
   Bump `WEBAPI_VERSION` to 2.16.2 once the WebAPI set is complete.
3. Trivial defaults and polish: #24505, #24711, #23825, #24279, #24740,
   #24023, #24117, #23949/#24061, check #25037.
4. Search: #24912, #24933, #25017, #24548 (nova3 and plugin manager ports).
5. Small features: #23829 info hash filter, #23973 Copy path, #24090 toolbar
   button, #24260 Force start in sidebar menus, #23989 Contribution column,
   #24346 Ignore dotfiles, #24336 magnet `so`, #24891 I2P variance.
6. E-mail and RSS: #23838 encryption type, #24056 clone rule (+ `rss/cloneRule`),
   #23963 "All" item, #24842 RSS rules export/import endpoints, check #24120.
7. Share limits: #24043 match any/all, then #24918 category share limits in
   the WebAPI.
8. Backups and sessions: #24641 .torrent backup revamp, #24269/#24249 pause
   and resume session, #23157 ban IP ranges, #24135 `torrents/downloadFile`
   (needs the path-escape review above), #23784 search status fields and
   stored jobs, #23386 per-component name validation.
9. Bigger UI and engine work: #24266 Manage content dialog, #24084 queued
   announces metric, #24158 / #24253 / #24684 engine settings (need engine
   work, low value), verify #24866, #24000, #23817, #24671.
10. Bundled Web UI ports for everything above, then the stragglers
    (#23975 select all/invert, #24376 secondary sort, #24984, #24404, #24427).
11. Lua plugins (#24485): last, optional, with a separate design and security
    review (see the size and security note in the Plugins section).
