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
| #24720 | WebUI sessions count limit | ✅ | Pref `web_ui_sessions_count_limit` (default 15, 0 = unlimited; `MAX_SESSIONS` still caps) drops the least recently used login; API-key sessions do not count (`webui/session.rs`); Settings field + WebUI settings page |
| #24379 | Allow cross-origin links to the webroot | ✅ | GET/HEAD of `/` and `/index.html` skip the CSRF check (`webui/webapp.rs`); e2e `csrf_and_host_validation` |
| #23838 | Fix STARTTLS; replace the SSL checkbox by an encryption type (None / STARTTLS / SMTPS) | ✅ | `SmtpEncryptionType` (default SMTPS) → lettre `Tls::Wrapper` / `Required` / `None`, default ports 465 / 587 / 25; stored `mail_notification_ssl_enabled` and qBittorrent's `req_ssl` (ini or 5.2 WebAPI import) migrate as 5.3 does; Swift picker with 5.3's tooltip. Test: required STARTTLS sends no AUTH or MAIL to a server without it |
| #24512 | Drop unsupported tracker URLs from magnets | ✅ | `parse_magnet` keeps only http/https/udp trackers with a host (`is_supported_tracker_url`, `bittorrent/metainfo.rs`); test `magnets_keep_only_trackers_they_can_announce_to` |
| #24087 | Do not load a zero-size GeoIP database | ✅ | `net/geoip.rs:95-110` turns a failed open into a warning; add a test with an empty file to be sure |

## Core/engine

