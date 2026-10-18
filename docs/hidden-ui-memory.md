# Freeing memory while the UI is hidden

Question: the app keeps running (menu bar item, Dock tile, widgets) after its
window is closed. How much memory can it give back while nobody looks at it,
and what does each approach cost when the window comes back?

Constraint from the owner: the Dock icon stays for users who want it (it shows
the speed badge, "Show speed in Dock", `GUIPreferences.showSpeedInDock`,
drawn by `DockBadgeController`/`DockBadgeView`). Dropping it can only be an
opt-in.

## Method

* Release build of this branch with temporary measurement hooks (not
  committed): Darwin notifications close / hide / minimize / reopen the main
  window without activating the app, call `malloc_zone_pressure_relief`, switch
  the activation policy, and log first-draw and "window visible" times. The
  hooks, prototypes and scripts live in the session scratchpad
  (`hiddenui/`), not in the repository.
* Test copy with its own bundle identifier, no widget extension, its own
  `--profile`, other ports; 16 generated sparse torrents (6 single-file, 6
  multi-file, 3 "season" folders with 320–780 files, one with 1,500 files),
  all seeding, tracker `127.0.0.1:9`, DHT/LSD/UPnP off. No peers, so no
  traffic (traffic adds the same amount to every variant: it lives in the
  core).
* Footprint is `vmmap --summary` "Physical footprint" (what Activity Monitor
  shows), CPU and wake-ups from `proc_pid_rusage` over 30–60 s. Main window
  1100 × 700 pt. Three runs per variant, medians reported (all runs in
  brackets where the spread matters). The machine was shared with other
  builds, so latencies are noisy; memory numbers are stable to ±1–2 MB.
* "Re-show" is the time from the request (`openWindow` / `unhide` /
  `deminiaturize`) until the window reports itself visible (occlusion state)
  or draws its first frame, whichever is later.

Baseline (window open, 16 torrents): **93 MB**. For reference
`qbit-neors-nox` with the same profile: **8.7 MB**.

The measurements were taken before main switched the main menu from SwiftUI
commands to AppKit (`MainMenuController`). A smoke run of the merged build
started at 74 MB with the window open (one run), so absolute numbers are now
lower; the differences between variants are not affected by the menu.

## Results

| Variant | Footprint hidden | vs open | Re-show latency | CPU hidden | Complexity |
|---|---|---|---|---|---|
| Window open (baseline) | 93.1 MB | — | — | — | — |
| Close window (today) | 94.3 MB | +1 | 520 ms [245–594] | 2.8 ms/s | — |
| ⌘H hide | 95.5 MB | +2 | 190 ms [171–341] | 6.3 ms/s | — |
| Minimize | 94.5 MB | +1 | 835 ms (Dock animation) | 1.3 ms/s | — |
| + `malloc_zone_pressure_relief` | ±0 (returns 0 bytes) | | | | trivial |
| **A** prototype: drop the view tree at close | 81.2 MB [77.7–82.4] | **−12** | 800–900 ms | 1.7 ms/s | small |
| **A** committed: release 20 s after close¹ | 77.9 MB [77.2–88.4] | **−16** | 230 ms visible, 720 ms content | 0.5 ms/s | small |
| **B** close + `.accessory` policy | 95.4 MB | +2 | 430 ms | 1.3 ms/s | trivial |
| A + B | 78.8 MB | −14 | 790 ms | 1.1 ms/s | small |
| **D** same binary, headless (AppKit only, Dock tile + status item) | 18.6 MB | **−75** | cold start ≈ 1.9 s [0.9–3.5] | 1.5 ms/s | medium |
| D, accessory (no Dock icon) | 17.7 MB | −75 | ≈ 1.9 s | 1.3 ms/s | medium |
| **C** helper process (AppKit, core, Dock tile, status item) | 18.6 MB | **−75** | ≈ 1.9 s + attach | 3.5 ms/s | large |
| C helper, accessory | 17.3 MB | −76 | | 2.9 ms/s | large |
| C helper without core (AppKit + status item + Dock only) | 13.9 MB | | | 2.6 ms/s | |

"Today" means the current code: the window is closed but SwiftUI keeps it.
Re-show latency of the cold-start rows is the launch-to-first-frame time of
the full app (`--profile` with the 16 torrents, median of 21 launches);
the demo-data UI without any core starts just as slowly (1.9 s), so the UI,
not the core, is the cold-start cost. The core itself is up 0.2–0.5 s after
launch in the headless runs.

