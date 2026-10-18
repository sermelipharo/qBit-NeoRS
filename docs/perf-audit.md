# Performance audit

Scope: engine/, crates/qbt-core, crates/qbt-ffi, apps/macos. Method: reading
the hot paths, plus micro-measurements where cheap (debug build, shared target;
numbers are relative). Not measured: end-to-end swarm throughput (needs a
local multi-peer rig and is noisy on a shared machine), Instruments traces of
the SwiftUI app.

Files being changed by another agent and therefore only reviewed, not edited:
`engine/librqbit/src/peer_connection.rs`, `read_buf.rs`, `Properties/Trackers/*`,
`TransferCell.swift`.

## Already good (no action)

* Swift refresh loop slows to 5 s when the main window is hidden and polls only
  `serverState` (`AppModel.swift:136-160`); `TorrentItem` is updated per field so
  only changed cells redraw; `visibleItems` is reassigned only when the order
  changes; filtering/sorting live in the model, not in `body`.
* Core sync is record-level diffed (`bittorrent/sync.rs`).
* Per-torrent speed estimator already backs off to 1 s when idle
  (`live/mod.rs:343-385`); request window is 128 blocks (2 MiB) per peer with a
  rate-based window under a download limit (`live/mod.rs:1295-1313`); OS
  default socket buffers (auto-tuned); pwritev of chunks straight to the file
  (no copy).

## A. Implemented quick wins (commits on this branch)

1. **`TorrentEntry::info` computed the same O(files) values repeatedly**
   (`bittorrent/torrent.rs`, `info()`). Per torrent per refresh it called
   `wanted_size` / `completed_bytes` ~6 times (through `progress()`, `eta()` and
   directly), `total_done` twice, `info_hashes` three times (hex encoding),
   `root_path` twice (`content_path` calls it again). Now each is computed once.
   Measured (debug build, 2000-file torrent, 1000 calls): 389 ms -> 163 ms
   (2.4x less CPU in the loop that runs for every torrent on every GUI tick).
   Test `info_matches_individual_accessors` pins the result to the individual
   accessors. Risk: none (pure refactor).
2. **Session tick copied and sorted tracker lists for every torrent every
   tick** (`bittorrent/policy.rs` `update_torrents`): built an owned
   `Vec<TrackerTierEntry>` (URL clones), cloned it, cloned `e.trackers`, sorted
   both. Now compares by reference (`same_trackers`) and builds the owned list
   only on change. Saves ~2 allocations + 2 sorts + N string clones per torrent
   per tick. Test added.
3. **`distributed_copies` (scan of every piece) ran for every torrent with an
   engine handle, including paused ones** (`policy.rs`). The result is only
   displayed for live torrents (`TorrentEntry::availability`). Now skipped
   unless the engine phase is `Live`. For N paused incomplete torrents with P
   pieces that removed N*P work per tick (e.g. 500 torrents x 50k pieces = 25 M
   iterations per tick, on the thread holding the state write lock).
4. **Periodic "save all" rewrote and fsynced every `.fastresume` file**
   (`bittorrent/resume.rs` `BackupDir::save_resume`, default every 60 min, also
   at shutdown). `std::fs::File::sync_all` is `F_FULLFSYNC` on macOS (several ms to
   tens of ms each, flushes the drive cache). Now a hash of the last written
   bytes per torrent is kept (8 bytes per torrent) and an unchanged record is
   skipped (the file must still exist). 1000 torrents: up to seconds of blocking
   disk time and 1000 full-cache flushes avoided per cycle. Test added.

## B. Findings left for later, ordered by impact / effort

### B1. Quick but not done (touch FFI/Swift API or need a design call)