| PR | Change | Status | Notes |
|---|---|---|---|
| #24043 | Option to match all share limits (`MatchAny`/`MatchAll`) | ✅ | `ShareLimitsMode` per torrent, category (inherits from the parent) and global (`share_limits_mode`, default MatchAny); `share_limits::evaluate` (MatchAll: every enabled limit, "Torrent reached the share limit(s).") and the seeding ETA (min / max of the per-limit ETAs); resume `share_limits_mode`, RSS `share_limits_mode`, imports from qBittorrent's ini and fastresume. Swift: "Mode:" in the share limits editor, "Any of the above" / "All the above" in Seeding Limits |
| #24266 | "Manage content" (Torrent Content Layout) with batch editing of file names | ⬜ | New Swift dialog from the transfer-list menu: edit many file/folder names at once. Core rename exists (`bittorrent/engine.rs:848`) |
| #23386 | Enhance file name validation | ✅ | The core already checked every component (`paths::is_valid_relative_path`, refusing `.`/`..`); the WebAPI leaf check now uses the same rules (`:` refused on macOS, 255 bytes). Session test covers `..`, `.`, `:` and control characters in middle components |
| #23157 | Allow banning an IP range | ✅ | `ipfilter::parse_ban_entry` (address, CIDR, `a - b`), stored as `a`/`a - b`; ranges block peers through the peer IP filter (single addresses stay in the engine's ban list), trackers with "Apply to trackers"; rejected entries logged with 5.3's message; Swift `IPAddressParser.normalizedBanEntry` with unit tests |
| #24195 | IPv6 `/128` is a single IP | ✅ | `webui/subnet.rs:74` (mask math handles 128); `IPAddressParser.swift:29` |
| #24680 | P2P filter parsing for IPv6 | ✅ | `ipfilter.rs:288-300` falls back to earlier colons |
| #24734 | Log IP ban alerts | ⬜ | Small: log a line when the engine bans a peer on its own; check the engine reports automatic bans (only the manual path, `bittorrent/engine.rs:556`, was found) |
| #24158 | `seeding_outgoing_connections` setting | ⬜ | Pref (default true); needs engine support to stop dialling peers while seeding (`engine/librqbit`), otherwise inert |
| #24253 | `max_outstanding_block_requests` (libtorrent `max_allowed_in_request_queue`) | ⬜ | Low: pref + cap on queued incoming requests per peer in the engine. NeoRS only has `request_queue_size` (`prefs/model.rs:582`) |
| #24684 | `allow_multiple_connections_per_pid` | ⬜ | Low: pref + engine check on duplicate peer IDs; only `enable_multi_connections_from_same_ip` exists (`prefs/api_json.rs:404`) |
| #24891 | I2P inbound/outbound length variance | ✅ | `SamOptions::{inbound,outbound}_length_variance` (-7..7), sent as `inbound.lengthVariance` / `outbound.lengthVariance` in SESSION CREATE like libtorrent |
| #24870 | I2P PeX setting | ➖ | No I2P PeX in the engine (`docs/PARITY.md`, I2P row); `i2p_pex_enabled` is stored and returned for clients |
| #24837 | WebTorrent STUN server setting | ➖ | No WebTorrent in the engine; `webtorrent_stun_server` is stored and returned for clients |
| #24641 | Revamp .torrent backup management | ✅ | `bittorrent/torrent_backup.rs`: backup on add (magnets when the metadata arrives, v2 torrents when they finish), move to the finished folder, removal with the torrent; the registry is the resume record's `torrent_file_backup`. Relative folders are inside the profile's `data/` (default `backup`). Old `export_dir`/`export_dir_fin` are migrated in `preferences.json` and when importing qBittorrent settings (old and new ini keys). Deviation: the backup is written from the stored metadata (as "Export .torrent") instead of copying the source file byte for byte; qBittorrent's `BackupTorrentFiles` registry is not imported. Mac app: Transfers › Adding › .torrent Files; Web UI settings |
| #24724 | `seedMode` replaces `skip_checking` | ✅ | `torrents/add` takes `seedMode` and still `skip_checking` for older clients; saved add parameters (RSS rules) write `seed_mode` and read either name. The core field keeps its name. The Mac app keeps the label "Skip hash check": it says what the option does and is translated, while 5.3 has no translations for "Seed mode" yet |
| #24336 | Magnet `so` (select only) parameter | ✅ | `MagnetLink::select_only` (libtorrent parsing and 10000 bound), kept in the resume record until metadata arrives, then unlisted files get "Do not download" (`apply_select_only`, by engine index); cached dialog metadata keeps it; `MetadataFile.priority` preselects the Add Torrent dialog |
| #24346 | Torrent Creator "Ignore dotfiles" (default on) | ✅ | `ignore_dotfiles` in the creator params, `collect_files` and piece counting; dot folders are skipped with their whole subtree (5.3 descends into them); hidden-flag files are included like 5.3; checkbox in the creator window (`TorrentCreator/IgnoreDotfiles`) |
| #24173 | Dotfiles filtered on all platforms | ✅ | `creator/layout.rs:65,96,132` |
| #24159 | "Force reannounce" really reannounces | ✅ | `engine/tracker_comms/src/tiers.rs:471-486` clears the timers, DHT too (`engine/librqbit/src/torrent_state/trackers.rs:304`) |
| #24605 | No failed add result after a tracker merge | ✅ | Duplicates return a conflict plus a `DuplicateTorrent` event (`bittorrent/adding.rs:394-453`); RSS treats it as already added (`rss/tests.rs:166`) |
| #24866 | Completion with ignored files | ✅ | Verified, no change: `rt.finished` is set when every file not set to "Do not download" is complete (`TorrentEntry::wanted_complete`, unit-tested in torrent.rs), independent of the engine's own finished flag |
| #24670 | Update share limits timer when limits change | ✅ | Limits are evaluated by the session tick (`bittorrent/policy.rs:865`); no stale timer to refresh |
| #25040 | Free space of the parent path when the path is missing | ✅ | `utils/fs.rs:16-20` walks up to an existing parent |
| #24304 | Several recipients for e-mail notifications | ✅ | `net/smtp.rs` sends one e-mail per `;` group, to the group's `,`-separated addresses (as `SMTPClient::sendMail`); tests `parses_settings`, `sends_through_local_server` |
| #24000 | Session speed = sum of per-torrent payload rates | ✅ | Came from the engine's session-wide estimator; now the sum of the torrents' rates each tick (`policy.rs` tick), also for the speed graph's payload series |
| #23817 | Do not overwrite config without write permission | ✅ | `preferences.json` is skipped when it exists and is not writable (`access(W_OK)`), as `SettingsStorage`; the atomic rename used to replace it regardless. Unit test `read_only_preferences_are_not_overwritten` |
| #24671 | Relative logs folder | ✅ | Was resolved against the process' working directory; now inside the profile's data folder (`logger::log_dir`, unit test) |
| #24505 | "Resolve peer countries" off by default | ✅ | Off for new profiles (saved settings keep their value); demo mode keeps flags on |
| #24711 | Disk queue default 100 MiB | ✅ | Default changed; the engine does not use the value, it is reported for clients |
| #24084 | "Queued tracker announces" session metric | ⬜ | Count announces waiting on `max_concurrent_http_announces` (`prefs/model.rs:573`); needs a counter in `engine/tracker_comms`; show in Statistics and `server_state` |
| #24152 | "Request latency" metric | ➖ | The engine does not measure it; `server_state.request_latency` is always 0 |
| #24675 | Fix IP address comparison | ➖ | C++ `lt::address` bug; Rust `IpAddr` ordering is correct |
| #25047 | Avoid heavy work in a callback | ➖ | Qt/libtorrent callback internals |