¹ The committed delay is 120 s; the measured build used 20 s to keep runs
short. "Visible" is when the window is on screen, "content" when the rebuilt
main window drew its first frame. The test reopens without activating the app
(`openWindow` only), so the window becomes visible through the occlusion
fallback and shows the empty placeholder for ~0.5 s; a user-initiated reopen
activates the app and makes the window key, which triggers the rebuild before
the first frame. Wake-ups while released: 6/s versus 12/s for a kept closed
window (the kept hosting view still reacts to every status change).

### What SwiftUI does with a closed window

* `Window` (main window, RSS, Search, Log, Statistics, …) and `Settings`
  scenes **keep the closed `NSWindow` and its complete hosting view**: after
  closing, `NSApp.windows` still lists the `AppKitWindow` with the
  `AppKitWindowHostingView<MainWindow…>` and the footprint does not move
  (93 → 94 MB). The view graph stays live and keeps reacting to model changes.
  Opening the Log and Statistics windows adds 18 MB; closing them gives back
  nothing, and releasing their views gives back only ~2 MB (median 111.9 →
  109.8 MB): their cost is mostly fragmentation and feature models, not views.
* ⌘H and minimize keep everything too (by design).
* `.task` and `.onChange` inside a closed window stop running, so a timer that
  should fire *while* the window is closed cannot be built from them (the
  first version of the release timer used `.task(id:)` and never fired).
* A closed window does not lay out or display. When its SwiftUI content
  changes, the AppKit views SwiftUI removed (table, toolbar, CA layers) stay
  alive until the next layout pass: the first working release only gave back
  6 MB; forcing `layoutSubtreeIfNeeded()`/`displayIfNeeded()` on the closed
  window after the swap brought it to 16 MB.
* `malloc_zone_pressure_relief(nil, 0)` returned 0 bytes in every state.

### Why A only gets 12–16 MB of ~75

Dropping the main window's view tree frees **25 MB of live objects** (live
malloc 52.7 → 27.5 MB, 363k → 178k allocations), but fragmentation in the
small-allocation regions grows from 10.8 to 25.8 MB: the freed blocks sit on
pages that still hold a few long-lived objects (SwiftUI/AppKit caches, menu
items, metadata), so the pages stay dirty and the allocator cannot return
them. Pressure relief does not help. What remains in-process when hidden:

* ~28 MB live heap: SwiftUI scene infrastructure, the main menu (then built
  from `AppCommands`: 1,500 `PlatformItemList.Item` arrays; now AppKit), the `MenuBarExtra`
  scene, Swift metadata (4 MB), SF Symbol/CoreSVG data, AttributeGraph,
  `objc` method caches, the core (~4 MB);
* ~26 MB fragmentation from the freed views;
* ~20 MB dirty framework data, CoreAnimation, shared memory.

Only a process exit returns all of it, which is why C and D land at 18 MB.

Most of these pages are compressed by the system while idle (the "swapped"
column of `vmmap` holds 60 of the 65 MB of malloc), so the RAM actually
freed is smaller than the footprint drop. The footprint is still what users
see in Activity Monitor.

### B: accessory activation policy

Switching to `.accessory` after the window closes removes the Dock icon and
the app's main menu from the menu bar, but the menu objects stay allocated:
footprint is unchanged within noise (95.4 vs 94.3 MB; with A 78.8 vs 81.2).
Apple documents `.accessory` as "doesn't appear in the Dock and doesn't have
a menu bar", which also rules out running a full UI process as an accessory
app (option C below needs a `.regular` UI process). Not worth doing for
memory; at most an opt-in "Hide Dock icon while the window is closed".

### Dock tile cost

Measured with random speeds pushed every tick so the badge redraws every
time (worst case; it redraws only when the rounded text changes):

| | footprint | CPU | wake-ups |
|---|---|---|---|
| Helper, badge off (speeds 0) | 18.7 MB | 3.5 ms/s | 13.6/s |
| Helper, badge redrawn every 5 s | 20.7 MB (+2.0) | 4.7 ms/s (+1.2) | 14.2/s |
| Helper, badge redrawn every 1 s | 20.9 MB (+2.2) | 16.9 ms/s (+13.4) | 18.8/s |
| App (window closed), badge every 5 s | 96.1 MB (+2.3) | 9.3 ms/s (+3.3) | 13.7/s |

