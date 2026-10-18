# Migrating from qBittorrent 5.x or Transmission

qBit NeoRS imports a qBittorrent 5.x installation or the Transmission app for
macOS (3.x and 4.x), from the app (File > Import from qBittorrent… /
Import from Transmission…, see "In-app import" below) or with
`qbit-neors-nox migrate-from-qbittorrent` / `migrate-from-transmission`.

## qBittorrent 5.x

`qbit-neors-nox migrate-from-qbittorrent` builds a qBit NeoRS profile from a
qBittorrent 5.x installation (code: `crates/qbt-core/src/migrate/`).
qBittorrent's files are only read: copy nothing by hand, change nothing there.
The tool works on profile files, so qBit NeoRS must not be running on the
target profile. The tool refuses a profile in use: a process started with that
`--profile`, the app on the default profile, or a process holding the
profile's engine files open.

### Quick cutover (default profile)

1. Quit qBittorrent and qBit NeoRS. qBittorrent saves its resume data on exit.
2. `scripts/migrate-from-qbittorrent.sh` runs a dry run and prints the report.
3. `scripts/migrate-from-qbittorrent.sh --yes` migrates.
4. Start qBit NeoRS yourself. The script never launches anything.

If `~/Library/Application Support/qBit NeoRS` already holds a profile, the
script uses merge mode (below). Before writing, it copies the profile's
`config/`, `data/BT_backup` and `data/stats.json`, plus an export of the
`mk.lex.qbitneors` defaults domain, to `qBit NeoRS.before-migration-<time>/`.

### Command

```
qbit-neors-nox migrate-from-qbittorrent --profile <dir> [options]
  --source-config <dir>   qBittorrent.ini, qBittorrent-data.ini   (default ~/.config/qBittorrent)
  --source-prefs <dir>    categories.json, watched_folders.json, rss/  (default ~/Library/Preferences/qBittorrent)
  --source-data <dir>     BT_backup, rss/articles, nova3, SearchUI (default ~/Library/Application Support/qBittorrent)
  --webui <url>           take preferences from a running qBittorrent (GET /api/v2/app/preferences only)
  --webui-prefs <file>    ... or from a saved copy of that JSON
  --dry-run               report only
  --merge                 merge into an existing profile
  --force                 write into a non-empty profile without merging
  --stop-all              every torrent stopped
  --skipped-file-data recover|unclaim
  --sandbox [--sandbox-listen-port N --sandbox-webui-port N]   test profile (see Verification)
```

The report goes to stdout and to `<profile>/migration/report.txt`. It never
contains the password hash, API key, tracker URLs (they can carry passkeys)
or cookies.

### What is carried over

* **Preferences:** every WebAPI preference (`app/preferences`), read from
  `qBittorrent.ini` through the ini key, type and unit table in
  `prefs_map.rs` (`docs/spec/preferences.md`) and applied with our
  `setPreferences` semantics. Also carried over: the WebUI switch, the WebUI
  password hash (`Password_PBKDF2` copied as is, so the same password works),
  the API key, start paused, subcategories, the shutdown timeout and the
  network interface name. The report lists every key that reads differently
  in our `app/preferences`. A log folder inside qBittorrent's data folder (its
  default) becomes the profile's `data/logs`.
* **Categories** (save and download paths, share limits), **tags**, **watched
  folders** (the full `watched_folders.json` options), **RSS** (`feeds.json`,
  `download_rules.json`, article files), **search plugins** (`nova3/engines`
  `*.py`/icons, the disabled list), **all-time statistics** and the
  alternative speed limit state.
* **GUI settings** that exist in the app (start minimized, confirmations,
  double-click actions, filters, history lists, search history, torrent
  creator, UI lock hash...). They are written to
  `config/gui-preferences.plist`, which the app reads for a `--profile` launch
  and the script imports into `mk.lex.qbitneors` for the default profile.
* **Torrents**, from `BT_backup/<id>.torrent` plus `<id>.fastresume`:
  * the `.torrent` bytes as they are (qBittorrent stores no announce list in
    them: the trackers come from the resume data);
  * save and download path, or automatic mode;
  * category, tags, name and comment;
  * stopped, started or forced state, and the stop condition;
  * content layout, renamed file paths (`mapped_files`, without the `.!qB`
    suffix) and file priorities;
  * trackers with their tiers, and web seeds;
  * per-torrent speed limits and ratio/seeding-time limits with their action;
  * added, completed and last-activity dates;
  * uploaded and downloaded totals, active and seeding time (ratio
    continuity);
  * sequential download, first/last piece priority, super seeding, and
    per-torrent DHT/PeX/LSD;
  * queue order (`queue` file);
  * the **piece bitfield**, as the engine's `data/engine/fastresume/<hash>.bitv`,
    so nothing is rechecked. At start the engine spot-checks a few pieces per
    file: if one fails, that torrent alone is fully checked. Without a usable
    bitfield (wrong length, seed mode, no metadata) the torrent is checked.