## macOS app UI

| PR | Change | Status | Notes |
|---|---|---|---|
| #23829 | Filter the torrent list by info hash | ✅ | Info Hash v1 / v2 scopes in the filter field menu (`TransferSearchScope`), matched like the other scopes |
| #23973 | "Copy path" in the Content tab | ✅ | Copy Path in `ContentContextMenu` (single item), the full on-disk path |
| #24090 | "Open Destination Folder" toolbar button | ✅ | Optional "openDestination" toolbar item (Customize Toolbar); hidden by default, unlike upstream |
| #24260 | "Force start" in filter context menus | ✅ | Force Start Torrents in every sidebar row menu, on the visible torrents |
| #23989 | "Contribution" column in the peers list | ✅ | `PeerInfo.contribution` (`peers::peer_contribution`, same formula), Peers tab column after Files; also in `sync/torrentPeers` |
| #24023 | Clickable "speed graphs disabled" warning | ✅ | The Speed tab's hint is a link that opens Settings on "Show speed graphs" (`SpeedTab.swift`) |
| #24117 | Better "Deletion confirmation" dialog | ✅ | The confirm button says "Remove torrent" or "Remove torrent and content" as the checkbox changes (`RemoveTorrentsSheet.swift`) |
| #24084, #24152 | Statistics rows for the new metrics | ⬜ | After the core metrics: `Features/Statistics/StatisticsWindow.swift:23` (queued announces; latency only if measured) |
| #24776 | Option: close search tabs with middle click | ⬜ | Low: pref + gesture in `Features/Search/SearchTabChip.swift` |
| #24740 | Detect redundant quotes in the Python path | ✅ | Settings drops a matching pair of surrounding quotes as you type; the core uses a quoted path without them and logs a warning (5.3 refuses it in the Web UI form) |
| #24279 | Python minimum becomes 3.13 | ✅ | The app bundles a trimmed Python 3.13 (`scripts/embed-python.sh`); `PythonVersion::MINIMUM` is 3.13 with the 5.3 nova3 scripts |
| #23825 | "North Macedonia" instead of FYR Macedonia | ✅ | `net/geoip/countries.rs` |
| #25037 | Re-sort when Remaining changes | ✅ | Nothing to do: every sync with changed torrents re-sorts the list (`TransferListModel.storeDidChange` → `refilter`), Remaining included |
| #23949 | E-mail examples as placeholder text | ✅ | Prompts `qBittorrent_notification@example.com` and `smtp.example.com:465` (`NotificationsGroup.swift`) |
| #24061 | Tooltips on e-mail option fields | ✅ | `.help()` on the sender, recipient and SMTP server fields with qBittorrent's texts (Russian added; upstream has no translations yet). The encryption picker's tooltip comes with the SMTP encryption setting |
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
| #24918 (2.16.2) | Category share limits in `createCategory`/`editCategory` (`ratioLimit`, `seedingTimeLimit`, `inactiveSeedingTimeLimit`, `shareLimitsMode`, `shareLimitAction`); `editCategory` no longer needs `savePath` | ✅ | Limits parsed and validated in `apply_share_limit_params`, `shareLimitsMode` included (an unknown name is a 400, never a silent Default); `torrents/categories` reports the stored limits and `share_limits_mode` |
| #24798 | `editCategory` must not overwrite unrelated options | ✅ | Edits start from the stored category; `savePath` optional; empty `downloadPathEnabled` = default (`webui/api/torrents.rs` `edit_category`, e2e `edit_category_keeps_what_the_call_leaves_out`) |
| #24920 | New prefs `confirm_remove_all_tags`, `confirm_remove_tracker_from_all_torrents` | ✅ | Core preferences (`advanced`), imported from qBittorrent's ini; the Mac app moved its own stored values into them once and reads them from the core; the Web UI honours `confirm_remove_all_tags` and `confirm_torrent_recheck` |
| #24891, #24870 | `i2p_*_length_variance`, `i2p_pex_enabled` keys | ✅ | Stored, clamped to -7..7, imported from qBittorrent; passing the variance to SAM is the #24891 engine row |
| #24837 | `webtorrent_stun_server` key | ✅ | Stored only |
| #24842 | `rss/exportRules`, `rss/importRules` (POST) | ✅ | `webui/api/rss.rs`; export GET/POST as an attachment, import exactly one JSON file (400 otherwise); e2e test `rss_rules_export_and_import` |
| #24269 | `transfer/pauseSession`, `transfer/resumeSession`, `session_state` in `server_state` | ✅ | POST endpoints on the existing core session pause (`Session::pause_session`/`resume_session`: torrents keep their states, the engine holds them; not persisted); `session_state` (bool) in `sync/maindata`'s `server_state` only, as upstream; e2e `pause_and_resume_session` |
| #24249 | Prefs `start_paused`, `shutdown_timeout` | ✅ | Both already existed as GUI-only settings (`advanced.start_paused`, `advanced.shutdown_timeout`); now WebAPI keys after `python_executable_path` (SET ignores a timeout below -1 or not an int) and imported through the ini map. The Mac app's settings catalog uses the WebAPI keys |
| #24791 | POST-only endpoints | ✅ | See Security |
| #24724 | `seedMode` replaces `skip_checking` | ✅ | See Core/engine |
| #24720 | `web_ui_sessions_count_limit` | ✅ | Pref `web_ui_sessions_count_limit` (default 15, 0 = unlimited; `MAX_SESSIONS` still caps) drops the least recently used login; API-key sessions do not count (`webui/session.rs`); Settings field + WebUI settings page |
| #24641 | New backup prefs, `export_dir*` removed | ✅ | See Core/engine; e2e `torrent_file_backups` |
| #24684 | `enable_multi_connections_from_same_peer_id` | ⬜ | See Core/engine |
| #23989 | `contribution` in `sync/torrentPeers` | ✅ | Serialized from `PeerInfo.contribution` |
| #24346 | `ignoreDotfiles` in `torrentcreator/addTask` and `status` | ✅ | Default true; reported by `status` |
| #24246 | File `priority` in `torrents/fetchMetadata`/`parseMetadata` when excluded names apply | ⬜ | Check the handlers in `webui/api/torrents.rs` return priority 0 for excluded names (the excluded-names list is in the core) |
| #24210 | `torrentcreator/status` times as Unix timestamps | ✅ | `timeAdded`, `timeStarted`, `timeFinished` are epoch seconds; the NeoRS Web UI and the Mac app do not read them |
| #24158 | `seeding_outgoing_connections` key | ⬜ | See Core/engine |
| #24152 | `request_latency` in `sync/maindata` | ✅ | Always 0 |
| #24084 | `queued_tracker_announces` in `sync/maindata` | ⬜ | See Core/engine |
| #23838 | `mail_notification_encryption_type` | ✅ | Replaces `mail_notification_ssl_enabled` in `app/preferences` |
| #24134 | `transfer/getSpeedLimits`, `transfer/setSpeedLimits` | ✅ | Both modes' limits in bytes/s; `setSpeedLimits` is POST-only, needs all four keys and normalises them like `setPreferences` |
| #24253 | `max_outstanding_block_requests` | ⬜ | See Core/engine |
| #24135 | `torrents/downloadFile` (`hash`, `file` as index or relative path) | ✅ | `Session::completed_file` (only files of the torrent, complete), `open_inside` (canonical path inside the torrent folder, `O_NOFOLLOW`, regular file), streamed `Reply::File` with Content-Length; e2e `download_completed_files` incl. a symlink swap and login check |
| #24056 | `rss/cloneRule` (`sourceName`, `cloneName`) | ✅ | `RssManager::clone_rule` |
| #24043 | `share_limits_mode` in maindata, prefs and `setShareLimits` | ✅ | `torrents/info` and `sync/maindata` report the torrent's own mode; `app/preferences` `share_limits_mode` (unknown → MatchAny); `torrents/add` `shareLimitsMode`. Deviation: `setShareLimits` keeps `shareLimitsMode` optional (missing = Default) so clients written for 5.2 still work |
| #23856 (2.15.2) | `app/getFreeSpaceAtPath` | ✅ | `webui/api/app.rs:92`, route `webui/api/mod.rs:173` (the changelog line's `Action` suffix is the C++ method name; the real route is `getFreeSpaceAtPath`) |
| #23784 (2.16.1) | `search/status` has `pattern`, `category`, `plugins`; prefs `store_search_jobs`, `store_search_job_results` | ✅ | `webui/search_jobs.rs` ports `SearchJobManager`: jobs shared by all WebUI sessions (5 running at most), `status` = `{id, status, pattern, category, plugins, total}` (`plugins` = the list the job started with, `all`/`enabled` expanded), `Session.json` / `NNNN.json` under `data/WebUI/Search` (1 MiB / 10 MiB, atomic, 0600, same log messages), restored jobs Stopped, delete removes the results file, turning the prefs off deletes. Prefs after `confirm_torrent_deletion` (ini `Search/StoreSearchJobs`, `Search/StoreSearchJobResults`; imports also take 5.2's `Search/StoreOpenedSearchTab*`). Like 5.3's GUI, the macOS app's "Reopen search tabs at launch" / "Also store search results" now are these core prefs (old defaults moved once). Deviations: data read on the first `search/*` request, at most 50 jobs, stored values validated, corrupt results file logged. Tests: `webui/search_jobs.rs`, e2e `search_jobs_are_restored_and_shared_by_sessions`, `preferences_round_trip` |
| #24341 | Use torrent names for exported .torrent files | ✅ | `torrents/export` names the file after the torrent (`to_valid_file_name`), the hash as fallback; `Content-Disposition` carries an ASCII `filename` plus RFC 5987 `filename*` (`webui/webapp.rs` `content_disposition`) |
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
| #24134 | Unified global speed limit window | ✅ | The status bar's speed sheet edits the normal and alternative limits together |
| #24135 | Download completed files | ⬜ | Port after `torrents/downloadFile` exists (`details.ts` file menu) |
| #24246 | Respect excluded file names in Add torrent | ⬜ | Port after the WebAPI priority field |
| #24283 | Test e-mail notice instead of alerts | ⬜ | Endpoint exists (`webui/api/app.rs:49`); `settings.ts` has no e-mail test button, add one with an inline notice |
| #24404 | Skip the "Add new torrent" dialog | ⬜ | Port in `add.ts` with the matching pref |
| #24427 | Reuse category and plugin when refreshing a search | ⬜ | Port in `search.ts` (no refresh action yet); relies on the `search/status` fields |
| #24376 | Secondary sort column | ⬜ | `transfers.ts` sorts by one key |
| #24678 | "Copy path" in the content menu | ⬜ | Port in `details.ts` |
| #24692 | Non-torrent RSS link handling | ⬜ | Port in `rss.ts`; mirror `RSSLinkPolicy.swift` (only http(s), magnet and hashes are downloadable) |
| #24720 | Sessions limit setting | ✅ | Pref `web_ui_sessions_count_limit` (default 15, 0 = unlimited; `MAX_SESSIONS` still caps) drops the least recently used login; API-key sessions do not count (`webui/session.rs`); Settings field + WebUI settings page |
| #23784 | Keep search tabs across restarts and sessions | ✅ | The bundled Web UI has one search view (no tabs): its first visit shows the latest job from `search/status` again (pattern, category, single plugin, all results); Settings › RSS › Search has "Save search tabs" / "Also save search results" |
| #24249, #24269 | Start paused, shutdown timeout, pause/resume session | ✅ | Status bar button (pause icon, "Pause Session" / "Resume Session", highlighted while paused) from `server_state.session_state`; the two prefs are in All Settings |
| #24842 | Export/import RSS rules | ⬜ | Port in `rss.ts` after the endpoints exist |
| #24848 | Cap path autocompletion at 25 | ➖ | Upstream caps the client-side suggestion list only (the API still returns every entry). The NeoRS Web UI has no path autocompletion yet, so there is nothing to cap |
| #24920 | Missing Options page settings | ⬜ | `settings.ts` is a subset of the prefs; add the new keys together with the ones above |
| #24984 | Shared download window for RSS articles | ⬜ | Low; `rss.ts` downloads one article at a time |
| #24379 | Cross-origin links to webroot | ✅ | GET/HEAD of `/` and `/index.html` skip the CSRF check (`webui/webapp.rs`); e2e `csrf_and_host_validation` |
| #24210 | Creator dates as epoch seconds | ✅ | See WebAPI |
| #23878, #23837, #23818, #24708, #24710, #24804, #24816, #24879, #24686 | Folder collapse, in-place array filtering, set-rows speed, disabled instead of hidden menu items, header click errors, category indentation, version table layout, sortable properties panel, RSS refresh back-off | ➖ | Behaviour of upstream's MooTools/Mocha code; the NeoRS UI is different code. Spot-check folder collapse and disabled-vs-hidden menu items when touching `details.ts` |