The 2 MB is the tile's content-view backing store. While hidden the app
refreshes every 5 s, so the Dock badge costs about 1 ms/s in a lean process
and ~3 ms/s in the SwiftUI app (the menu bar label redraws on the same
change). At the visible refresh rate (1.5 s) a redraw per tick costs ~10 ms/s.
Cheap enough to keep; the existing "only redraw when the text changes" rule
matters.

## Option C: core in a helper process

### Footprint and latency

The prototype (`hiddenui/helper/main.swift`, swiftc, links
`libqbt_ffi-release.a` and the UniFFI bindings, no SwiftUI) creates the core
for the profile, polls `serverState` every 5 s, shows an `NSStatusItem`
(text + `NSMenu`) and draws the same Dock badge. **18.6 MB** with the 16
torrents (17.3 MB as accessory, 13.9 MB without the core). Its extra cost over
`qbit-neors-nox` (8.7 MB) is the AppKit/WindowServer connection, status item
and Dock tile (~10 MB, mostly framework dirty data and CoreAnimation).

The **same binary in an AppKit-only mode** (option D's prototype: `@main`
entry that skips `QBTNeoApp.main()` and runs `NSApplication` with a small
delegate that creates the existing `AppModel`, `DockBadgeController` and an
AppKit status item) measures the same **18.6 MB**: linking SwiftUI costs
nothing until a scene is instantiated, and the existing models are cheap. So
the helper can be the app's own executable started with an `--agent` flag and
reuse the Swift code for notifications, widgets, completion actions and the
Dock badge, instead of a separate target.

With the UI open, C uses roughly helper + UI = 18.6 + ~85 MB (the UI minus its
in-process core) — about 10 MB more than today. The saving is entirely in the
hidden state.

Reopening = cold start of the UI process: **≈ 1.9 s** median to first frame
here (0.9–3.5 s on a loaded machine), versus 0.2–0.9 s for an in-process
re-show. Attaching to the helper adds one full `sync_main_data` (sub-ms over a
local socket for hundreds of torrents).

### Dock icon with two processes

Only a `.regular` process gets a Dock icon *and* a menu bar; an `.accessory`
process can show windows but has no menu bar. So the UI process must be
`.regular`, and two `.regular` processes would show two Dock icons. Workable
layouts:

1. **One bundle, two processes, policy hand-over (recommended for C).** The
   helper is the app's executable in `--agent` mode, `.regular` while no UI
   runs (Dock icon with badge, Dock menu, click → launch UI). When the UI
   process starts it takes over: helper switches to `.accessory`
   (`setActivationPolicy` works at runtime both ways), UI is `.regular` and
   draws the badge in its own tile. Both share the bundle identifier, so a
   pinned Dock item stays the same item; an unpinned icon may jump to the end
   of the Dock on hand-over. *Not verified here* (it needs activating apps and
   changing the Dock, which this investigation does not do).
2. A Dock tile plug-in (`NSDockTilePlugIn`, runs inside the Dock) can draw
   the badge of a *pinned* app that is not running, fed from the App Group
   snapshot. Then the helper never needs a Dock icon, but unpinned users get
   none while the UI is closed, and plug-ins are old, rarely used API.
3. Helper always `.accessory`, no Dock icon while the UI is closed: violates
   the Dock requirement.

### Mapping the facade to IPC

* The Swift side already talks to the core only through the `CoreClient`
  protocol family (`CoreClient`, `AppInfoClient`, `RSSClient`, `SearchClient`,
  `CreatorClient`: ~135 methods) plus `EventListener`. A remote client that
  conforms to them leaves models and views unchanged.
* Most calls are synchronous by design (in-memory, cheap). Over IPC each is a
  round trip (~50–200 µs locally); the bulky ones already go through
  `CoreCall.run` off the main actor. Calls made directly on the main actor
  would need to become async or stay cached in the UI.
* Transport options, cheapest first:
  * **Rust-side RPC over a Unix socket in the App Group container**: the
    facade records already derive `serde`; a `RemoteApp` object exported by
    UniFFI with the same method set forwards each call (bincode/postcard
    frames) and turns the event stream into `EventListener` calls. Generated
    by a macro from the `api_*.rs` signatures, so the two cannot drift.
  * NSXPC with a Swift protocol mirroring `CoreClient`: needs `NSSecureCoding`
    or `Codable` wrappers for ~150 record types, and a launchd-registered Mach
    service (`SMAppService.agent` with `MachServices`) for the UI to find the
    helper.
  * The existing WebAPI (`/api/v2`, `sync/maindata` diffs) on a Unix socket:
    no new protocol, but lossy (GUI-only calls, push events, typed errors) and
    the GUI would depend on WebAPI semantics.
