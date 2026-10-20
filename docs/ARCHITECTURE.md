# qBit NeoRS — architecture

qBit NeoRS is a rewrite of qBittorrent 5.2.4 (C++/Qt/libtorrent) as a
pure-Rust core with a native SwiftUI front end for macOS. The goal is feature
parity with qBittorrent 5.2.4 — including every preference — with a small
memory footprint and a native, pleasant UI.

The reference implementation is pinned at `reference/qBittorrent`
(git submodule, tag `release-5.2.4`). The parity checklists derived from it are:

* `docs/spec/features.md` — windows, menus, columns, states, dialogs, core
  behaviours, search/RSS internals, the complete WebAPI v2 (2.15.1).
* `docs/spec/preferences.md` — all 223 WebAPI preference keys with defaults,
  units, enum values and GUI labels, plus GUI-only settings.

## Layers

```
┌──────────────────────────────┐   ┌─────────────────────────────┐
│ apps/macos  (SwiftUI, Swift 6)│   │ Web UI (TypeScript, webui/) │
│  AppModel ⇄ generated bindings│   │  served by qbt-core::webui  │
└──────────────┬───────────────┘   └──────────────┬──────────────┘
               │ UniFFI (static lib)                │ HTTP /api/v2
┌──────────────┴────────────────────────────────────┴──────────────┐
│ crates/qbt-core   — application core (port of qBittorrent base/) │
│  app::App facade ─ bittorrent::Session ─ rss ─ search ─ creator  │
│  prefs ─ logger ─ events ─ net ─ webui (axum)                    │
└──────────────────────────────┬───────────────────────────────────┘
                               │ Rust API
┌──────────────────────────────┴───────────────────────────────────┐
│ engine/  — fork of librqbit 9.0.1 (BitTorrent protocol engine)   │
│  peers, piece picker, storage, trackers, DHT, uTP, LSD, UPnP     │
└──────────────────────────────────────────────────────────────────┘
```

* `engine/` — fork of the rqbit library crates (Apache-2.0). Changes are
  documented in `docs/ENGINE.md`. Keep engine changes generic (no qBittorrent
  concepts such as categories); expose mechanisms, the core owns policy.
* `crates/qbt-core` — everything qBittorrent's `src/base` and `src/webui` do.
  Feature `ffi` adds UniFFI derives/exports used by the macOS app.
* `crates/qbt-ffi` — `staticlib` that links `qbt-core` (+ `uniffi-bindgen` binary).
* `crates/qbt-nox` — headless daemon (`qbittorrent-nox` equivalent).
* `apps/macos` — XcodeGen project (`project.yml`), SwiftUI app, Swift 6 mode,
  macOS 27 deployment target, arm64.

## The facade: `qbt_core::App`

`crates/qbt-core/src/app/` is the single contract shared by every front end.
It is split by area (`api_torrents.rs`, `api_transfer.rs`, `api_app.rs`,
`api_log.rs`, `api_rss.rs`, `api_search.rs`, `api_creator.rs`); each file is an
`impl App` block exported to Swift with `#[cfg_attr(feature = "ffi", uniffi::export)]`.

Rules:

* The facade owns a multi-thread Tokio runtime (2–4 workers).
* Sync methods only touch in-memory state and must never block on I/O.
* Async methods (engine, disk, network) run on the app runtime through
  `App::spawn_on_rt`, because UniFFI polls futures on foreign threads with no
  Tokio context. Dropping the future aborts the task (Swift task cancellation).
* Data types live in `crates/qbt-core/src/model/` and `prefs/`. They derive
  `serde::Serialize` with the exact WebAPI JSON names and, with `ffi`, UniFFI
  `Record`/`Enum`. Values follow qBittorrent semantics (`-1` unknown, `8640000`
  infinite ETA, bytes/s, Unix seconds) so the WebAPI can serialize them as is.
* The GUI polls `App::sync_main_data()` every `refresh_interval` ms (record-
  level diffs) and fetches the properties tabs of the selected torrent on the
  same timer. Push notifications (finished, errors, log lines, RSS/search
  updates) arrive through `EventListener::on_event`.
* Errors are `QbtError` (UniFFI error enum in Swift).

## Core internals

* `CoreContext` (runtime handle, profile paths, `PreferencesStore`, `EventBus`,
  `Logger`, start-up config) is passed to every subsystem's `launch(ctx, ...)`.
* `PreferencesStore` persists `config/preferences.json` atomically and
  publishes changes on a `tokio::sync::watch` channel; subsystems subscribe and
  apply differences live (rate limits, queueing, WebUI restart, RSS timers ...).
* `EventBus` fans `CoreEvent`s out to the Swift listener and in-process
  subscribers.