## RSS

| PR | Change | Status | Notes |
|---|---|---|---|
| #24056 | "Clone rule" in the auto-downloader | ✅ | Toolbar button and context menu item "Clone rule…" with the "Rule cloning" prompt; the copy is disabled, history and last match cleared |
| #23963 | Sticky "All" RSS item | ✅ | `RSSNode.Kind.all` above "Unread" in the RSS window and the main window's sidebar RSS section (once feeds exist); shows every article; sticky rows can't be renamed, moved or deleted |
| #24120 | Add torrents from links in the article viewer | ✅ | Was missing: `magnet:` hrefs were dropped and other links only opened. Now `RSSLinkPolicy.isTorrentLink` (magnet, http(s) `*.torrent`) adds the torrent from `RSSArticleView`; the sanitizer keeps `magnet:` and relative-link resolution skips it. Unit tests `RSSLinkTests` |
| #24519 | Persist feed list sort order | ⬜ | Low: `RSSFeedList` has no sort order to persist; add sorting and remember it |
| #24806 | Clamp max articles per feed to non-negative | ✅ | `rss/mod.rs:179` clamps to `0..=i32::MAX` |
| #24082 | Added rule follows the "enabled" state | ➖ | Qt widget bug; check once while building clone rule |
| #24842 | RSS rules export/import over WebAPI | ✅ | See WebAPI |

## Search

| PR | Change | Status | Notes |
|---|---|---|---|
| #24912 | Keep cookies across HTTP redirects | ✅ | nova3 scripts from 5.3 (`helpers.py` 1.59 keeps cookies across redirects) |
| #24933 | Escape pipe characters properly | ✅ | `novaprinter.py` 1.56 percent-encodes the name and refuses `|` in other fields; `search/job.rs` decodes the name (test `field_rules`) |
| #25017 | Relaxed plugin versioning | ✅ | `PluginVersion` takes 1 to 3 components and prints without trailing zeros; the header is read in 22-byte chunks (tests in `search/nova.rs`) |
| #24548 | Revised plugin install procedure (unlisted: more checks, new log format) | ✅ | The install was already staged, syntax-checked and rolled back; now with 5.3's log lines ("Same or newer version…", "Search plugin has been updated…" for new plugins too, "Search plugin installation failed…"). The search HTTP proxy URL percent-encodes its credentials, as 5.3's QUrl does; SOCKS ones stay raw because helpers.py does not decode them |
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