| # | Where | Problem | Impact | Fix | Risk |
|---|-------|---------|--------|-----|------|
| 1 | `live/mod.rs:2401` | A finished torrent's per-peer requester task sleeps 5 s in a loop for every connected peer that is still interested ("TODO: wait for a notification of interest"). 2000 seed peers = 400 wakeups/s. | idle CPU wakeups | await a `Notify` fired on peer-interest change / selected-files change, with the 5 s timeout kept as fallback | low |
| 2 | `speed_history` (`app/api_transfer.rs:41`, `PropertiesModel.swift:69`) | Speed tab refetches the whole history (up to 1800 `SpeedSample` x 11 `i64` = ~158 KB lowered through UniFFI) every tick and downsamples in Swift. The graph offers 3/6/12/24 h but `HISTORY_SECS` is 30 min (`speed.rs:8`), so those periods silently show 30 min. | CPU/alloc each tick while the Speed tab is open; correctness | `speed_history_since(ts)` returning only new samples; keep the buffer in the model. Optionally store more history downsampled in Rust | low |
| 3 | `RSSWindow.swift:52` | RSS window reloads all feeds/articles every 5 s while open; the model already observes core events. | wakeups/alloc while open | reload on RSS events only | low |
| 4 | `Statistics`, `ExecutionLogModel.pollBlockedPeers` (2 s), `PowerManager` (15 s) | Each runs its own poll while their window is open / always. Small. | negligible | fold into one event-driven source | low |
| 5 | `UnitFormatter.format` | `String(localized:)` for unit and "/s" on every call; `.formatted(.number...)` builds a style per call. Measured 2.75 us -> 0.97 us per call with cached names. Per tick that is ~50 cells, so under 0.1 ms. | negligible | static cache of unit names | none |
| 6 | `file_ops.rs:455` | `hash_piece` allocates and zeroes a 64 KiB buffer per piece and reads in 64 KiB steps (64 `pread`s for a 4 MiB piece). | small CPU while downloading fast | 256 KiB thread-local buffer | low |
| 7 | `session.rs:397-416` `main_data_snapshot` | Every tick clones every tracker URL and torrent id into a `BTreeMap<String, Vec<String>>`, sorts, then `diff` compares it with the previous one. O(torrents x trackers) allocations per tick. | CPU/alloc per tick with many torrents | keep the map in the session, update only when a torrent's tracker list changes (the dirty flag set in `policy.rs` already marks it), send a version counter | medium |
| 8 | `TorrentEntry::info` still builds `magnet_uri` (percent-encoding all trackers), `comment`, `created_by`, paths for every torrent each tick | allocation churn proportional to torrents | cache the static strings per entry (invalidate on name/tracker/url-seed change) or move static fields out of the per-tick record | medium (invalidation points) |

### B2. Bigger redesigns

1. **Per-torrent speed estimator task -> one session task** (`live/mod.rs:343-385`).
   Every live torrent (including every seeding one) owns a 100 ms (active) or
   1 s (idle) timer that takes the torrent-wide chunk-tracker read lock. 500
   seeding torrents = 500 wakeups/s forever; 20 active = 200/s. A single
   session ticker (the 1 s `peer_tick` in `peer_policy.rs:454` and
   `session_stats` 1 s timer could be merged into it) iterating live torrents
   would give one wakeup, and could read counters without the lock
   (`get_remaining_bytes` can be an atomic). Active torrents may need 250 ms
   resolution; use a "hot" list. Expected: idle wakeups from O(torrents) to ~1/s.
   Risk: speed/ETA smoothing behaviour must stay identical (tests exist for
   the estimator).
2. **`block_in_place` per received 16 KiB chunk** (`live/mod.rs:2738`). The
   whole of `write_to_disk` (pwrite, chunk bookkeeping, and when the piece
   completes the SHA over up to 16 MiB read back from disk) runs inside
   `block_in_place_with_semaphore`, so every chunk pays a semaphore acquire
   plus a potential worker hand-off (tokio spawns/wakes a replacement worker
   when the current one has queued tasks). At 100 MB/s that is ~6400 per second.
   Do the buffered `pwrite` (page cache, microseconds) without block_in_place and
   use it only for the piece verification, or hash on a dedicated hasher pool.
   Expected: lower CPU and latency jitter on fast links. Needs a benchmark with
   the existing local swarm test; risk medium (peer_connection.rs is being
   edited by another agent, coordinate).
3. **Torrent-wide `RwLock` write per chunk** (`live/mod.rs:2632`
   `lock_write("mark_chunk_downloaded")`, plus `lock_read("check_steal")` and the
   per-piece lock). With many peers on a fast link the writer serialises all
   peers. Track chunk completion per in-flight piece (atomic bitmap or the piece
   lock only) and take the torrent lock only on piece completion/steal. Risk
   high (stealing and endgame logic).
4. **Hash from memory instead of re-reading the piece** (`file_ops.rs:426-498`).
   `check_piece` re-reads every completed piece from disk. It usually hits the
   page cache, but on a loaded or encrypted volume it costs real reads and
   syscalls (one `pread` per 64 KiB). Keep an incremental SHA-1 state per
   in-flight piece when chunks arrive in order (typical: peers request a piece's
   chunks sequentially), falling back to the re-read for out-of-order pieces.
   Saves a full read of all downloaded data; risk medium (state per in-flight
   piece costs ~100 bytes).
5. **Field-level diffs across FFI.** `GuiSyncState::diff` sends the whole
   ~75-field `TorrentInfo` (magnet link, comment, four paths, creator ...) for
   every torrent that changed, and any downloading torrent changes every tick.
   Send a compact "hot" record (state, progress, speeds, eta, seeds/peers,
   amount left, ...) every tick and the static/rarely changing remainder only on
   change, with the Swift `TorrentItem` merging the two. Cuts FFI lowering,
   allocation and Swift `!=` comparisons by roughly 5-10x for active torrents.