* Profile ownership moves to the helper: it holds the profile lock and is the
  only process that opens `BT_backup`, preferences and stats. The UI never
  creates `App` itself; GUI-only preferences stay in `UserDefaults` (both
  processes may read them; the helper needs Dock/notification/menu bar keys).
* Single instance: the helper takes an exclusive `flock` on a file in the
  profile; a second helper exits, a second UI attaches to the running helper.
* Login item: `SMAppService.agent(plistName:)` with `--agent` in
  `ProgramArguments` (or `SMAppService.mainApp` plus a "start hidden" flag),
  replacing today's login-item code path.
* Widgets: `WidgetPublisher` moves to the helper (it already only needs
  `AppModel` stores); widget buttons' Darwin notifications go to the helper.
* Notifications: posted by the helper; same bundle identifier, so the same
  authorization and Notification Center entry.
* Dock menu, magnet URLs, `.torrent` "Open With", drag onto the Dock icon:
  delivered by LaunchServices to the `.regular` process of the bundle. The
  helper handles what needs no UI (add with defaults when "show add dialog"
  is off) and launches/forwards to the UI otherwise. With two processes of one
  bundle the receiver must be deterministic; routing everything through the
  helper's socket avoids depending on which one LaunchServices picks.
* Menu bar extra: lives in the helper as an AppKit `NSStatusItem` (+ `NSMenu`);
  the UI drops its `MenuBarExtra` while a helper exists.
* Crash isolation improves (UI crash no longer stops torrents), but there are
  now two processes to update/sign/notarize and version-match.

## Option D: relaunch into headless mode

Same binary, `--headless`: `NSApplication` with an AppKit delegate, no SwiftUI
scenes, `AppModel` + `DockBadgeController` + `NSStatusItem`. **18.6 MB**
(Dock icon kept) / 17.7 MB (accessory), CPU 1.5 ms/s, i.e. the same as the C
helper. The process is launched-to-core-ready in 0.5–1.1 s.

The catch is that a relaunch **restarts the torrent session** on every hide
and show: resume data is saved, all peer connections drop, trackers are
re-announced (and may rate-limit), checking/queue state starts over, and with
thousands of torrents start-up takes seconds. For a torrent client that is
worse than the memory. D as a plain relaunch is therefore only acceptable
with a long idle threshold (e.g. hidden for an hour) and as an opt-in, and it
still has the Dock "icon disappears and comes back" glitch during the
relaunch. Its real value is as the implementation of C's helper: the same
headless mode, but kept running while the UI process comes and goes.

## Recommendation

**Now (committed on this branch): A.** It is small, safe and keeps today's
behaviour (Dock icon, menu bar, instant reopen for the first 2 minutes):

* the main window, Settings, Log, Statistics and About drop their view trees
  after being closed for 2 minutes, or immediately on a memory-pressure
  warning, and rebuild when shown again (`ReleasedWhileClosed`, now
  `ReleasedWhileHidden`, and `WindowContentReleaser`; occluded windows were
  added later, see "Minimized, hidden and occluded windows" below). Windows whose models hold work in progress
  (Search, Torrent Creator, RSS, Cookies, Add Torrent) are left alone;
* the selected torrent's properties data (file tree, peers, trackers) is
  dropped with the main window, and properties are no longer fetched every
  5 s while the window is hidden (they were whenever the menu bar showed
  counts);
* `onOpenURL` moved out of the released part so magnets/files still open
  while the window is closed;
* a `DispatchSource` memory-pressure monitor releases closed windows at once
  and calls `malloc_zone_pressure_relief` afterwards (not exercised live:
  simulating pressure would stress the whole machine).

Measured: −16 MB for the main window (median of 3; one run only −6 MB),
~2 MB for Log + Statistics, half the wake-ups while hidden; reopening after a
release costs ~0.2–0.5 s more than reopening a kept window. Settings was not
measured.

**Not worth it: B** (no memory gain; only as an opt-in to hide the Dock icon).

**Later, if 90 MB hidden still matters: C built from D's headless mode.**
One binary with an `--agent` mode that owns the core, Dock tile, status item,
widgets and notifications (~18–21 MB measured), and the SwiftUI UI as a
second process of the same bundle that attaches over a Unix-socket RPC
generated from the facade and quits when its last window closes. Hidden
footprint drops from ~80–95 MB to ~19 MB (−75 MB); reopening becomes a ~1–2 s
cold start; the UI-open footprint grows by ~10 MB. Verify the Dock policy
hand-over (one icon, pinned slot stable) before committing to it; that is the
riskiest unknown. Rough size: RPC layer + remote client (1–2 weeks), helper
mode and hand-over (1 week), login item/widgets/notifications/URL routing
moves (1 week).