#### Pieces shared with skipped files

libtorrent keeps the bytes of "Do not download" files in a part file
(`<save path>/.<id>.parts`), not in the files. Our engine has no part file. A
piece shared by a wanted and a skipped file counts only if the skipped file's
bytes are on disk, otherwise the torrent reports missing files. With
`--skipped-file-data recover` (the default), the tool rebuilds each such piece
from the files on disk plus the part file and checks it against the piece
hash. If it passes, the tool creates the skipped file with those bytes: a new
file, never an overwrite, named with `.!qB` when partial and the
incomplete-suffix option is on. A piece that can't be verified is unclaimed
and downloaded again later. With `unclaim`, every such piece is unclaimed.
Then the wanted file next to it is one piece short, and qBit NeoRS renames it
to `.!qB` until the piece arrives.

### Merge mode (`--merge`)

Merge mode is for a profile that already has the torrents, for example after
adding them by hand:

* Torrents already in the profile keep their paths, category, tags, file
  priorities, state and pieces. Nothing is re-added or rechecked. They get:
  * qBittorrent's uploaded/downloaded totals and active/seeding time, added
    to what the profile counted since;
  * qBittorrent's added and completed dates;
  * qBittorrent's tracker tiers, with trackers only the profile has appended
    after them;
  * web seeds not yet present;
  * limits and share limits still at their defaults;
  * the queue position.

  The report notes differences it left alone (category, tags, save path,
  priorities, file paths).
* Torrents missing from the profile are added fully, as above.
* The profile's preferences, categories, tags and other files are kept;
  missing categories and tags are added. qBittorrent's GUI settings go to
  `migration/gui-preferences.plist`; the script imports them only with
  `--import-gui`.
* The global all-time statistics are added once. `migration/qbittorrent-merge.json`
  records what was merged, so running merge again changes nothing.

### Not carried over

* Window geometry, column layouts, dialog sizes, the Qt style and custom UI
  themes (Qt-specific). The report lists every ini key left out.
* Cookies (`Network/Cookies`). Add them again under Settings if a feed or
  tracker needs them.
* Partially downloaded pieces (`unfinished`): they start over. Custom piece
  priorities: file priorities are kept. BEP 17 HTTP seeds (`httpseeds`).
  Per-torrent SSL parameters. Known peer lists.
* qBittorrent's logs, GeoIP database (downloaded again) and execution state.
* The SQLite resume storage (`torrents.db`). Switch qBittorrent to "Fastresume
  files", restart and quit it, then migrate.


## Transmission (macOS app)

`qbit-neors-nox migrate-from-transmission` (code:
`crates/qbt-core/src/migrate/transmission.rs`) reads
`~/Library/Application Support/Transmission` (`Torrents/`, `Resume/`,
`Transfers.plist`, `queue.json`, `stats.json`) and the app's settings, the
`org.m0k.transmission` defaults domain (with `defaults export`, which only
reads, or a file given with `--defaults`). Quit Transmission first: it saves
its resume data when it quits, and the tool refuses to write while it runs.
Nothing of Transmission is written, and content files are only looked at
(existence and modification time), never moved or renamed.

```
qbit-neors-nox migrate-from-transmission --profile <dir> [options]
  --source <dir>        Transmission's folder (default ~/Library/Application Support/Transmission)
  --defaults <file>     exported settings (default: the org.m0k.transmission domain, read only)
  --dry-run             report only
  --merge | --force     as for qBittorrent
  --no-backup           don't copy the profile to <profile>.before-migration-<time> first
  --stop-all, --sandbox [--sandbox-listen-port N --sandbox-webui-port N]
```

With `--merge` or `--force` the profile's small files (`config/`,
`data/BT_backup`, `data/engine/fastresume`, `data/stats.json`,
`data/session_state.json`, `migration/`) are copied to
`<profile>.before-migration-<time>` first. The report goes to stdout and
`<profile>/migration/report.txt`; it never contains tracker URLs, the
blocklist URL or the RPC user name and password.

### What is carried over