* Profile layout (`~/Library/Application Support/qBit NeoRS/`):
  `config/preferences.json`, `config/categories.json`,
  `config/watched_folders.json`, `config/search.json` (disabled search
  plugins), `data/BT_backup/` (per torrent `<id>.torrent`
  + `<id>.fastresume` JSON), `data/rss/`, `data/nova3/`, `data/logs/`,
  `data/GeoDB/`, `data/stats.json`, `data/engine/`.

## macOS app

* `AppModel` (`@Observable @MainActor`) owns the `App` handle, a refresh task,
  and the stores (`TransferStore` with one `@Observable` `TorrentItem` per
  torrent, updated from diffs so only changed rows redraw).
* Scenes: main window (sidebar filters · transfer list · properties
  pane), `Settings` (toolbar panes built from one settings catalog with
  tiers and search; see `docs/design/settings-ia.md`), RSS, Search, Execution Log, Torrent
  Creator, Statistics, About; `MenuBarExtra` with speeds; Dock badge.
* Hidden windows give their views back (`releasedWhileHidden`, see
  `docs/hidden-ui-memory.md`): the main window, Settings, Log, Statistics and
  About drop their view trees after 2 minutes closed or 10 minutes fully
  occluded (`WindowReleasePolicy`, unit-tested in `QBTNeoUnitTests`) and
  rebuild when shown again. An occluded window is photographed first
  (`WindowSnapshot`, ScreenCaptureKit, kept as PNG data) and the picture
  covers the rebuild (`WindowSnapshotOverlay`). State that must survive lives
  in models (selection, sort, filters, search, properties tab, transfer list
  scroll position); column widths and split positions are autosaved.
* Large lists are AppKit tables (`Shared/Table`), because a SwiftUI `Table`
  keeps a hosting view per cell (about 88 KB each): the transfer list, the
  Trackers, Peers and Content tabs, the Add Torrent content tree, search
  results, the Execution Log, RSS articles and the filters sidebar (a source-list
  `NSOutlineView`). `NativeTableView` (context
  menu for the targeted rows, Return/Delete/Copy/Paste, no stripes behind an
  empty table) with plain `NSTextField`/drawn cells; `TableColumnsController`
  (header show/hide menu, reorder, resize, layout persisted as JSON in the GUI
  defaults); `ListTableController` for rows that a model rebuilds per fetch;
  `TransferTableController` and `FileTreeTableController` for models updated
  in place. Controllers follow the `@Observable` models through
  `Observations` and reconfigure only the realized cells of changed rows;
  cells compare their content, so unchanged cells are not redrawn.