**Not recommended: D as a relaunch** (restarts the torrent session on every
hide/show).

## Properties pane

Question: how much do the details pane's tabs (General, Trackers, Peers,
HTTP Sources, Content, Speed) cost, and what can go without changing a
pixel? Release build, scratch `--profile` with the 16 generated torrents,
`--assume-visible`, window 1100 × 700 pt, `photos-0` (1,500 files)
selected, each tab visited for 8 s, then back to General and the window
closed. Footprint from `heap`/`footprint`, medians of 3 runs, ±1–2 MB noise
(Speed and the closed states up to ±4 MB).

| State | before | after |
|---|---|---|
| window open, nothing selected | 73.9 MB | 74.2 MB |
| General | 81.5 MB (heap 46.4) | 80.8 MB (heap 44.7) |
| Trackers / Peers / HTTP Sources | 83.7 / 83.2 / 84.8 | 82.5 / 82.1 / 83.4 |
| Content | 87.3 | 86.8 |
| Speed | 97.5 | 96.9 |
| back on General | 96.6 (heap 50.2) | 90.6 (heap 48.5) |
| window closed 15 s / 135 s | 95.8 / 93.4 | 90.7 / 88.5 |

What the pane costs and why most of it stays:

* Only the visible tab exists: `PropertiesPage` switches views, so hidden
  tabs keep no view tree (only their data: the Content file tree until
  another torrent is selected, ~0.5 MB for 1,500 files).
* General: ~5 MB of live heap for ~30 `Text` cells in two `Grid`s
  (Transfer group ~1.3 MB, Information ~0.9 MB, the rest scroll view, group
  boxes and per-view environment tracking). Selectable text was the largest
  single item (~2 MB: a text interaction view, two gesture recognizers and a
  TextKit layout manager per value); it is now attached on the first hover
  (`SelectableValue`).
* Speed: Swift Charts renders the plot into a Metal surface, ~5 MB of
  IOSurface plus ~2.4 MB of IOAccelerator memory while the tab is visible
  (freed when it is left), and ~1.5 MB of chart content and accessibility
  data rebuilt on every tick. `LinePlot` (vectorized) draws identical pixels
  and cuts ~2 MB of heap but neither footprint nor CPU (15 ms/s either way);
  hiding the chart from accessibility saves no CPU either. Not changed.
* HTTP Sources: the SwiftUI `Table` costs ~1 MB while shown. An AppKit table
  would be cheaper but cannot match its inset, rounded stripes pixel for
  pixel. Not changed.
* What visiting the tabs leaves behind is mostly one-time Swift metadata and
  method caches (~1.5 MB for Charts and tables) and malloc fragmentation from
  the transient allocations of each tab (5–8 MB), which only a process exit
  returns (see "Why A only gets 12–16 MB" above).

Changed (all pixel-identical in the `--snapshot` renders with a fixed demo
clock, and in window captures apart from live values): selectable values
on first hover, no empty tooltips on General labels, no data detector per
tick for the torrent comment, and `FileNode` is no longer `@Observable`
(the tables follow `FileTreeModel.rows`/`valuesRevision`; saves a registrar
and a lock per file and value update). Idle CPU on each tab is unchanged.

## Minimized, hidden and occluded windows

Question: can the closed-window release (A above) also cover a main window
that is minimized, hidden with ⌘H, or fully covered by other windows, and
come back without a visible flash?

### Method

Release build with temporary hooks (not committed): Darwin notifications to
order the window front without activating the app, hide/unhide, close/open;
minimizing through AX on the test instance's own pid; occlusion by a helper
process that orders an opaque borderless window over the test window without
activating anything. Scratch `--profile` with 56 generated sparse torrents
(no peers, DHT/LSD/UPnP off), window 1280 × 800 pt at 2x. Delays shortened
with a debug-only override (`QBT_WINDOW_RELEASE_DELAYS`), except the final
run, which used the real 2 and 10 minutes. Footprint from `footprint -p`,
surviving views counted with `heap` (`TransferTableController` instances),
owners traced with `leaks --traceTree`. Restores captured with
`screencapture -l <window id>` every ~200 ms.

### Results

