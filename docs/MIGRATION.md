# Migrating from qBittorrent 5.x

`qbit-neors-nox migrate-from-qbittorrent` builds a qBit NeoRS profile from a
qBittorrent 5.x installation (code: `crates/qbt-core/src/migrate/`).
qBittorrent's files are only read: copy nothing by hand, change nothing there.
The tool works on profile files, so qBit NeoRS must not be running on the
target profile. The tool refuses a profile in use: a process started with that
`--profile`, the app on the default profile, or a process holding the
profile's engine files open.

## Quick cutover (default profile)

1. Quit qBittorrent and qBit NeoRS. qBittorrent saves its resume data on exit.
2. `scripts/migrate-from-qbittorrent.sh` runs a dry run and prints the report.
3. `scripts/migrate-from-qbittorrent.sh --yes` migrates.
4. Start qBit NeoRS yourself. The script never launches anything.

If `~/Library/Application Support/qBit NeoRS` already holds a profile, the
script uses merge mode (below). Before writing, it copies the profile's
`config/`, `data/BT_backup` and `data/stats.json`, plus an export of the
`mk.lex.qbitneors` defaults domain, to `qBit NeoRS.before-migration-<time>/`.

## Command

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

## What is carried over

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

### Pieces shared with skipped files

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

## Merge mode (`--merge`)

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

## Not carried over

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