* **Torrents** from `Torrents/<hash>.torrent` (4.x) or
  `<name>.<hash16>.torrent` (3.x) and the matching `.resume`, and magnet
  links without metadata (`<hash>.magnet`):
  * the `.torrent` bytes, trackers and web seeds from it;
  * the data folder (`destination`, or `incomplete-dir` while incomplete) as
    a manual save path (no automatic mode), the incomplete folder as the
    download path;
  * renamed torrent and file names (`name`, `files`), skipped files (`dnd`)
    and file priorities (high stays high; low becomes normal);
  * started/stopped (Transfers.plist `Active`/`WaitToStart`, else `paused`);
  * the group (Transfers.plist `GroupValue`) as the category, labels as tags;
  * per-torrent speed limits, ratio limit (own/unlimited/global) and idle
    limit (as the inactive seeding time limit), "remove when seeding
    completes" as the Remove share-limit action;
  * added, completed and last-activity dates, uploaded/downloaded totals,
    seeding and active time, sequential download;
  * queue order (`queue.json`, 3.x: the Transfers.plist order) for torrents
    still downloading;
  * **progress without a recheck** when the resume data proves it: a piece
    counts when all of its 16 KiB blocks are set in `progress.blocks`, and
    every file holding claimed data must still have the modification time
    Transmission recorded when it checked it (`progress.mtimes`, 3.x: not
    newer than `time-checked`). Then the bitfield is written as
    `data/engine/fastresume/<hash>.bitv` and the engine only spot-checks a
    few pieces per file at start. If a file changed, has no recorded time,
    or the bitfield doesn't fit the torrent, the torrent is checked in full.
    Missing files keep the bitfield and show as "Missing Files".
  * Incomplete files that carry Transmission's `.part` suffix keep that name
    (a file rename in qBit NeoRS) so their data is found; rename them once
    they complete. The ".!qB" option is left off.
* **Groups** used by torrents, or with their own download folder, become
  categories (that folder as the category save path). `GroupDicts` is an
  `NSKeyedArchiver` archive in the defaults; without it Transmission's seven
  color groups are assumed.
* **Settings:** download folder, incomplete folder, start added torrents,
  delete the .torrent after adding, the watch folder (`AutoImportDirectory`),
  speed limits and alternative limits (kB/s = 1000 B/s), the alternative
  speed schedule (days and times; the alternative speed state when no
  schedule), peer port, port forwarding, global and per-torrent peer limits,
  encryption (required or allowed), DHT, PeX, LSD, uTP, download/seed queue
  sizes, stalled torrents not counting (`CheckStalled`, `StalledMinutes`),
  ratio and idle seeding limits (action Stop), the removal confirmation.
  GUI settings: the quit confirmation, the Add Torrent dialog switch and
  preventing sleep.
* **All-time statistics** (`stats.json`).

### Not carried over

* Remote access (RPC): the report says whether it was on; its password lives
  in Transmission's keychain item and is never read. qBit NeoRS offers the
  qBittorrent Web UI/WebAPI instead.
* Group colors and automatic group rules (`NSPredicate`), bandwidth groups,
  bandwidth priority, per-torrent peer limits, known peers (`peers2`), the
  blocklist (downloaded from a URL; choose a list file under IP filtering),
  the DHT cache (`dht.dat`), "same folder as the .torrent" as the download
  location, a random port at each start, the done script (Transmission passes
  `TR_*` environment variables), bind addresses, window state and update
  settings. The report lists every one found.

### Merge mode

As for qBittorrent: torrents already in the profile keep paths, category,
tags, priorities, state and pieces, and get Transmission's statistics (added
once, `migration/transmission-merge.json`), dates, missing trackers, limits
left at their defaults and the queue position; the others are added fully;
the profile's settings stay, its GUI settings are only offered in
`migration/gui-preferences.plist`.

## In-app import

File > Import from qBittorrent… / Import from Transmission… (and, on the
first launch with an empty profile, an offer when either has torrents)
opens a sheet with the dry run: counts, settings taken over or kept ("Keep my
current qBit NeoRS settings"; always kept when the profile has torrents,
which means merge mode), what doesn't carry over and the full report. The
source application must be quit; the app never quits it.

The import never runs next to a live core. Confirming records
`migration/pending-import.json` (valid 15 minutes), quits normally (resume
data saved, engine stopped) and opens the app again once the process has
exited. The new instance applies the source's GUI settings to its defaults
(full import; old values in `migration/gui-defaults-before-import.plist`),
backs the profile up to `<profile>.before-migration-<time>`, migrates with
the same code as the command line, and only then starts its core. A result
sheet shows the counts, the backup and the report. Core side:
`crates/qbt-core/src/migrate/import.rs` (UniFFI functions).

## Verification (developers)

`--sandbox` writes a profile that cannot touch the network or anything
outside it:
* all torrents are stopped;
* DHT, LSD, PeX, UPnP/NAT-PMP, I2P, RSS, watched folders, e-mail, external
  programs, export folders and GeoIP are off;
* the WebUI listens on `127.0.0.1` on a free port.

Run it on a **copy** of qBittorrent's folders. Torrents still have to read
their data: never point a test profile at the real files. A torrent that
fails its spot check is fully checked, and the engine opens files read-write
while checking. Use APFS clones (`cp -c`) of the content, or no content at
all.