| State | Footprint | Notes |
|---|---|---|
| Visible | 74 MB | real delays, one run |
| Minimized 3 min | 74 MB | not released (see below) |
| Occluded 5 min | 74 MB | before the 10-minute mark |
| Occluded 11 min, released | **61 MB (−13)** | snapshot kept: 615 KB PNG + 4 MB standby image |
| Restored, 5 s / 30 s | 75 / 74 MB | no growth after the rebuild |
| Occluded, released, 4 short cycles | 61–69 MB (visible 71–74) | with snapshot |
| Same without snapshot | 57–66 MB (visible 67–72) | the picture costs ~4–5 MB |
| Closed, released, 4 short cycles | 64–70 MB (visible 70–72) | existing path, for comparison |
| Minimized, released (experiment) | −1…−2 MB; visible grows 74 → 84–86 MB | old views stay alive |
| ⌘H hidden, released (experiment) | −1 MB; visible grows 73 → 86–87 MB | old views stay alive |

Restore timing (occluded): the app hears of the uncovering 59–81 ms after
the covering window goes away; the full picture is up then, and the rebuilt
content has drawn and the picture is faded out 440–630 ms after that (median
~470 ms). In every captured sequence the first frames after uncovering match
the final content (0.1–0.3 % of pixels differ: live values and the sidebar
icon tint noted below); no frame shows an empty or stretched window.

### Why minimized and hidden windows are left alone

SwiftUI does process the release while a window is minimized or the app is
hidden (`onDisappear` and `dismantleNSView` run), but the views it removes
stay alive: `leaks --traceTree` shows the old detail column's `NSHostingView`
held by its `NSSplitViewItem`, by pending `-[NSView _commonAwake]` blocks and
by SwiftUI observation entries, all of which are only flushed when the
window is displayed again. Forcing `display()`, `layoutSubtreeIfNeeded()`,
`viewWillDraw()`, a `CATransaction.flush()` or an occlusion notification
changes nothing. So the release frees 1–2 MB while hidden, and when the window
returns the old and new trees briefly coexist, which leaves ~10–12 MB of
fragmentation behind for good. Closed windows (ordered out by `close`) and
occluded windows (still displayed) free their views at once.

A minimized window's content is live: the deminiaturize animation and the
Dock's window image show whatever its layers contain at that moment (a
released window animates out of the Dock empty). AppKit posts no documented
notification before that animation; the undocumented
`_NSWindowWillBecomeVisible` arrives at its start and is observed as an early
restore signal, with `didDeminiaturize` as the documented fallback.

### Snapshot

* `cacheDisplay(in:to:)` of the frame view renders without the glass and
  vibrancy materials (the sidebar comes out blank), so the window server's
  own image is used: ScreenCaptureKit, `SCShareableContent.currentProcess`
  and `SCScreenshotManager` with a desktop-independent window filter. Own
  windows need no screen-recording permission, and occluded, minimized and
  hidden windows can be captured. 50–190 ms, off the main thread.
* A decoded 2560 × 1600 picture kept as layer contents cost ~14 MB, most of
  the gain. It is kept as PNG instead (lossless, 460–615 KB, 25 ms to encode,
  ~30 ms to decode; HEIC is 4× smaller but 100–200 ms to encode and over 1 s
  to decode the first time).
* An occluded window can be uncovered before the app is told, so a
  point-resolution copy (1280 × 800, 4 MB) stays in the window as a plain
  layer on the frame view; the full picture replaces it on the first sign of
  return (occlusion, key/main, unhide, ordering in), the rebuild starts at
  the same moment, and the picture fades out (0.15 s) once the new views
  have drawn. The traffic-light buttons are left uncovered.
* The picture is dropped, and the window rebuilt plainly, when the window's
  size, backing scale or appearance changed or it entered full screen.

### Not covered / open

* After an occluded restore of an inactive window, the gray sidebar icons
  (categories, tags, trackers) are drawn slightly lighter than at launch or
  after a closed-window reopen; the cross-fade shows that change. Reproducible,
  cosmetic; the cause (rows created while the window is on screen and
  inactive) was not pinned down.
* Releases are skipped and retried every 30 s while a sheet, popover or child
  window is open, a field is being edited, a mouse button is down or a menu is
  tracked. View state that lives only in views (sidebar scroll position,
  expanded tracker rows in the properties pane) starts over; the transfer
  list's scroll position is saved in the model.
* Stage Manager, Spaces switching and full screen were not exercised live;
  their window changes go through the same occlusion, resize and
  backing-property notifications.