* Torrent names (qBit NeoRS extension, `docs/design/torrent-names.md`):
  `Features/TorrentNames` splits names into a title and technical tags with
  ordered, editable regular-expression rules (pure parser, unit-tested;
  rules validated with a timed run, results cached per name in
  `TorrentNamesModel`, GUI defaults only); each result also carries a
  content kind (`ContentKindDetector`: fixed markers, tags, extension, then
  the category name). The list draws titles semibold; View › as Cards
  (⌘1/⌘2, or the toolbar's List / Cards switch) swaps the table for
  `TransferCardsController`, an `NSCollectionView` of reused self-drawn
  cards sized to their content (`TransferCardHeights`) that shares the list
  model's selection, sorting, filters and actions. Only the chosen view
  exists.
* The main menu is a hand-built `NSMenu` (`App/MainMenu`, rendered from the
  command registry and validated on demand) instead of SwiftUI `Commands`,
  whose per-item view graphs cost several MB; SwiftUI's default commands are
  emptied and `MainMenuController` restores its items whenever SwiftUI
  rewrites the main menu after a scene-graph change.
* GUI-only preferences (Dock speed, menu bar icon, confirmations, double-click
  actions, notifications, columns, toolbar) live in `UserDefaults`, as do the
  icons and colors of categories and tags (`LabelAppearanceStore`, keyed by
  name; the core's `categories.json` and the WebAPI stay unchanged). The SF
  Symbol names searched by the icon picker are bundled in
  `Resources/SFSymbolNames.txt` (`scripts/make-symbol-list.py`).
  They go through `AppDefaults.store`: the standard domain for the default
  profile, `<profile>/config/gui-preferences.plist` with `--profile`, a
  temporary-folder store for demo/snapshot runs. Such an instance also
  reroutes `UserDefaults.standard`'s `setObject:forKey:`,
  `removeObjectForKey:` and `objectForKey:` to its store
  (`StandardDefaultsRedirect`, installed first thing in `AppDelegate.init`),
  because AppKit autosaves window frames, split-view dividers, table columns
  and status-item positions into the standard domain; test instances must
  never touch the owner's `mk.lex.qbitneors` domain.
* Quitting goes through `TerminationController` (`App/Termination`; the
  steps are the pure, unit-tested `TerminationGate`). AppKit drops a quit,
  without asking the delegate, while a window shows a SwiftUI sheet, so every
  request is prepared first: the in-app Quit commands (menu, menu bar extra,
  start-up error, the core's shutdown request, the completion action) call
  `requestQuit()`, and the quit Apple event (Dock, other apps, log out) passes
  through `QuitEventInterceptor`, which wraps AppKit's own handler. Preparing
  asks the exit confirmation first when it applies (No keeps the sheets),
  then ends every sheet (`SheetDismissal`, innermost first). Then
  `applicationShouldTerminate` asks the lock password or the exit
  confirmation and saves resume data off the main thread before replying.
  `NSApp.terminate` and the reply run from the main run loop, never inside a
  main-actor task: AppKit waits for the reply in a nested run loop, and a
  main-actor job waiting there would starve the job that replies.
  `QBT_UI_SCRIPT=quit-presented` (DEBUG) checks quitting with sheets,
  alerts, menus and closed windows.
* Importing from qBittorrent or Transmission (`Features/Import`,
  `docs/MIGRATION.md`) never touches a live core: the sheet runs a dry run,
  records the request and restarts the app (`AppRelauncher`); the next
  launch migrates before `App::new`.
* `--snapshot <dir>` launch argument renders key windows with demo data to
  PNG files off-screen (UI verification without screen recording).
* Widgets (`apps/macos/QBTNeoWidgets`, a sandboxed WidgetKit extension:
  "Transfers" small/medium/large with a filter and category option, "Speed &
  Controls" small/medium, "Torrent" small/medium/large following one chosen
  torrent). The app writes a compact JSON snapshot (`apps/macos/WidgetShared`,
  typically 5–30 KB, at most ~105 KB) into the App Group container
  `YWA2X8G6VY.mk.lex.qbitneors` from its refresh loop: no extra timer, and
  nothing at all while no widget is installed (`WidgetCenter` configurations,
  plus a Darwin notification from a widget building its first timeline).
  `WidgetPublishPolicy` limits writes to changes (at most every 15 s) and
  timeline reloads to status/list changes (≥ 30 s apart), speed changes
  (≥ 3 min) and a 15-minute heartbeat. Torrents chosen in installed Torrent
  widgets (read from `WidgetCenter` configurations) are "pinned": published
  with details and their own speed history; up to 200 names feed the
  widget's torrent picker. Widget buttons post fixed Darwin notifications the
  app handles (start/stop all, alternative speed limits); a Torrent widget's
  Start/Stop first queues a small command file (`WidgetCommandQueue`).
  Clicking a Torrent widget opens `qbitneors://torrent/<hash>`, which selects
  the torrent (`AppModel.revealTorrent`). The widget never talks to the core.
  Only the default profile publishes (`--widgets` forces it for tests,
  `--widget-pin <hash>` pins a torrent).

## Web UI

`crates/qbt-core/webui/src` is the bundled Web UI: vanilla TypeScript and
plain CSS, no framework. `scripts/build-webui.sh` (Node, pinned esbuild;
`--check` also runs `tsc`) bundles it into one minified script and one
stylesheet in `webui/www/{public,private}`, adds content-hash cache busters
to the two HTML pages, and the output is committed, so cargo never needs
Node. `build.rs` embeds `www` gzip-compressed and the server sends the
compressed bytes as is (`webui/assets.rs`); `public/` is the login page,
`private/` (session only) the app. An alternative Web UI folder replaces it
entirely (qBittorrent's own WebUI still works there: its `QBT_TR` markers
are translated at serve time).

* State: `store.ts` mirrors `sync/maindata` (rid diffs merged into one
  long-lived record per torrent, each with a version counter); polling
  pauses while the tab is hidden.
* Lists (`vlist.ts`) are virtualized: only visible rows exist, elements are
  reused, and a row is redrawn only when its record's version changes; the
  list is re-filtered/re-sorted only when a diff touches the sort key or a
  filter field.
* Text from the server (names, tracker messages, RSS, search results) is only
  ever set through `textContent`; RSS descriptions are parsed in an inert
  `DOMParser` document and shown as plain text; links are opened only for
  `http(s)` URLs. The CSP for the built-in UI is `script-src 'self';
  style-src 'self'` (no inline code; styles set from scripts use the CSSOM).
* Layout: sidebar · list/cards · inspector on wide screens; below 760 px a
  drawer, a tab bar, two-line rows and bottom sheets (`css/phone.css`).

## Build

```
scripts/build-core.sh [debug|release]   # Rust static lib + Swift bindings + XCFramework
cd apps/macos && xcodegen generate       # Xcode project from project.yml
xcodebuild -project apps/macos/QBTNeo.xcodeproj -scheme QBTNeo build
cargo test --workspace                   # core + engine tests
```

The Xcode target runs `scripts/build-core.sh` as a pre-build phase. Xcode
copies the XCFramework into the products directory before that phase runs,
so the script also copies a rebuilt library and headers there.

Sibling project: qBittorrent Neo — separate repo, C++ qBittorrent core with this SwiftUI UI.

Peer ID: Azureus-style `-NR0210-` (client code `NR`, version 0.2.1.0).