6. **Core tick runs at `refresh_interval` regardless of GUI visibility**
   (`policy.rs:252`). With the window hidden and nothing active it still wakes
   every 1.5 s, builds `status` + tracker clones for every torrent, etc.
   Back off to e.g. 5 s when the GUI is not polling and no torrent is active
   (the loop is delta-based, so accounting stays correct), and make torrent
   updates incremental (only torrents the engine flagged as changed).
   `engine.status()` also allocates `file_progress` (O(files)) per torrent per
   tick plus a second Vec in `map_file_progress`.
7. **Sort/filter/recount on the main thread every tick** (`TransferListModel`
   `storeDidChange`: `recount()` + `refilter()` + full sort, O(N log N) with a
   closure comparator over enum switch). At 1000 torrents this is ~1-3 ms per
   tick; fine today. If it shows up in Instruments: recount/refilter only when a
   filter- or sort-relevant field changed (state, category, tags, trackers, the
   sort column), and insertion-sort the nearly sorted array.
8. **Disk write coalescing.** Chunks are written as they arrive in 16 KiB
   `pwritev`s. On APFS this is fine (page cache coalesces), but there is no
   explicit write-behind control; `F_NOCACHE` for large sequential downloads
   would avoid polluting the unified cache with data the app itself will not
   read again (hashing reads it right away, so only enable after verification).
   Low priority, measure first.
9. **uTP-after-TCP 1 s wait** (`stream_connect.rs:386`). Connection setup waits up
   to 1 s for TCP before trying uTP; harmless for throughput, adds latency for
   peers reachable only over uTP. Consider racing with a 250 ms head start.

## C. Throughput notes (reasoning, not measured)

* 128-block (2 MiB) window per peer is enough for ~100 ms RTT at 160 MB/s per
  peer; no slow-start limit was found. The 3 s `request_queue_time` under a
  download limit matches libtorrent.
* Piece selection is rarest-first with priorities; per-peer one piece at a time
  but chunks are queued ahead up to the window, so peers are not starved at piece
  boundaries.
* The throughput ceiling is most likely CPU-side per-chunk overhead (B2.2, B2.3),
  not the protocol: fix those before touching window sizes or buffers.
* `SO_RCVBUF`/`SO_SNDBUF` are set to 64 KiB for outgoing sockets only when
  tuning is non-default; the macOS defaults auto-tune upwards, so keeping them
  at 0 (OS default) is the right choice for high-latency, high-bandwidth peers.

## D. Status of follow-up items

| Item | Status | Commit |
|------|--------|--------|
| B1.2 Speed history | Done. Two fixed ring buffers in `bittorrent/speed.rs`: 1 s x 30 min plus 36 s means x 24 h (~35 KiB + ~48 KiB). New `speed_history_points(seconds, resolution, after_ms)` returns clock-aligned, complete buckets (max ~600 points; 24 h/144 s = 600) and only those newer than `after_ms`; the Speed tab appends them (full fetch only when the period changes). 3/6/12/24 h now show real data. Tests for alignment, incremental fetch and fixed memory. Debug build and SwiftLint clean. | `3b5c13f` |
| B1.3 RSS poll | Done: the 5 s timer is gone; the window reloads on the core's debounced `RssUpdated` event. | `e2606ab` |
| B1.7 Tracker map per tick | Done: the snapshot hashes all (torrent, tracker) pairs (no allocation) and builds the map only when the fingerprint differs from the last one sent; `GuiSyncState` skips the tracker diff otherwise. Test added. | `4243f07` |
| B1.8 Static strings | Not done. Caching the magnet link alone saves a few microseconds per torrent; the real fix is the hot/static record split (B2.5), which changes the FFI record. | - |
| B1.1 5 s per-peer poll | Done in `live/mod.rs` (outside `peer_connection.rs`): the idle requester of a finished torrent waits on its peer's interest/have/bitfield notification and the torrent's new-pieces notification, with a 30 s safety timeout instead of a 5 s poll. 186 engine tests and the qbt-e2e swarm tests pass. | `3ef1c42` |
| B2.1 Speed ticker | Done in a conservative form: a torrent whose sampler has seen no traffic for ~1 s parks its task (no timer at all). The session's existing 1 s peer tick samples parked torrents (same 1 s samples as before) and wakes the task when bytes move, so latency to resume 100 ms sampling is at most 1 s, as before. Idle seeding torrents now cost 0 timer wakeups of their own (was 1/s each: 500 seeding torrents = 500 wakeups/s fewer). Active torrents still use their 100 ms timer. Not measured with Instruments; reasoning from the timer count. | `d920306` |

Gates after these commits: `cargo test -p qbt-core` (353 passed), `cargo test -p librqbit --lib` (186 passed), `cargo test -p qbt-e2e`, clippy `-p qbt-core --features ffi` and `-p librqbit` with no warnings, Debug xcodebuild succeeded, SwiftLint clean.
