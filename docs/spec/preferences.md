# qBittorrent 5.2.4 — Preferences Specification (parity checklist)

Reference build: **qBittorrent 5.2.4** (`src/base/version.h.in`: `QBT_VERSION_MAJOR 5`, `MINOR 2`, `BUGFIX 4`), WebAPI **2.15.1** (`API_VERSION {2, 15, 1}` in `src/webui/webapplication.h`).

Canonical key list: `src/webui/api/appcontroller.cpp` → `AppController::preferencesAction()` (GET, 223 keys) and `AppController::setPreferencesAction()` (SET, 221 keys). Defaults come from the `SessionImpl` constructor in `src/base/bittorrent/sessionimpl.cpp`, `src/base/preferences.cpp`, `src/app/application.cpp`, `src/base/net/proxyconfigurationmanager.cpp`, `src/base/bittorrent/portforwarderimpl.cpp`, `src/base/rss/rss_session.cpp`, `src/base/rss/rss_autodownloader.cpp` and `src/base/torrentfileguard.cpp`. GUI labels come from `src/gui/optionsdialog.ui`, `src/gui/optionsdialog.cpp` and `src/gui/advancedsettings.cpp`.

How the file is laid out:

* §0: conventions such as storage, the WebAPI read/write contract, units, sentinel values and how the Options dialog behaves.
* §1–§9: one section per Options-dialog page, in GUI page order: Behavior, Downloads, Connection, Speed, BitTorrent, Search, RSS, WebUI, Advanced. Each page has:
  * a table of **WebAPI-exposed keys**. `#` is the key's position in the `app/preferences` GET response; see Appendix A.
  * a table of **GUI-only preferences** on that page that the WebAPI does not expose.
* §10: WebAPI-only keys that have no widget in the desktop Options dialog.
* §11: GUI-only preferences and persisted state found **outside** the Options dialog: menus, dialogs, lock, tray, notifications, and so on.
* §12: enum reference, with every integer and string value.
* Appendix A: the exact GET key order. Appendix B: where SET differs from GET.

---

## 0. Conventions

### 0.1 Storage

* Settings live in one QSettings file named `qBittorrent.ini` (INI format on Windows and macOS) or `qBittorrent.conf` (native format on Linux), in the profile config directory. On macOS, `QSettings(IniFormat, UserScope, "qBittorrent", "qBittorrent")` resolves to `~/.config/qBittorrent/qBittorrent.ini`. The **ini key** column gives the exact key path so an existing installation can be imported. The SwiftUI app may use its own storage format, but it must be able to read these keys for migration.
* The data directory (`specialFolderLocation(SpecialFolder::Data)`) is `~/Library/Application Support/qBittorrent` on macOS. The default log folder is `<data>/logs`.
* A `SettingValue<bool>` with no explicit default reads as `false`. A `SettingValue<QString>` with no default reads as `""`.

### 0.2 WebAPI read/write contract

* `GET|POST /api/v2/app/preferences` returns one JSON object with all keys, in the order of Appendix A.
* `POST /api/v2/app/setPreferences` takes the form field `json` (required; missing → HTTP 400). Its value is a JSON object.
  * It is a **partial update**: only keys present in the object are applied. Unknown keys are silently ignored.
  * Values are coerced through `QVariant` (`toBool()`, `toInt()`, `toString()`, `toReal()`, `toLongLong()`). A malformed number becomes `0`.
  * When every key has been processed, `Preferences::apply()` writes the settings to disk. A successful call returns HTTP 200 with an empty body.
  * Only **two** validations raise HTTP 400 (`APIErrorType::BadParams`), each with a message:
    * `web_ui_username`: `"WebUI username must be at least 3 characters long"` or `"WebUI username cannot contain a colon"`
    * `web_ui_password`: `"WebUI password must be at least 6 characters long"`

    Keys processed before the failing one have already been applied when the error is raised. Processing is sequential and not transactional.
  * **Paired keys:** `schedule_from_hour` and `schedule_from_min` are applied only when **both** are present. The same holds for `schedule_to_hour` and `schedule_to_min`.
  * **Enable-flag + value pairs:** if `max_ratio_enabled` is present and false, the ratio becomes `-1` and `max_ratio` is ignored. Otherwise `max_ratio` (if present) is applied. `max_seeding_time_enabled`/`max_seeding_time` and `max_inactive_seeding_time_enabled`/`max_inactive_seeding_time` work the same way.
  * `random_port: true` sets the port to `0` and ignores `listen_port`.
  * **String enums** are Qt meta-enum key names (`Utils::String::fromEnum`/`toEnum`). An unparsable string falls back to the default named for each key.
* **Units:** global and alternative speed limits are **bytes/s** in the WebAPI, with `0` meaning unlimited. They are stored in **KiB/s** (`value / 1024`; any value from 1 to 1024 B/s is stored as 1 KiB/s). The GUI spin boxes show KiB/s and multiply by 1024 on save. Slow-torrent thresholds are KiB/s everywhere. `file_log_max_size` is KiB in the API and stored in bytes.

### 0.3 Sentinel values (BitTorrent share limits, `src/base/bittorrent/sharelimits.h`)

| Constant | Value | Meaning |
|---|---|---|
| `DEFAULT_RATIO_LIMIT` | `-2` | per-torrent: "use global" |
| `NO_RATIO_LIMIT` | `-1` | unlimited |
| `DEFAULT_SEEDING_TIME_LIMIT` | `-2` | per-torrent: "use global" |
| `NO_SEEDING_TIME_LIMIT` | `-1` | unlimited (minutes) |
| `Torrent::MAX_RATIO` | `+∞` | displayed ratio cap ("∞") |

Global setters clamp the values: a ratio `< 0` becomes `-1`, and minutes `< -1` become `-1`.

### 0.4 Options dialog behaviour (`src/gui/optionsdialog.cpp`)

* The page list on the left contains `"Behavior"`, `"Downloads"`, `"Connection"`, `"Speed"`, `"BitTorrent"`, `"Search"`, `"RSS"`, `"WebUI"`, `"Advanced"`, with icons `preferences-desktop`, `download`, `network-connect`, `speedometer`, `preferences-bittorrent`, `edit-find`, `application-rss`, `preferences-webui`, `preferences-advanced`. WebUI is hidden when the app is built with `DISABLE_WEBUI`. The window title is `"Preferences"` on Unix (and so on macOS).
* The buttons are OK / Cancel / **Apply**. Apply is disabled until any widget changes. OK applies if needed and then closes.
* The mouse wheel is ignored on every `QComboBox` and `QSpinBox` in the dialog, to avoid accidental edits.
* Persisted dialog state: `OptionsDialog/Size`, `OptionsDialog/HorizontalSplitterSizes` and `OptionsDialog/LastViewedPage` (row index).
* Validation on Apply/OK, in this order. On failure the dialog switches to the offending page:
  1. Scheduler "From" equal to "To": `QMessageBox::warning` titled `"Time Error"`: `"The start time and the end time can't be the same."` (Speed page).
  2. If WebUI is enabled:
     * username shorter than 3 characters: `"Length Error"` / `"The WebUI username must be at least 3 characters long."`
     * username containing `:`: `"Character Error"` / `"The WebUI username must not contain a colon."`
     * password shorter than 6 characters, unless the field is empty and a password is already stored: `"Length Error"` / `"The WebUI password must be at least 6 characters long."`
  3. Alternative WebUI enabled with an empty path: `"Location Error"` / `"The alternative WebUI files location cannot be blank."`
* Save order: Behavior, Downloads, Connection, Speed, BitTorrent, RSS, Search, WebUI, Advanced, then `Preferences::apply()`.
* The Behavior page shows the label `"Changing Interface settings requires application restart"` above the language, style and theme widgets.

---

## 1. Behavior page

### 1.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| 1 | `locale` | string | `""` stored → GET returns `QLocale::system().name()` (e.g. `"en_US"`) | Language codes taken from the bundled `qbittorrent_<code>.qm` files, e.g. `en`, `de`, `zh_CN`, `pt_BR`, `sr@latin`, `uz@Latn`, `az@latin`, `eo`, `ltg` | `"Language:"` (combo; each item shows `Utils::Misc::languageToLocalizedString(code)`) | UI language. Codes are matched exact first, then language-only (`de_AT`→`de`), then fall back to `en`. A SET through the API installs the translator immediately; the GUI needs a restart. · `Preferences/General/Locale` |
| 2 | `performance_warning` | bool | `false` | | `"Log performance warnings"` | Logs libtorrent `performance_alert`s to the execution log. · `BitTorrent/Session/PerformanceWarning` |
| 3 | `status_bar_external_ip` | bool | `false` | | `"Show external IP in status bar"` | Shows the last external IPv4/IPv6 address that libtorrent reported in the status bar. · `Preferences/General/StatusbarExternalIPDisplayed` |
| 4 | `confirm_torrent_deletion` | bool | `true` | | `"Confirm when deleting torrents"` (tooltip `"Shows a confirmation dialog upon torrent deletion"`) | When false, Delete removes the torrent without a dialog (keeping content) and Shift+Delete removes it with content. · `Preferences/Advanced/confirmTorrentDeletion` |
| 5 | `file_log_enabled` | bool | `true` | | Checkable group `"Log Files"` (mnemonic `&Log Files`) | Writes the execution log to disk. · `Application/FileLogger/Enabled` |
| 6 | `file_log_path` | string (dir path) | `<data>/logs` (macOS: `~/Library/Application Support/qBittorrent/logs`) | | `"Save path:"` (directory picker; caption `"Choose a save directory"`) | Folder that holds `qbittorrent.log`. · `Application/FileLogger/Path` |
| 7 | `file_log_backup_enabled` | bool | `true` | | `"Backup the log file after:"` (tooltip `"Creates an additional log file after the log file reaches the specified file size"`) | Rotates the log once it reaches the size limit. · `Application/FileLogger/Backup` |
| 8 | `file_log_max_size` | int (**KiB**) | `65` | API: value×1024 clamped to [1024 B, 1000 MiB]; GUI spin 1–1024000, suffix `" KiB"` | spin after the "Backup…" checkbox | Rotation threshold. Stored in **bytes** (`66560` by default). · `Application/FileLogger/MaxSizeBytes` |
| 9 | `file_log_delete_old` | bool | `true` | | `"Delete backup logs older than:"` | Deletes old rotated logs. · `Application/FileLogger/DeleteOld` |
| 10 | `file_log_age` | int | `1` | clamped 1–365 | spin | Age threshold, in the unit set by `file_log_age_type`. · `Application/FileLogger/Age` |
| 11 | `file_log_age_type` | int enum | `1` | `0`=days, `1`=months, `2`=years (out of range → 1) | combo `"days"`, `"months"`, `"years"` | `FileLogger::FileLogAgeType`. · `Application/FileLogger/AgeType` |
| 12 | `delete_torrent_content_files` | bool | `false` | | No Options widget. It is the **"Remember choice"** button for the `"Also remove the content files"` checkbox in the *Remove torrent(s)* confirmation dialog. | Default state of "also remove content" in the deletion dialog. · `Preferences/General/DeleteTorrentsFilesAsDefault` |

### 1.2 GUI-only preferences on the Behavior page (not in WebAPI)

Widgets appear in this order: Interface group → Transfer List group → Torrent Content View → Desktop → Power Management → Log Files → status-bar checkboxes.

| GUI label (exact) | ini key | Type | Default | Values | Semantics |
|---|---|---|---|---|---|
| Group `"Interface"`: `"Style:"` | `Appearance/Style` | string | `"system"` (Windows: `"Fusion"`) | First item `"System"` (data `system`, tooltip `"Let Qt decide the style for this system"`), then a separator, then `QStyleFactory::keys()` naturally sorted | Qt widget style. On Windows a hint reads `"%1 is recommended for best compatibility with Windows dark mode"` (Fusion). **SwiftUI:** no equivalent; omit. |
| `"Color scheme:"` | `Appearance/ColorScheme` | enum `ColorScheme` | `System` | Combo order `"Dark"`, `"Light"`, `"System"`; enum ints `System=0`, `Light=1`, `Dark=2` | Forces a light or dark appearance (Qt ≥ 6.8 only). **SwiftUI:** maps to `preferredColorScheme`. |
| Checkable group `"Use custom UI Theme"` | `Preferences/General/UseCustomUITheme` | bool | `false` | | Loads a custom theme. Only effective when the path is non-empty. |
| `"UI Theme file:"` | `Preferences/General/CustomUIThemePath` | path | `""` | file filter `"qBittorrent UI Theme file (*.qbtheme config.json)"`, caption `"Select qBittorrent UI Theme file"` | `.qbtheme` bundle or a `config.json` folder theme. |
| `"Use icons from system theme"` | `Preferences/Advanced/useSystemIconTheme` | bool | `false` | | Linux only; hidden on macOS. |
| Button `"Customize UI Theme..."` | (writes the theme JSON in config) | — | — | | Opens `UIThemeDialog` to edit icon and color overrides. Disabled while a custom theme is checked. |
| Group `"Transfer List"`: `"Use alternating row colors"` | `Preferences/General/AlternatingRowColors` | bool | `true` | | Zebra striping in lists. |
| `"Use different text colors by torrent states"` | `GUI/TransferList/UseTorrentStatesColors` | bool | `true` | | Each torrent state gets its own text color in the transfer list. |
| `"Make progress bars follow text colors"` | `GUI/TransferList/ProgressBarFollowsTextColor` | bool | `false` | Enabled only while the previous checkbox is on | The progress bar fill uses the state color. |
| `"Hide zero and infinity values"` | `Preferences/General/HideZeroValues` | bool | `false` | | Blanks out 0 and ∞ cells in the transfer list. |
| (combo next to it) | `Preferences/General/HideZeroComboValues` | int | `0` | `0` `"Always"`, `1` `"Stopped torrents only"` | Scope of the hiding. Enabled only when the checkbox is on. |
| Group `"Action on double-click"`: `"Downloading torrents:"` | `Preferences/Downloads/DblClOnTorDl` | int `DoubleClickAction` | `0` | Combo items in order: `"Start / stop torrent"` (0), `"Open destination folder"` (1), `"Preview file, otherwise open destination folder"` (2), `"Open torrent options dialog"` (4), `"No action"` (3) | Double-click action for incomplete torrents. Values outside 0–4 fall back to 0. |
| `"Completed torrents:"` | `Preferences/Downloads/DblClOnTorFn` | int | `1` | same items | Double-click action for completed torrents. Invalid values fall back to 1. |
| `"Auto hide zero status filters"` | `TransferListFilters/HideZeroStatusFilters` | bool | `false` | | Hides status-filter rows whose count is 0. |
| `"Use separate "Tracker status" filter"` (tooltip `Use separate "Tracker status" filter. Otherwise it gets merged with "Trackers" filter.`) | `TransferListFilters/SeparateTrackerStatusFilter` | bool | `false` | | Shows a separate sidebar filter widget for tracker status. |
| Group `"Torrent Content View"`: `"Drag content from qBittorrent"` (tooltip `"Replaces multi-selection (hold Alt key to invert)"`) | `Preferences/General/TorrentContentDragEnabled` | bool | `false` | | Lets files be dragged out of the Content tab to Finder. |
| Group `"Desktop"`: `"Start qBittorrent on Windows start up"` | Windows `Run` registry key | bool | `false` | | Windows only; hidden elsewhere. **macOS equivalent:** "Open at Login" via `SMAppService` (product decision). |
| `"Show splash screen on start up"` | `Preferences/General/NoSplashScreen` (inverted) | bool | `NoSplashScreen=true` → **unchecked** | | Shows a splash screen at launch. |
| `"Window state on start up:"` (tooltip `"qBittorrent window state on start up"`) | `GUI/StartUpWindowState` | enum `WindowState` | `Normal` | `"Normal"` (0), `"Minimized"` (1), `"Hidden"` (2, not offered on macOS) | Initial window state at launch. |
| `"Confirmation on exit when torrents are active"` (tooltip `"Shows a confirmation dialog when exiting with active torrents"`) | `Preferences/General/ExitConfirm` | bool | `true` | | Asks for confirmation before quitting while torrents are active. |
| `"Confirmation on auto-exit when downloads finish"` | `ShutdownConfirmDlg/DontConfirmAutoExit` (inverted) | bool | `DontConfirmAutoExit=false` → **checked** | | Shows a countdown dialog before running the "on downloads done" action. |
| Checkable group `"Show qBittorrent in notification area"` | `Preferences/General/SystrayEnabled` | bool | `true` | | Tray icon. Hidden on macOS. If no tray is available it is disabled, with tooltip `"Disabled due to failed to detect system tray presence"`. |
| `"Minimize qBittorrent to notification area"` (tooltip `"When minimizing, the main window is closed and must be reopened from the systray icon"`) | `Preferences/General/MinimizeToTray` | bool | `false` | | |
| `"Close qBittorrent to notification area"` (tooltip `"The systray icon will still be visible when closing the main window"`) | `Preferences/General/CloseToTray` | bool | `true` | | |
| `"Tray icon style:"` | `Preferences/Advanced/TrayIconStyle` | enum `TrayIcon::Style` | `0` | `"Normal"` (0), `"Monochrome (for dark theme)"` (1 `MonoDark`), `"Monochrome (for light theme)"` (2 `MonoLight`) | |
| Group `"File association"`: `"Use qBittorrent for .torrent files"` | OS (LaunchServices on macOS) | bool | from OS | | macOS: checked if qBittorrent is the default handler for `.torrent`. Applying with the box checked calls `MacUtils::setTorrentFileAssoc()`. Once set, the checkbox is **disabled** and cannot be unset from the app. Windows/macOS only. |
| `"Use qBittorrent for magnet links"` | OS | bool | from OS | | Same, for the `magnet:` URL scheme (`MacUtils::setMagnetLinkAssoc()`). |
| `"Check for program updates"` | `Preferences/Advanced/updateCheck` | bool | `true` | | Windows/macOS only. Checks for updates at startup (see features.md §1, program updater). |
| `"Show speed in Dock"` | `Preferences/Desktop/ShowSpeedInDock` | bool | `true` | | macOS only. The Dock tile badge shows DL/UL speed. |
| `"Show qBittorrent in menu bar"` | `Preferences/Desktop/ShowMacOSMenuBarIcon` | bool | `true` | | macOS only. Adds an `NSStatusItem` menu-bar icon with speeds and a menu. |
| Group `"Power Management"`: `"Inhibit system sleep when torrents are downloading"` | `Preferences/General/PreventFromSuspendWhenDownloading` | bool | `false` | | Re-evaluated every 60 s (`PREVENT_SUSPEND_INTERVAL`). On Linux without D-Bus the checkbox is disabled. |
| `"Inhibit system sleep when torrents are seeding"` | `Preferences/General/PreventFromSuspendWhenSeeding` | bool | `false` | | |
| `"Show free disk space in status bar"` | `Preferences/General/StatusbarFreeDiskSpaceDisplayed` | bool | `false` | | Free space of the default save path, shown in the status bar. |

---

## 2. Downloads page

### 2.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| 13 | `torrent_content_layout` | string enum | `"Original"` | `"Original"` (0), `"Subfolder"` (1), `"NoSubfolder"` (2). Unparsable → `Original` | Group `"When adding a torrent"` → `"Torrent content layout:"` combo `"Original"`, `"Create subfolder"`, `"Don't create subfolder"` (index = enum int) | `Original` keeps the torrent's own layout. `Subfolder` always creates a root folder named after the torrent, even for single-file torrents. `NoSubfolder` strips the torrent's root folder. · `BitTorrent/Session/TorrentContentLayout` |
| 14 | `add_to_top_of_queue` | bool | `false` | | `"Add to top of queue"` (tooltip `"The torrent will be added to the top of the download queue"`) | New torrents get queue position 0. · `BitTorrent/Session/AddTorrentToTopOfQueue` |
| 15 | `add_stopped_enabled` | bool | `false` | | `"Do not start the download automatically"` (tooltip `"The torrent will be added to download list in a stopped state"`) | Default "stopped" for new torrents. Checking it disables the stop-condition combo. · `BitTorrent/Session/AddTorrentStopped` |
| 16 | `torrent_stop_condition` | string enum | `"None"` | `"None"` (0), `"MetadataReceived"` (1), `"FilesChecked"` (2). Unparsable → `None` | `"Torrent stop condition:"` combo `"None"`, `"Metadata received"`, `"Files checked"`. Tooltip HTML: **None** – "No stop condition is set." **Metadata received** – "Torrent will stop after metadata is received." *"Torrents that have metadata initially will be added as stopped."* **Files checked** – "Torrent will stop after files are initially checked." *"This will also download metadata if it wasn't there initially."* | Default stop condition for new torrents. · `BitTorrent/Session/TorrentStopCondition` |
| 17 | `merge_trackers` | bool | `false` | | Group `"When adding a duplicate torrent"` → `"Merge trackers to existing torrent"` | When an already-present torrent is added again, its trackers and web seeds are merged into the existing torrent (private torrents excluded). · `BitTorrent/MergeTrackersEnabled` |
| 18 | `auto_delete_mode` | int enum | `0` | `0` Never, `1` IfAdded, `2` Always (`TorrentFileGuard::AutoDeleteMode`) | Checkable group `"Delete .torrent files afterwards "` (tooltip `"Whether the .torrent file should be deleted after adding it"`) with child `"Also when addition is cancelled"` (tooltip `"Also delete .torrent files whose addition was cancelled"`). Group unchecked = 0, checked = 1, checked + child = 2. When checked, a red icon and `"Warning! Data loss possible!"` appear, with an HTML tooltip explaining that files opened through file association are affected too | Deletes the source `.torrent` file after it is added (or also when the addition is cancelled). · `Core/AutoDeleteAddedTorrentFile` (stored as the enum *name*: `Never`/`IfAdded`/`Always`) |
| 19 | `preallocate_all` | bool | `false` | | `"Pre-allocate disk space for all files"` (tooltip `"Allocate full file sizes on disk before starting downloads, to minimize fragmentation. Only useful for HDDs."`) | Full storage allocation mode. · `BitTorrent/Session/Preallocation` |
| 20 | `incomplete_files_ext` | bool | `false` | | `"Append .!qB extension to incomplete files"` | Incomplete files carry the `.!qB` suffix until finished. · `BitTorrent/Session/AddExtensionToIncompleteFiles` |
| 21 | `use_unwanted_folder` | bool | `false` | | `"Keep unselected files in ".unwanted" folder"` | Files set to "Do not download" live in a `.unwanted` subfolder. · `BitTorrent/Session/UseUnwantedFolder` |
| 22 | `auto_tmm_enabled` | bool | `false` | | Group `"Saving Management"` → `"Default Torrent Management Mode:"` combo `"Manual"` (0 = false), `"Automatic"` (1 = true). Tooltip: `"Automatic: Various torrent properties (e.g. save path) will be decided by the associated category\nManual: Various torrent properties (e.g. save path) must be assigned manually"` | Default ATM state for new torrents. Stored inverted. · `BitTorrent/Session/DisableAutoTMMByDefault` (default `true`) |
| 23 | `torrent_changed_tmm_enabled` | bool | `true` | | `"When Torrent Category changed:"` combo `"Relocate torrent"` (true), `"Switch torrent to Manual Mode"` (false) | Whether an ATM torrent moves when its category changes, or drops to Manual mode instead. Stored inverted. · `BitTorrent/Session/DisableAutoTMMTriggers/CategoryChanged` (default `false`) |
| 24 | `save_path_changed_tmm_enabled` | bool | `false` | | `"When Default Save/Incomplete Path changed:"` combo `"Relocate affected torrents"` (true), `"Switch affected torrents to Manual Mode"` (false); GUI default index 1 | Same trigger, for a change to the default save or download path. · `BitTorrent/Session/DisableAutoTMMTriggers/DefaultSavePathChanged` (default `true`) |
| 25 | `category_changed_tmm_enabled` | bool | `false` | | `"When Category Save Path changed:"` same two items; GUI default index 1 | Same trigger, for a change to a category's save path. · `BitTorrent/Session/DisableAutoTMMTriggers/CategorySavePathChanged` (default `true`) |
| 26 | `save_path` | string path | the OS Downloads folder (`QStandardPaths::DownloadLocation`, e.g. `~/Downloads`) | | `"Default Save Path:"` (directory picker, caption `"Choose a save directory"`) | Default save path, and the base for relative category paths. · `BitTorrent/Session/DefaultSavePath` |
| 27 | `temp_path_enabled` | bool | `false` | | `"Use another path for incomplete torrents:"` | Turns on the global "download path" (incomplete folder). · `BitTorrent/Session/TempPathEnabled` |
| 28 | `temp_path` | string path | `<save_path>/temp` (computed from the save path at construction) | | path edit next to the checkbox (enabled only while it is checked) | Incomplete-download folder. Torrents are moved to the save path when they finish. · `BitTorrent/Session/TempPath` |
| 29 | `use_category_paths_in_manual_mode` | bool | `false` | | `"Use Category paths in Manual Mode"` (tooltip `"Resolve relative Save Path against appropriate Category path instead of Default one"`) | In Manual mode, a relative save path is resolved against the category path instead of the default path. · `BitTorrent/Session/UseCategoryPathsInManualMode` |
| 30 | `export_dir` | string path | `""` | `""` = disabled | `"Copy .torrent files to:"` (checkbox + path; caption `"Choose export directory"`) | Copies every added torrent's `.torrent` file here. · `BitTorrent/Session/TorrentExportDirectory` |
| 31 | `export_dir_fin` | string path | `""` | `""` = disabled | `"Copy .torrent files for finished downloads to:"` | Copies the `.torrent` file here when a download finishes. · `BitTorrent/Session/FinishedTorrentExportDirectory` |
| 32 | `scan_dirs` | object `{ "<folder path>": 0 \| 1 \| "<custom save path>" }` | `{}` | GET: `0` = save into the watched folder itself; `1` = default save path (the folder's AddTorrentParams.savePath is empty); string = custom save path. SET: int `0` → savePath = watched folder with ATM off; int `1` → existing options unchanged; string → that save path with ATM off. Folders absent from the object are **removed**. Invalid folders are skipped silently | Group `"Automatically add torrents from:"`: tree with column `"Watched Folder"`, buttons `"Add..."`, `"Options.."`, `"Remove"` (see §2.2) | **Deprecated** legacy view of watched folders. The full model lives in `watched_folders.json` (per folder: `recursive` plus full AddTorrentParams). · file `<config>/watched_folders.json` |
| 33 | `excluded_file_names_enabled` | bool | `false` | | Checkable group `"Excluded file names"` | Turns the excluded-file-name filter on. · `BitTorrent/ExcludedFileNamesEnabled` |
| 34 | `excluded_file_names` | string (`\n`-separated) | `""` | wildcard patterns | plain-text edit inside that group. Tooltip (verbatim): `Blacklist filtered file names from being downloaded from torrent(s).\nFiles matching any of the filters in this list will have their priority automatically set to "Do not download".\n\nUse newlines to separate multiple entries. Can use wildcards as outlined below.\n*: matches zero or more of any characters.\n?: matches any single character.\n[...]: sets of characters can be represented in square brackets.\n\nExamples\n*.exe: filter '.exe' file extension.\nreadme.txt: filter exact file name.\n?.txt: filter 'a.txt', 'b.txt' but not 'aa.txt'.\nreadme[0-9].txt: filter 'readme1.txt', 'readme2.txt' but not 'readme10.txt'.` | Matching files get priority "Do not download" when the torrent is added. The API SET splits on `\n` and keeps empty entries; the GUI drops empty lines. · `BitTorrent/Session/ExcludedFileNames` (string list) |
| 35 | `mail_notification_enabled` | bool | `false` | | Checkable group `"Email notification upon download completion"` (mnemonic `&upon`) | Sends an email when a torrent finishes. · `Preferences/MailNotification/enabled` |
| 36 | `mail_notification_sender` | string | `"qBittorrent_notification@example.com"` | | `"From:"` (tooltip `"Sender"`) | · `Preferences/MailNotification/sender` |
| 37 | `mail_notification_email` | string | `""` | | `"To:"` (tooltip `"Receiver"`) | Recipient. · `Preferences/MailNotification/email` |
| 38 | `mail_notification_smtp` | string | `"smtp.changeme.com"` | `host` or `host:port` | `"SMTP server:"` | · `Preferences/MailNotification/smtp_server` |
| 39 | `mail_notification_ssl_enabled` | bool | `false` | | `"This server requires a secure connection (SSL)"` | SSL/TLS SMTP. · `Preferences/MailNotification/req_ssl` |
| 40 | `mail_notification_auth_enabled` | bool | `false` | | Checkable group `"Authentication"` | SMTP AUTH. · `Preferences/MailNotification/req_auth` |
| 41 | `mail_notification_username` | string | `""` | | `"Username:"` | · `Preferences/MailNotification/username` |
| 42 | `mail_notification_password` | string | `""` | | `"Password:"` (password echo) | Stored in **plain text**. · `Preferences/MailNotification/password` |
| 43 | `autorun_on_torrent_added_enabled` | bool | `false` | | Group `"Run external program"` → checkable group `"Run on torrent added:"` | Runs a command when a torrent is added. · `AutoRun/OnTorrentAdded/Enabled` |
| 44 | `autorun_on_torrent_added_program` | string | `""` | trimmed on save; supports placeholders | line edit | Command line with placeholders (see the label below). · `AutoRun/OnTorrentAdded/Program` |
| 45 | `autorun_enabled` | bool | `false` | | Checkable group `"Run on torrent finished:"` | Runs a command when a torrent finishes. · `AutoRun/enabled` |
| 46 | `autorun_program` | string | `""` | trimmed | line edit | · `AutoRun/program` |

Help label under "Run external program" (`labelAutoRunParam`), verbatim:

```
Supported parameters (case sensitive):
    %N: Torrent name
    %L: Category
    %G: Tags (separated by comma)
    %F: Content path (same as root path for multifile torrent)
    %R: Root path (first torrent subdirectory path)
    %D: Save path
    %C: Number of files
    %Z: Torrent size (bytes)
    %T: Current tracker
    %I: Info hash v1 (or '-' if unavailable)
    %J: Info hash v2 (or '-' if unavailable)
    %K: Torrent ID (either sha-1 info hash for v1 torrent or truncated sha-256 info hash for v2/hybrid torrent)
    %M: Comment
Tip: Encapsulate parameter with quotation marks to avoid text being cut off at whitespace (e.g., "%N")
```

Button `"Send test email"` (GUI) calls `app()->sendTestEmail()` and then shows the information box `"Test email"` / `"Attempted to send email. Check your inbox to confirm success"`. The WebAPI equivalent is `POST /api/v2/app/sendTestEmail`.

### 2.2 GUI-only preferences on the Downloads page

| GUI label (exact) | ini key | Type | Default | Semantics |
|---|---|---|---|---|
| Checkable group `"Display torrent content and some options"` (mnemonic `Display &torrent…`) | `AddNewTorrentDialog/Enabled` | bool | `true` | Shows the *Add new torrent* dialog for torrents added manually. When off, torrents are added with the defaults. |
| `"Bring torrent dialog to the front"` | `AddNewTorrentDialog/TopLevel` | bool | `true` | Raises and activates the Add dialog. |
| `"Ask to merge trackers for manually added torrent"` | `GUI/ConfirmActions/MergeTrackers` | bool | `true` | Enabled only while the Add dialog is enabled. If not, it shows unchecked and is **not saved**. On a duplicate, asks "merge trackers?" instead of applying `merge_trackers` silently. |
| `"Enable recursive download dialog"` (tooltip `"When a torrent is downloaded, offer to add torrents from any .torrent files found inside it"`) | `Preferences/Advanced/DisableRecursiveDownload` (inverted) | bool | `DisableRecursiveDownload=false` → **enabled** | When a finished torrent contains `.torrent` files, offers to add them. |
| Watched-folder list: `"Add..."` → folder picker `"Select folder to monitor"` (starts at the parent of `Preferences/Downloads/ScanDirsLastPath`), then the **Watched Folder Options** dialog; `"Options.."` (enabled with exactly 1 row selected; double-clicking a row also opens it); `"Remove"` (enabled with ≥ 1 row selected) | `watched_folders.json`; last path `Preferences/Downloads/ScanDirsLastPath` | — | — | Add validation errors (`QMessageBox::critical` titled `"Adding entry failed"`): `"Watched folder path cannot be empty."`, `"Watched folder path cannot be relative."`, `"Folder '%1' is already in watch list."`, `"Folder '%1' doesn't exist."`, `"Folder '%1' isn't readable."`. The options dialog (`"Watched Folder Options"`) has `"Recursive mode"` (tooltip `"Will watch the folder and all its subfolders. In Manual torrent management mode it will also add subfolder name to the selected Save path."`) and a `"Torrent parameters"` AddTorrentParams widget (see features.md §3). |
| `"Show console window"` | `AutoRun/ConsoleEnabled` | bool | `false` | Windows only (hidden elsewhere). Runs the external program with a visible console. |

---

## 3. Connection page

### 3.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| 83 | `bittorrent_protocol` | int enum | `0` | `0` Both, `1` TCP, `2` UTP (clamped 0–2) | `"Peer connection protocol:"` combo `"TCP and μTP"`, `"TCP"`, `"μTP"` (top of the Connection page; in the API it sits in the "Speed" block) | Which peer transports are enabled. · `BitTorrent/Session/BTProtocol` |
| 47 | `listen_port` | int | stored `-1`, which means a **random port in [1024, 65535]** is picked at first start and persisted | `0`–`65535`; `0` = let the OS pick | Group `"Listening Port"` → `"Port used for incoming connections:"` (spin 0–65535, special text `"Any"` for 0, tooltip `"Set to 0 to let your system pick an unused port"`) and button `"Random"` (sets a random 1024–65535) | Incoming peer port (TCP and uTP). The `--torrenting-port` command-line option overrides it. · `BitTorrent/Session/Port` |
| 48 | `ssl_enabled` | bool | `false` | | **no GUI widget** | Enables the SSL-torrent listen socket. · `BitTorrent/Session/SSL/Enabled` |
| 49 | `ssl_listen_port` | int | stored `-1` → random 1024–65535, different from `listen_port` | 0–65535 | **no GUI widget** | Port for SSL torrents. · `BitTorrent/Session/SSL/Port` |
| 50 | `random_port` | bool | derived: `listen_port == 0` | | — | **Deprecated.** SET `true` → port 0. SET `false` is ignored. |
| 51 | `upnp` | bool | `true` | | `"Use UPnP / NAT-PMP port forwarding from my router"` | Port mapping through UPnP / NAT-PMP. · `Network/PortForwardingEnabled` |
| 52 | `max_connec` | int | `500` | `-1` = unlimited; any value ≤ 0 is stored as `-1` | Group `"Connections Limits"` → checkbox `"Global maximum number of connections:"` + spin (2–2147483647). Unchecked → `-1` | Global peer connection limit. · `BitTorrent/Session/MaxConnections` |
| 53 | `max_connec_per_torrent` | int | `100` | same | `"Maximum number of connections per torrent:"` + spin (2–…) | · `BitTorrent/Session/MaxConnectionsPerTorrent` |
| 54 | `max_uploads` | int | `20` | same | `"Global maximum number of upload slots:"` + spin (0–…) | Global number of unchoked peers. · `BitTorrent/Session/MaxUploads` |
| 55 | `max_uploads_per_torrent` | int | `4` | same | `"Maximum number of upload slots per torrent:"` | · `BitTorrent/Session/MaxUploadsPerTorrent` |
| 56 | `i2p_enabled` | bool | `false` | | Checkable group `"I2P (experimental)"`, shown only in libtorrent-2 builds with I2P | Connects to an I2P SAM bridge. · `BitTorrent/Session/I2P/Enabled` |
| 57 | `i2p_address` | string | `"127.0.0.1"` | | `"Host:"` | SAM host (trimmed). · `BitTorrent/Session/I2P/Address` |
| 58 | `i2p_port` | int | `7656` | 1–65535 | `"Port:"` | SAM port. · `BitTorrent/Session/I2P/Port` |
| 59 | `i2p_mixed_mode` | bool | `false` | | `"Mixed mode"` (tooltip: `If "mixed mode" is enabled I2P torrents are allowed to also get peers from other sources than the tracker, and connect to regular IPs, not providing any anonymization. This may be useful if the user is not interested in the anonymization of I2P, but still wants to be able to connect to I2P peers.`) | · `BitTorrent/Session/I2P/MixedMode` |
| 60–63 | `i2p_inbound_quantity`, `i2p_outbound_quantity`, `i2p_inbound_length`, `i2p_outbound_length` | int | `3`, `3`, `3`, `3` | quantity 1–16, length 0–7 | Advanced page rows (see §9) | libtorrent I2P tunnel settings. · `BitTorrent/Session/I2P/{InboundQuantity,OutboundQuantity,InboundLength,OutboundLength}` |
| 64 | `proxy_type` | string enum | `"None"` | `"None"` (0), `"HTTP"` (1), `"SOCKS5"` (2), `"SOCKS4"` (5). Unparsable → `None`. Stored ints outside [None, SOCKS4] → None | Group `"Proxy Server"` → `"Type:"` combo, in order `"(None)"`, `"SOCKS4"`, `"SOCKS5"`, `"HTTP"` | Proxy kind. · `Network/Proxy/Type` |
| 65 | `proxy_ip` | string | `""` if type is None, else `"0.0.0.0"` | | `"Host:"` | Proxy host (trimmed). · `Network/Proxy/IP` |
| 66 | `proxy_port` | int | `8080` | 1–65535 | `"Port:"` | · `Network/Proxy/Port` |
| 67 | `proxy_auth_enabled` | bool | `false` | | Checkable group `"Authentication"` (mnemonic `A&uthentication`); enabled only for SOCKS5 and HTTP | · `Network/Proxy/AuthEnabled` |
| 68 | `proxy_username` | string | `""` | | `"Username:"` | trimmed · `Network/Proxy/Username` |
| 69 | `proxy_password` | string | `""` | | `"Password:"` + note `"Note: The password is saved unencrypted"` | trimmed · `Network/Proxy/Password` |
| 70 | `proxy_hostname_lookup` | bool | `true` | | `"Perform hostname lookup via proxy"` (tooltip `"If checked, hostname lookups are done via the proxy"`) | Resolves hostnames remotely (SOCKS5/HTTP only). · `Network/Proxy/HostnameLookupEnabled` |
| 71 | `proxy_bittorrent` | bool | `false` | | Checkable group `"Use proxy for BitTorrent purposes"` | Routes tracker traffic (and, with the next key, peers) through the proxy. · `Network/Proxy/Profiles/BitTorrent` |
| 72 | `proxy_peer_connections` | bool | `false` | | `"Use proxy for peer connections"` (tooltip `"Otherwise, the proxy server is only used for tracker connections"`) | · `BitTorrent/Session/ProxyPeerConnections` |
| 73 | `proxy_rss` | bool | `false` | | `"Use proxy for RSS purposes"` (tooltip `"RSS feeds will use proxy"`) | · `Network/Proxy/Profiles/RSS` |
| 74 | `proxy_misc` | bool | `false` | | `"Use proxy for general purposes"` (tooltip `"Search engine, software updates or anything else will use proxy"`) | Also used when downloading the tracker list from URL. · `Network/Proxy/Profiles/Misc` |
| 75 | `ip_filter_enabled` | bool | `false` | | Group `"IP Filtering"` (mnemonic `IP Fi&ltering`) → `"Filter path (.dat, .p2p, .p2b):"` | Applies the IP filter file to peers. · `BitTorrent/Session/IPFilteringEnabled` |
| 76 | `ip_filter_path` | string path | `""` | file filter `"All supported filters (*.dat *.p2p *.p2b);;.dat (*.dat);;.p2p (*.p2p);;.p2b (*.p2b)"`, caption `"Choose an IP filter file"` | path edit + refresh tool button (icon `view-refresh`, tooltip `"Reload the filter"`) | eMule `.dat`, PeerGuardian text `.p2p`, or binary `.p2b`. · `BitTorrent/Session/IPFilter` |
| 77 | `ip_filter_trackers` | bool | `false` | | `"Apply to trackers"` | The filter also blocks tracker IPs. · `BitTorrent/Session/TrackerFilteringEnabled` |
| 78 | `banned_IPs` | string (`\n`-joined) | `""` | IPv4/IPv6 addresses; SET splits on `\n` and skips empty entries; stored sorted | Button `"Manually banned IP addresses..."` opens the *Banned IPs* dialog | Manual peer ban list. · `State/BannedIPs` |

Proxy-type UI rules (`adjustProxyOptions`):
* **(None):** host, port, hostname lookup, BitTorrent/peer/RSS/misc checkboxes and auth are all disabled.
* **SOCKS4:** the red label `"Some functions are unavailable with the chosen proxy type!"` is shown. Hostname lookup, RSS and misc are disabled. Auth is disabled.
* **SOCKS5 and HTTP:** everything is enabled, including auth.

IP filter refresh button: it forces a reload by setting the filter path to empty and then back to the chosen path, and shows a wait cursor. When parsing finishes it shows either `"Successfully refreshed"` / `"Successfully parsed the provided IP filter: %1 rules were applied."` or `"Parsing error"` / `"Failed to parse the provided IP filter"`. Pressing refresh also force-enables IP filtering.

### 3.2 GUI-only on Connection page

None beyond the buttons described above (`"Random"`, refresh, `"Manually banned IP addresses..."`).

---

## 4. Speed page

### 4.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| 79 | `dl_limit` | int **B/s** | `0` (unlimited) | ≥ 0 | Group `"Global Rate Limits"` (icon `slow_off`) → `"Download:"` spin 0–2000000, suffix `" KiB/s"`, special text `"∞"` for 0 | Global download limit. · `BitTorrent/Session/GlobalDLSpeedLimit` (**KiB/s**) |
| 80 | `up_limit` | int B/s | `0` | | `"Upload:"` (same spin) | Global upload limit. · `BitTorrent/Session/GlobalUPSpeedLimit` (KiB/s) |
| 81 | `alt_dl_limit` | int B/s | `10240` (10 KiB/s) | | Group `"Alternative Rate Limits"` (icon `slow`) → `"Download:"` | Download limit while alternative ("turtle") mode is on. · `BitTorrent/Session/AlternativeGlobalDLSpeedLimit` (KiB/s, default 10) |
| 82 | `alt_up_limit` | int B/s | `10240` | | `"Upload:"` | · `BitTorrent/Session/AlternativeGlobalUPSpeedLimit` (KiB/s, default 10) |
| 84 | `limit_utp_rate` | bool | `true` | | Group `"Rate Limits Settings"` → `"Apply rate limit to µTP protocol"` | uTP peers count toward the limits. · `BitTorrent/Session/uTPRateLimited` |
| 85 | `limit_tcp_overhead` | bool | `false` | | `"Apply rate limit to transport overhead"` | Protocol overhead counts toward the limits. · `BitTorrent/Session/IncludeOverheadInLimits` |
| 86 | `limit_lan_peers` | bool | `true` | | `"Apply rate limit to peers on LAN"` | Stored inverted. · `BitTorrent/Session/IgnoreLimitsOnLAN` (default `false`) |
| 87 | `scheduler_enabled` | bool | `false` | | Checkable group `"Schedule the use of alternative rate limits"` (mnemonic `Schedule &the…`) | Bandwidth scheduler: turns alternative limits on automatically inside the time window. · `BitTorrent/Session/BandwidthSchedulerEnabled` |
| 88 | `schedule_from_hour` | int | `8` | 0–23 | `"From:"` (tooltip `"Start time"`), `QTimeEdit` | Start of the window. Applied only together with `schedule_from_min`. · `Preferences/Scheduler/start_time` (QTime 08:00) |
| 89 | `schedule_from_min` | int | `0` | 0–59 | (same time edit) | |
| 90 | `schedule_to_hour` | int | `20` | 0–23 | `"To:"` (tooltip `"End time"`) | End of the window. · `Preferences/Scheduler/end_time` (QTime 20:00) |
| 91 | `schedule_to_min` | int | `0` | 0–59 | | |
| 92 | `scheduler_days` | int enum | `0` | `0` EveryDay, `1` Weekday, `2` Weekend, `3` Monday … `9` Sunday | `"When:"` combo: `"Every day"`, `"Weekdays"`, `"Weekends"`, then the localized full weekday names Monday…Sunday | Days on which the schedule applies. · `Preferences/Scheduler/days` |

Note: whether alternative mode is currently **active** (`BitTorrent/Session/UseAlternativeGlobalSpeedLimit`, default `false`) is not a preference key. It is toggled through `transfer/toggleSpeedLimitsMode`, the status-bar turtle button, or the scheduler.

### 4.2 GUI-only on Speed page

None. In the source, the macOS `"Show speed in Dock"` / `"Show qBittorrent in menu bar"` checkboxes are loaded and saved by the Speed-tab functions, but they appear on the Behavior page; see §1.2.

---

## 5. BitTorrent page

### 5.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| 93 | `dht` | bool | `true` | | Group `"Privacy"` → `"Enable DHT (decentralized network) to find more peers"` (tooltip `"Find peers on the DHT network"`) | · `BitTorrent/Session/DHTEnabled` |
| 94 | `pex` | bool | `true` | | `"Enable Peer Exchange (PeX) to find more peers"` (tooltip `"Exchange peers with compatible Bittorrent clients (µTorrent, Vuze, ...)"`) | · `BitTorrent/Session/PeXEnabled` |
| 95 | `lsd` | bool | `true` | | `"Enable Local Peer Discovery to find more peers"` (tooltip `"Look for peers on your local network"`) | · `BitTorrent/Session/LSDEnabled` |
| 96 | `encryption` | int enum | `0` | `0` = allow/prefer (libtorrent `pe_enabled` in and out), `1` = require (`pe_forced`), `2` = disable (`pe_disabled`). `allowed_enc_level = pe_rc4` | `"Encryption mode:"` combo `"Allow encryption"`, `"Require encryption"`, `"Disable encryption"`. Tooltip: `"Allow encryption: Connect to peers regardless of setting\nRequire encryption: Only connect to peers with protocol encryption\nDisable encryption: Only connect to peers without protocol encryption"` | On change the log shows `"Encryption support: ON/FORCED/OFF"`. · `BitTorrent/Session/Encryption` |
| 97 | `anonymous_mode` | bool | `false` | | `"Enable anonymous mode"` (tooltip `"Enable when using a proxy or a VPN connection"`) + link `"(More information)"` → `https://github.com/qbittorrent/qBittorrent/wiki/Anonymous-Mode` | libtorrent `anonymous_mode`: hides the client ID and user agent and does not send the listen port. · `BitTorrent/Session/AnonymousModeEnabled` |
| 98 | `max_active_checking_torrents` | int | `1` | `-1` = unlimited (special text `"∞"`), spin −1…INT_MAX | `"Maximum active checking torrents:"` | Number of hash checks that can run at once. · `BitTorrent/Session/MaxActiveCheckingTorrents` |
| 99 | `queueing_enabled` | bool | `false` | | Checkable group `"Torrent Queueing"` (mnemonic `&Torrent Queueing`) | Turns the queueing system on. · `BitTorrent/Session/QueueingSystemEnabled` |
| 100 | `max_active_downloads` | int | `3` | ≥ −1; −1 = `"∞"` | `"Maximum active downloads:"` | · `BitTorrent/Session/MaxActiveDownloads` |
| 101 | `max_active_torrents` | int | `5` | ≥ −1 | `"Maximum active torrents:"` | · `BitTorrent/Session/MaxActiveTorrents` |
| 102 | `max_active_uploads` | int | `3` | ≥ −1 | `"Maximum active uploads:"` | · `BitTorrent/Session/MaxActiveUploads` |
| 103 | `dont_count_slow_torrents` | bool | `false` | | Checkable group `"Do not count slow torrents in these limits"` | Inactive (slow) torrents do not count toward the active limits. · `BitTorrent/Session/IgnoreSlowTorrentsForQueueing` |
| 104 | `slow_torrent_dl_rate_threshold` | int **KiB/s** | `2` | 0–2000000 | `"Download rate threshold:"` (suffix `" KiB/s"`) | Tooltip on all three labels: `A torrent will be considered slow if its download and upload rates stay below these values for "Torrent inactivity timer" seconds`. · `BitTorrent/Session/SlowTorrentsDownloadRate` |
| 105 | `slow_torrent_ul_rate_threshold` | int KiB/s | `2` | | `"Upload rate threshold:"` | · `BitTorrent/Session/SlowTorrentsUploadRate` |
| 106 | `slow_torrent_inactive_timer` | int s | `60` | 1–999999 | `"Torrent inactivity timer:"` (suffix `" sec"`) | · `BitTorrent/Session/SlowTorrentsInactivityTimer` |
| 107 | `max_ratio_enabled` | bool (derived) | `false` | GET: `max_ratio >= 0` | Group `"Seeding Limits"` → `"When ratio reaches"` | |
| 108 | `max_ratio` | double | `-1` | `-1` = off; ≥ 0. GUI spin step 0.05; spin default `1.00` when first enabled | spin next to it | Global ratio limit. · `BitTorrent/Session/GlobalMaxRatio` |
| 109 | `max_seeding_time_enabled` | bool (derived) | `false` | GET: `max_seeding_time >= 0` | `"When total seeding time reaches"` | |
| 110 | `max_seeding_time` | int **minutes** | `-1` | `-1` = off; 0–9999999 in the GUI; spin default 1440 | spin, suffix `" min"` | · `BitTorrent/Session/GlobalMaxSeedingMinutes` |
| 111 | `max_inactive_seeding_time_enabled` | bool (derived) | `false` | | `"When inactive seeding time reaches"` | |
| 112 | `max_inactive_seeding_time` | int minutes | `-1` | as above; spin default 1440 | spin `" min"` | Seeding time with no upload activity. · `BitTorrent/Session/GlobalMaxInactiveSeedingMinutes` |
| 113 | `max_ratio_act` | int enum `ShareLimitAction` | `0` | `0` Stop, `1` Remove, `2` EnableSuperSeeding, `3` RemoveWithContent. SET values outside 0–3 → Stop | `"then"` combo, **GUI index ≠ enum value**: idx0 `"Stop torrent"` (0), idx1 `"Remove torrent"` (1), idx2 `"Remove torrent and its files"` (3), idx3 `"Enable super seeding for torrent"` (2). Enabled only when at least one limit is checked | Action when a global share limit is reached. `Default` (−1) is never stored globally (it maps to Stop). · `BitTorrent/Session/ShareLimitAction` |
| 114 | `add_trackers_enabled` | bool | `false` | | Checkable group `"Automatically append these trackers to new downloads:"` (mnemonic `A&utomatically…`) | · `BitTorrent/Session/AddTrackersEnabled` |
| 115 | `add_trackers` | string | `""` | One URL per line; a **blank line starts a new tier** (`parseTrackerEntries`) | plain-text edit | Trackers appended to every new torrent. · `BitTorrent/Session/AdditionalTrackers` |
| 116 | `add_trackers_from_url_enabled` | bool | `false` | | Checkable group `"Automatically append trackers from URL to new downloads:"` | Enabling it fetches the list now and then every **24 h**. Disabling it clears the fetched list and deletes `<data>/additional_trackers_from_url.txt`. · `BitTorrent/Session/AddTrackersFromURLEnabled` |
| 117 | `add_trackers_url` | string | `""` | trimmed URL | `"URL:"` | List source. Changing it re-fetches immediately if the feature is enabled. Download uses the "general purposes" proxy setting. Logs `"Tracker list updated"` or `"Failed to update tracker list. Reason: \"%1\""`. · `BitTorrent/Session/AdditionalTrackersURL` |
| 118 | `add_trackers_url_list` | string (**read-only**) | `""` | same text format as `add_trackers` | `"Fetched trackers"` (read-only text box) | Last downloaded tracker list. Persisted in `<data>/additional_trackers_from_url.txt`. |

---

## 6. Search page (GUI-only; no WebAPI keys)

| GUI label (exact) | ini key | Type | Default | Semantics |
|---|---|---|---|---|
| Group `"Search UI"` → checkable group `"Store opened tabs"` | `Search/StoreOpenedSearchTabs` | bool | `false` | Restores the open search tabs (their queries) at startup. |
| `"Also store search results"` | `Search/StoreOpenedSearchTabResults` | bool | `false` | Also saves and restores each tab's results. |
| `"History length"` (spin 0–99) | `Search/HistoryLength` | int | `50` (clamped 0–99) | Number of past queries kept in the search box's completer. |

Other persisted search state (not on the page): `Preferences/Search/SearchEnabled` (View → Search Engine, default `false`), `SearchEngines/disabledEngines` (string list), `SearchTab/UseRegexAsFilteringPattern` (`false`), `Search/FilteringMode`, `GUI/Qt6/SearchTab/HeaderState`. The `python_executable_path` key (§9) also concerns search.

---

## 7. RSS page

### 7.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| 149 | `rss_refresh_interval` | int **minutes** | `30` | 1–999999 in the GUI | Group `"RSS Reader"` → `"Feeds refresh interval:"` (suffix `" min"`) | Global auto-refresh period, used by every feed whose own refresh interval is 0 (a per-feed interval in seconds can be set in the feed options dialog or through `rss/setFeedRefreshInterval`). Changing it restarts the timer. · `RSS/Session/RefreshInterval` |
| 150 | `rss_fetch_delay` | int64 **seconds** | `2` | 0–2147483646 | `"Same host request delay:"` (suffix `" sec"`) | Minimum delay between requests to the same host. · `RSS/Session/FetchDelay` |
| 151 | `rss_max_articles_per_feed` | int | `50` | 0–2147483646 | `"Maximum number of articles per feed:"` | Older articles beyond this count are dropped. · `RSS/Session/MaxArticlesPerFeed` |
| 152 | `rss_processing_enabled` | bool | `false` | | `"Enable fetching RSS feeds"` | Master switch for RSS fetching. · `RSS/Session/EnableProcessing` |
| 153 | `rss_auto_downloading_enabled` | bool | `false` | | Group `"RSS Torrent Auto Downloader"` → `"Enable auto downloading of RSS torrents"` + button `"Edit auto downloading rules..."` (opens the RSS Downloader dialog) | · `RSS/AutoDownloader/EnableProcessing` |
| 154 | `rss_download_repack_proper_episodes` | bool | `true` | | Group `"RSS Smart Episode Filter"` → `"Download REPACK/PROPER episodes"` | The smart filter lets REPACK/PROPER releases of an already-downloaded episode through. · `RSS/AutoDownloader/DownloadRepacks` |
| 155 | `rss_smart_episode_filters` | string (`\n`-joined regexes) | the 4 default regexes below | GUI drops empty lines; API keeps them | `"Filters:"` (plain-text edit) | Regexes that extract the episode ID, combined with `|` and matched case-insensitively. · `RSS/AutoDownloader/SmartEpisodeFilter` (unset → defaults) |

Default smart-episode filters (`AutoDownloader::smartEpisodeFilters()`), one per line:
```
s(\d+)e(\d+)
(\d+)x(\d+)
(\d{4}[.\-]\d{1,2}[.\-]\d{1,2})
(\d{1,2}[.\-]\d{1,2}[.\-]\d{4})
```
(The source comments label them "Format 1: s01e01", "Format 2: 01x01", "Format 3: 2017.01.01" and "Format 4: 01.01.2017".)

Note: the `.ui` file gives the spin boxes defaults of 5 min and 100 articles. Those are placeholders; the effective defaults are the session values above (30 and 50).

---

## 8. WebUI page

### 8.1 WebAPI-exposed keys

| # | JSON key | Type | Default | Values / range | GUI label (exact) | Semantics · ini key |
|---|---|---|---|---|---|---|
| — | *(GUI only)* WebUI enabled | bool | `false` (GUI build); `true` (headless `nox` build) | | Checkable group `"Web User Interface (Remote control)"`. A red error label shows `"WebUI configuration failed. Reason: %1"` if the server failed to start | Runs the HTTP server. · `Preferences/WebUI/Enabled` |
| 119 | `web_ui_domain_list` | string | `"*"` | `;`-separated; `*` wildcard | Group `"Enable Host header validation"` → `"Server domains:"`. Tooltip: `"Whitelist for filtering HTTP Host header values.\nIn order to defend against DNS rebinding attack,\nyou should put in domain names used by WebUI server.\n\nUse ';' to split multiple entries. Can use wildcard '*'."` | · `Preferences/WebUI/ServerDomains` |
| 120 | `web_ui_address` | string | `"*"` | IPv4/IPv6, `0.0.0.0`, `::`, or `*` (trimmed) | `"IP address:"`. Tooltip: `"IP address that the Web UI will bind to.\nSpecify an IPv4 or IPv6 address. You can specify "0.0.0.0" for any IPv4 address,\n"::" for any IPv6 address, or "*" for both IPv4 and IPv6."` | · `Preferences/WebUI/Address` |
| 121 | `web_ui_port` | int (u16) | `8080` | 1–65535 | `"Port:"` | The `--webui-port` command-line option overrides it. · `Preferences/WebUI/Port` |
| 122 | `web_ui_upnp` | bool | `false` | | `"Use UPnP / NAT-PMP to forward the port from my router"` | · `Preferences/WebUI/UseUPnP` |
| 123 | `use_https` | bool | `false` | | Checkable group `"Use HTTPS instead of HTTP"` (mnemonic `&Use HTTPS…`) | · `Preferences/WebUI/HTTPS/Enabled` |
| 124 | `web_ui_https_cert_path` | string path | `""` | file filter `"Certificate (*.cer *.crt *.pem)"`, caption `"Select certificate"` | `"Certificate:"` + validity icon (`security-high` valid / `security-low` invalid) | PEM certificate chain. · `Preferences/WebUI/HTTPS/CertificatePath` |
| 125 | `web_ui_https_key_path` | string path | `""` | filter `"Private key (*.key *.pem)"`, caption `"Select private key"` | `"Key:"` + validity icon; link `"Information about certificates"` → `https://httpd.apache.org/docs/current/ssl/ssl_faq.html#aboutcerts` | · `Preferences/WebUI/HTTPS/KeyPath` |
| 126 | `web_ui_username` | string | `"admin"` | ≥ 3 characters, no `:` (API → HTTP 400) | Group `"Authentication"` → group `"User"` → `"Username:"` | · `Preferences/WebUI/Username` |
| — (SET only) | `web_ui_password` | string (plain text in) | none set → a **temporary 9-character password** is generated at each launch and logged: `"The WebUI administrator password was not set. A temporary password is provided for this session: %1"` | ≥ 6 characters (API → HTTP 400) | `"Password:"` (placeholder `"Change current password"`; empty = keep) | Stored as a PBKDF2 hash. · `Preferences/WebUI/Password_PBKDF2` |
| 133 (GET only) | `web_ui_api_key` | string | `""` | `"qbt_"` + 28 random characters (32 in total). Valid only if it has that prefix and length | Group `"API Key"` → `"Key:"` (read-only, placeholder `"Generate a key"`, shown **masked**: first 4 characters + `•`×(len−10) + last 6). Buttons: copy (tooltip `"Copy API key"`), generate/rotate (tooltip `"Generate API key"` or `"Rotate API key"`), delete (tooltip `"Delete API key"`) | Bearer API key. Generate/rotate/delete ask first. Generate: `"Generate API key"` / `"Generate an API key? This key can be used to interact with qBittorrent's API."`. Rotate: `"Rotate API key"` / `"Rotate this API key? The current key will immediately stop working and a new key will be generated."`. Delete: `"Delete API key"` / `"Delete this API key? The current key will immediately stop working."`. Yes/No with default No. These take effect **immediately** (they do not wait for Apply). WebAPI: `app/rotateAPIKey`, `app/deleteAPIKey`. · `Preferences/WebUI/APIKey` |
| 127 | `bypass_local_auth` | bool | `false` | | `"Bypass authentication for clients on localhost"` | Stored inverted. · `Preferences/WebUI/LocalHostAuth` (default `true`) |
| 128 | `bypass_auth_subnet_whitelist_enabled` | bool | `false` | | `"Bypass authentication for clients in whitelisted IP subnets"` + button `"IP subnet whitelist..."` (enabled only while the box is checked) | · `Preferences/WebUI/AuthSubnetWhitelistEnabled` |
| 129 | `bypass_auth_subnet_whitelist` | string (`\n`-joined CIDRs) | `""` | SET splits on `\n` **or** `,` and skips empty entries; unparsable subnets are dropped when read | (edited in the IP subnet whitelist dialog) | · `Preferences/WebUI/AuthSubnetWhitelist` |
| 130 | `web_ui_max_auth_fail_count` | int | `5` | 0 = never ban (special text `"Never"`) | `"Ban client after consecutive failures:"` | · `Preferences/WebUI/MaxAuthenticationFailCount` |
| 131 | `web_ui_ban_duration` | int **seconds** | `3600` | ≥ 1 in the GUI | `"ban for:"` (suffix `" sec"`) | · `Preferences/WebUI/BanDuration` |
| 132 | `web_ui_session_timeout` | int seconds | `3600` | 0 = disabled (special text `"Disabled"`) | `"Session timeout:"` (suffix `" sec"`) | Idle session expiry. · `Preferences/WebUI/SessionTimeout` |
| 134 | `alternative_webui_enabled` | bool | `false` | | Checkable group `"Use alternative WebUI"` + link `"List of alternative WebUI"` → `https://github.com/qbittorrent/qBittorrent/wiki/List-of-known-alternate-WebUIs` | · `Preferences/WebUI/AlternativeUIEnabled` |
| 135 | `alternative_webui_path` | string path | `""` | directory; caption `"Choose Alternative UI files location"` | `"Files location:"` | Must not be empty when enabled (GUI). · `Preferences/WebUI/RootFolder` |
| 136 | `web_ui_clickjacking_protection_enabled` | bool | `true` | | Group `"Security"` → `"Enable clickjacking protection"` | Adds `X-Frame-Options: SAMEORIGIN` and a CSP `frame-ancestors 'self'` header. · `Preferences/WebUI/ClickjackingProtection` |
| 137 | `web_ui_csrf_protection_enabled` | bool | `true` | | `"Enable Cross-Site Request Forgery (CSRF) protection"` | Checks Origin/Referer. · `Preferences/WebUI/CSRFProtection` |
| 138 | `web_ui_secure_cookie_enabled` | bool | `true` | | `"Enable cookie Secure flag (requires HTTPS or localhost connection)"` | · `Preferences/WebUI/SecureCookie` |
| 139 | `web_ui_host_header_validation_enabled` | bool | `true` | | Checkable group `"Enable Host header validation"` | · `Preferences/WebUI/HostHeaderValidation` |
| 140 | `web_ui_use_custom_http_headers_enabled` | bool | `false` | | Checkable group `"Add custom HTTP headers"` | · `Preferences/WebUI/CustomHTTPHeadersEnabled` |
| 141 | `web_ui_custom_http_headers` | string | `""` | `Header: value` per line | plain-text edit (placeholder `"Header: value pairs, one per line"`) | · `Preferences/WebUI/CustomHTTPHeaders` |
| 142 | `web_ui_reverse_proxy_enabled` | bool | `false` | | Checkable group `"Enable reverse proxy support"` + link `"Reverse proxy setup examples"` | Honors `X-Forwarded-For` from trusted proxies. · `Preferences/WebUI/ReverseProxySupportEnabled` |
| 143 | `web_ui_reverse_proxies_list` | string | `""` | `;`-separated IPs/subnets | `"Trusted proxies list:"` (tooltip `"Specify reverse proxy IPs (or subnets, e.g. 0.0.0.0/24) in order to use forwarded client address (X-Forwarded-For header). Use ';' to split multiple entries."`) | · `Preferences/WebUI/TrustedReverseProxiesList` |
| 144 | `dyndns_enabled` | bool | `false` | | Checkable group `"Update my dynamic domain name"` (mnemonic `Upda&te…`) | Dynamic DNS updater. It checks the external IP through `http://checkip.dyndns.org` every 30 min and on a change calls `https://members.dyndns.org/nic/update` (DynDNS) or `https://dynupdate.no-ip.com/nic/update` (No-IP). · `Preferences/DynDNS/Enabled` |
| 145 | `dyndns_service` | int enum | `0` | `0` DynDNS, `1` NoIP (`-1` None, internal) | `"Service:"` combo `"DynDNS"`, `"No-IP"` + button `"Register"` (opens `https://account.dyn.com/entrance/` or `https://www.noip.com/remote-access`) | · `Preferences/DynDNS/Service` |
| 146 | `dyndns_username` | string | `""` | | `"Username:"` | · `Preferences/DynDNS/Username` |
| 147 | `dyndns_password` | string | `""` | | `"Password:"` | Plain text. · `Preferences/DynDNS/Password` |
| 148 | `dyndns_domain` | string | `"changeme.dyndns.org"` | | `"Domain name:"` | · `Preferences/DynDNS/DomainName` |

### 8.2 IP subnet whitelist dialog (GUI)

Opened from `"IP subnet whitelist..."`. It edits `bypass_auth_subnet_whitelist`. Its size is stored in `IPSubnetWhitelistOptionsDialog/Size`. Field details are in features.md §3.

---

## 9. Advanced page (`src/gui/advancedsettings.cpp`)

The Advanced page is a two-column table with headers `"Setting"` and `"Value"`. It uses alternating row colors, has no selection, and has no editing triggers: every value is an embedded widget. Row labels are rich text, and many end with a `(?)` link to documentation. The tooltip of the value widget is copied onto the label. **Rows appear in the `AdvSettingsRows` enum order**, which is the order below. Some rows only exist on certain builds or platforms:

* **[lt2]**: libtorrent 2.x builds only.
* **[lt1]**: libtorrent 1.2 builds only.
* **[Win]**: Windows only.
* **[!mac]**: hidden on macOS.
* **[mac/Win]**: macOS and Windows only.
* **[DBus]**: Linux builds with D-Bus only.

For the SwiftUI app (macOS, libtorrent 2.x equivalent), the expected set of rows excludes **[lt1]**, **[Win]**, **[!mac]** and **[DBus]**.

### 9.1 "qBittorrent Section"

The section header row reads `"qBittorrent Section"` and links `"Open documentation"` to `https://github.com/qbittorrent/qBittorrent/wiki/Explanation-of-Options-in-qBittorrent#Advanced`.

| Row | GUI label (exact) | Widget / range | JSON key (#) | Type | Default | Values · semantics · ini key |
|---|---|---|---|---|---|---|
| 1 | `"Resume data storage type (requires restart)"` | combo | `resume_data_storage_type` (156) | string enum | `"Legacy"` | `"Legacy"` = `"Fastresume files"`, `"SQLite"` = `"SQLite database (experimental)"`. Unparsable string → Legacy. Takes effect on restart. · `BitTorrent/Session/ResumeDataStorageType` |
| 2 | `"Torrent content removing mode"` | combo | `torrent_content_remove_option` (157) | string enum | `"Delete"` | `"Delete"` = `"Delete files permanently"`, `"MoveToTrash"` = `"Move files to trash (if possible)"`. Unparsable string in the **API SET → MoveToTrash**. · `BitTorrent/Session/TorrentContentRemoveOption` |
| 3 [lt2][Win] | `"Physical memory (RAM) usage limit"` + `(?)` → `https://wikipedia.org/wiki/Working_set` | spin 1…INT_MAX, suffix `" MiB"` | `memory_working_set_limit` (158) | int MiB | `512` | Working-set limit; only applied on Windows. Always present in the API. · `Application/MemoryWorkingSetLimit` |
| 4 [Win] | `"Process memory priority"` + `(?)` | combo `"Normal"`, `"Below normal"`, `"Medium"`, `"Low"`, `"Very low"` | — (GUI only) | enum `MemoryPriority` | `BelowNormal` (1) | `Normal=0`, `BelowNormal=1`, `Medium=2`, `Low=3`, `VeryLow=4`. · `Application/ProcessMemoryPriority` |
| 5 | `"Network interface"` | combo: `"Any interface"` (data `""`), then each `QNetworkInterface` (shows `humanReadableName`, data `name`). If the saved interface is missing, it is appended using the saved name | `current_network_interface` (159); `current_interface_name` (160, GET only) | string | `""` | Binds libtorrent to this interface. The API SET looks up the interface by `name()`; if found (and it has addresses) it also stores its human-readable name, and an empty value clears the name. · `BitTorrent/Session/Interface`, `BitTorrent/Session/InterfaceName` |
| 6 | `"Optional IP address to bind to"` | combo: `"All addresses"` (`""`), `"All IPv4 addresses"` (`"0.0.0.0"`), `"All IPv6 addresses"` (`"::"`), then every address of the selected interface (or of all interfaces). IPv6 is shown canonically. An unknown saved value is appended | `current_interface_address` (161) | string | `""` | Validated with `QHostAddress`; invalid → `""`. · `BitTorrent/Session/InterfaceAddress` |
| 7 | `"Save resume data interval [0: disabled]"` | spin 0…, suffix `" min"`, special `"0 (disabled)"` | `save_resume_data_interval` (162) | int min | `60` | Periodic save of fastresume data for changed torrents. · `BitTorrent/Session/SaveResumeDataInterval` |
| 8 | `"Save statistics interval [0: disabled]"` | spin 0…, `" min"`, `"0 (disabled)"` | `save_statistics_interval` (163) | int min | `15` | Periodic save of all-time upload/download totals. · `BitTorrent/Session/SaveStatisticsInterval` |
| 9 | `".torrent file size limit"` | spin 1…(INT_MAX/1 MiB), `" MiB"` | `torrent_file_size_limit` (164) | int64 **bytes** | `104857600` (100 MiB) | Largest `.torrent` file that will be loaded or downloaded. · `BitTorrent/TorrentFileSizeLimit` |
| 10 | `"Confirm torrent recheck"` | checkbox | `confirm_torrent_recheck` (165) | bool | `true` | Asks before a forced recheck. · `Preferences/Advanced/confirmTorrentRecheck` |
| 11 | `"Recheck torrents on completion"` | checkbox | `recheck_completed_torrents` (166) | bool | `false` | Re-hashes a torrent once it finishes. · `Preferences/Advanced/RecheckOnCompletion` |
| 12 | `"Customize application instance name"` (tooltip `"It appends the text to the window title to help distinguish qBittorrent instances"`) | line edit | `app_instance_name` (167) | string | `""` | Title suffix (the window title becomes e.g. "qBittorrent v5.2.4 - <name>"). · `Application/InstanceName` |
| 13 | `"Refresh interval"` (tooltip `"It controls the internal state update interval which in turn will affect UI updates"`) | spin 30–99999, `" ms"` | `refresh_interval` (168) | int ms | `1500` | Session stats and torrent status polling period, which drives UI refresh. · `BitTorrent/Session/RefreshInterval` |
| 14 | `"Resolve peer host names"` | checkbox | `resolve_peer_host_names` (169) | bool | `false` | Reverse-DNS on peer IPs in the Peers tab. · `Preferences/Connection/ResolvePeerHostNames` |
| 15 | `"Resolve peer countries"` | checkbox | `resolve_peer_countries` (170) | bool | `true` | GeoIP country flags for peers; turns the DB-IP database download on. · `Preferences/Connection/ResolvePeerCountries` |
| 16 | `"Display notifications"` | checkbox | — (GUI only) | bool | `true` | Desktop notifications, e.g. download finished or I/O error. · `GUI/Notifications/Enabled` |
| 17 | `"Display notifications for added torrents"` | checkbox | — (GUI only) | bool | `false` | A notification each time a torrent is added. · `Application/GUI/Notifications/TorrentAdded` |
| 18 [DBus] | `"Notification timeout [0: infinite, -1: system default]"` | spin −1…, suffix `" (infinite)"` / `" (system default)"` / `" ms"` | — | int ms | `-1` | · `GUI/Notifications/Timeout` |
| 19 | `"Confirm removal of all tags"` | checkbox | — (GUI only) | bool | `true` | Asks before "Remove All" tags. · `Preferences/Advanced/confirmRemoveAllTags` |
| 20 | `"Confirm removal of tracker from all torrents"` | checkbox | — (GUI only) | bool | `true` | Asks before removing a tracker from the Trackers filter context menu. · `GUI/ConfirmActions/RemoveTrackerFromAllTorrents` |
| 21 | `"Reannounce to all trackers when IP or port changed"` | checkbox | `reannounce_when_address_changed` (171) | bool | `false` | · `BitTorrent/Session/ReannounceWhenAddressChanged` |
| 22 | `"Download tracker's favicon"` | checkbox | — (GUI only) | bool | `false` | The Trackers sidebar filter downloads `http(s)://<tracker host>/favicon.ico` for each tracker host. · `GUI/DownloadTrackerFavicon` |
| 23 | `"Save path history length"` | spin 0–99 | — (GUI only) | int | `8` (clamped 0–99) | Number of entries in the Add-dialog save-path and download-path history combos. · `AddNewTorrentDialog/SavePathHistoryLength` |
| 24 | `"Enable speed graphs"` | checkbox | — (GUI only) | bool | `true` | Shows the Speed tab in the properties panel. · `SpeedWidget/Enabled` |
| 25 [!mac] | `"Enable icons in menus"` | checkbox | — | bool | `true` | · `Preferences/Advanced/EnableIconsInMenus` |
| 26 [!mac] | `"Attach "Add new torrent" dialog to main window"` | checkbox | — | bool | `false` | Makes the Add dialog a child (sheet) of the main window. · `AddNewTorrentDialog/Attached` |
| 27 | `"Enable embedded tracker"` | checkbox | `enable_embedded_tracker` (172) | bool | `false` | Built-in HTTP tracker. · `BitTorrent/TrackerEnabled` |
| 28 | `"Embedded tracker port"` | spin 1–65535 | `embedded_tracker_port` (173) | int | `9000` | · `Preferences/Advanced/trackerPort` |
| 29 | `"Enable port forwarding for embedded tracker"` | checkbox | `embedded_tracker_port_forwarding` (174) | bool | `false` | UPnP mapping for the tracker port. · `Preferences/Advanced/trackerPortForwarding` |
| 30 [mac/Win] | macOS: `"Enable quarantine for downloaded files"`; Windows: `"Enable Mark-of-the-Web (MOTW) for downloaded files"` | checkbox | `mark_of_the_web` (175) | bool | `true` | macOS: sets the `com.apple.quarantine` xattr on downloaded content (and on files fetched by URL). · `Preferences/Advanced/markOfTheWeb` |
| 31 | `"Ignore SSL errors"` (tooltip `"Affects certificate validation and non-torrent protocol activities (e.g. RSS feeds, program updates, torrent files, geoip db, etc)"`) | checkbox | `ignore_ssl_errors` (176) | bool | `false` | For non-torrent HTTP downloads (`Net::DownloadManager`). · `Preferences/Advanced/IgnoreSSLErrors` |
| 32 | `"Python executable path (may require restart)"` | line edit, placeholder `"(Auto detect if empty)"` | `python_executable_path` (177) | string path | `""` | Python used for search plugins (trimmed). · `Preferences/Search/pythonExecutablePath` |
| 33 | `"Start BitTorrent session in paused state"` | checkbox | — (GUI only) | bool | `false` | The libtorrent session starts paused: nothing transfers until it is resumed. · `BitTorrent/Session/StartPaused` |
| 34 | `"BitTorrent session shutdown timeout [-1: unlimited]"` (tooltip: `Sets the timeout for the session to be shut down gracefully, at which point it will be forcibly terminated.<br>Note that this does not apply to the saving resume data time.`) | spin −1…, `" sec"`, special `"-1 (unlimited)"` | — (GUI only) | int s | `-1` | · `BitTorrent/Session/ShutdownTimeout` |

### 9.2 "libtorrent Section"

The section header row reads `"libtorrent Section"` and links `"Open documentation"` to `https://www.libtorrent.org/reference-Settings.html`. Every row label ends with a `(?)` link to the matching libtorrent setting anchor, given in brackets.

| Row | GUI label (exact) | Widget / range | JSON key (#) | Type | Default | Values · semantics · ini key |
|---|---|---|---|---|---|---|
| 35 | `"Bdecode depth limit"` [bdecode()] | spin 0…INT_MAX | `bdecode_depth_limit` (178) | int | `100` | Maximum nesting depth when bdecoding `.torrent` and resume files. · `BitTorrent/BdecodeDepthLimit` |
| 36 | `"Bdecode token limit"` | spin 0…INT_MAX | `bdecode_token_limit` (179) | int | `10000000` | · `BitTorrent/BdecodeTokenLimit` |
| 37 | `"Asynchronous I/O threads"` [aio_threads] | spin 1–1024 | `async_io_threads` (180) | int | `10` | · `BitTorrent/Session/AsyncIOThreadsCount` |
| 38 [lt2] | `"Hashing threads"` [hashing_threads] | spin 1–1024 | `hashing_threads` (181) | int | `1` | · `BitTorrent/Session/HashingThreadsCount` |
| 39 | `"File pool size"` [file_pool_size] | spin 1…INT_MAX | `file_pool_size` (182) | int | `100` | Maximum number of open file handles. · `BitTorrent/Session/FilePoolSize` |
| 40 | `"Outstanding memory when checking torrents"` [checking_mem_usage] | spin 1–1024 (128 on 32-bit), `" MiB"` | `checking_memory_use` (183) | int MiB | `32` | · `BitTorrent/Session/CheckingMemUsageSize` |
| 41 [lt1] | `"Disk cache"` [cache_size] | spin −1…33554431, suffix `" (auto)"` for <0, `" (disabled)"` for 0, `" MiB"` otherwise | `disk_cache` (184) | int MiB | `-1` | Clamped to 33554431 (1536 on 32-bit). Present in the API even on lt2 (no effect there). · `BitTorrent/Session/DiskCacheSize` |
| 42 [lt1] | `"Disk cache expiry interval"` [cache_expiry] | spin 1…, `" s"` | `disk_cache_ttl` (185) | int s | `60` | · `BitTorrent/Session/DiskCacheTTL` |
| 43 | `"Disk queue size"` [max_queued_disk_bytes] | spin 1…, `" KiB"` | `disk_queue_size` (186) | int64 **bytes** | `1048576` (1 MiB) | The GUI shows the value / 1024. · `BitTorrent/Session/DiskQueueSize` |
| 44 [lt2] | `"Disk IO type (requires restart)"` [default-disk-io-constructor] | combo `"Default"`, `"Memory mapped files"`, `"POSIX-compliant"`, `"Simple pread/pwrite"` | `disk_io_type` (187) | int enum | `0` | `Default=0`, `MMap=1`, `Posix=2`, `SimplePreadPwrite=3`. · `BitTorrent/Session/DiskIOType` |
| 45 | `"Disk IO read mode"` [disk_io_read_mode] | combo `"Disable OS cache"`, `"Enable OS cache"` | `disk_io_read_mode` (188) | int enum | `1` | `DisableOSCache=0`, `EnableOSCache=1`. · `BitTorrent/Session/DiskIOReadMode` |
| 46 | `"Disk IO write mode"` [disk_io_write_mode] | combo `"Disable OS cache"`, `"Enable OS cache"`, `"Write-through"` (lt2) | `disk_io_write_mode` (189) | int enum | `1` | `DisableOSCache=0`, `EnableOSCache=1`, `WriteThrough=2` (lt2). · `BitTorrent/Session/DiskIOWriteMode` |
| 47 [lt1] | `"Coalesce reads & writes"` [coalesce_reads] | checkbox | `enable_coalesce_read_write` (190) | bool | `true` on Windows, else `false` | · `BitTorrent/Session/CoalesceReadWrite` |
| 48 | `"Use piece extent affinity"` [piece_extent_affinity] | checkbox | `enable_piece_extent_affinity` (191) | bool | `false` | · `BitTorrent/Session/PieceExtentAffinity` |
| 49 | `"Send upload piece suggestions"` [suggest_mode] | checkbox | `enable_upload_suggestions` (192) | bool | `false` | · `BitTorrent/Session/SuggestMode` |
| 50 | `"Send buffer watermark"` [send_buffer_watermark] | spin 1…, `" KiB"` | `send_buffer_watermark` (193) | int KiB | `500` | · `BitTorrent/Session/SendBufferWatermark` |
| 51 | `"Send buffer low watermark"` | spin 1…, `" KiB"` | `send_buffer_low_watermark` (194) | int KiB | `10` | · `BitTorrent/Session/SendBufferLowWatermark` |
| 52 | `"Send buffer watermark factor"` | spin 1…, `" %"` | `send_buffer_watermark_factor` (195) | int % | `50` | · `BitTorrent/Session/SendBufferWatermarkFactor` |
| 53 | `"Outgoing connections per second"` [connection_speed] | spin 0… | `connection_speed` (196) | int | `30` | · `BitTorrent/Session/ConnectionSpeed` |
| 54 | `"Socket send buffer size [0: system default]"` [send_socket_buffer_size] | spin 0…, `" KiB"`, special `"0 (system default)"` | `socket_send_buffer_size` (197) | int **bytes** | `0` | GUI shows /1024. · `BitTorrent/Session/SocketSendBufferSize` |
| 55 | `"Socket receive buffer size [0: system default]"` [recv_socket_buffer_size] | same | `socket_receive_buffer_size` (198) | int bytes | `0` | · `BitTorrent/Session/SocketReceiveBufferSize` |
| 56 | `"Socket backlog size"` [listen_queue_size] | spin 1… | `socket_backlog_size` (199) | int | `30` | · `BitTorrent/Session/SocketBacklogSize` |
| 57 | `"Outgoing ports (Min) [0: disabled]"` [outgoing_port] | spin 0–65535, `"0 (disabled)"` | `outgoing_ports_min` (200) | int | `0` | Source-port range for outgoing connections. · `BitTorrent/Session/OutgoingPortsMin` |
| 58 | `"Outgoing ports (Max) [0: disabled]"` | spin 0–65535 | `outgoing_ports_max` (201) | int | `0` | · `BitTorrent/Session/OutgoingPortsMax` |
| 59 | `"UPnP lease duration [0: permanent lease]"` [upnp_lease_duration] | spin 0…, `" s"`, `"0 (permanent lease)"` | `upnp_lease_duration` (202) | int s | `0` | · `BitTorrent/Session/UPnPLeaseDuration` |
| 60 | `"Differentiated Services Code Point (DSCP) for connections to peers"` [peer_dscp] | spin 0–255 | `peer_tos` (203) | int | `1` (0x01) | · `BitTorrent/Session/PeerToS` |
| 61 | `"μTP-TCP mixed mode algorithm"` (`"%1-TCP mixed mode algorithm"` with %1 = μTP) [mixed_mode_algorithm] | combo `"Prefer TCP"`, `"Peer proportional (throttles TCP)"` | `utp_tcp_mixed_mode` (204) | int enum | `0` | `TCP=0`, `Proportional=1` (clamped). · `BitTorrent/Session/uTPMixedMode` |
| 62 | `"Internal hostname resolver cache expiry interval"` [resolver_cache_timeout] | spin 0…, `" s"` | `hostname_cache_ttl` (205) | int s | `1200` | · `BitTorrent/Session/HostnameCacheTTL` |
| 63 | `"Support internationalized domain name (IDN)"` [allow_idna] | checkbox | `idn_support_enabled` (206) | bool | `false` | · `BitTorrent/Session/IDNSupportEnabled` |
| 64 | `"Allow multiple connections from the same IP address"` [allow_multiple_connections_per_ip] | checkbox | `enable_multi_connections_from_same_ip` (207) | bool | `false` | · `BitTorrent/Session/MultiConnectionsPerIp` |
| 65 | `"Validate HTTPS tracker certificates"` [validate_https_trackers] | checkbox | `validate_https_tracker_certificate` (208) | bool | `true` | · `BitTorrent/Session/ValidateHTTPSTrackerCertificate` |
| 66 | `"Server-side request forgery (SSRF) mitigation"` [ssrf_mitigation] | checkbox | `ssrf_mitigation` (209) | bool | `true` | · `BitTorrent/Session/SSRFMitigation` |
| 67 | `"Disallow connection to peers on privileged ports"` [no_connect_privileged_ports] | checkbox | `block_peers_on_privileged_ports` (210) | bool | `false` | · `BitTorrent/Session/BlockPeersOnPrivilegedPorts` |
| 68 | `"Upload slots behavior"` [choking_algorithm] | combo `"Fixed slots"`, `"Upload rate based"` | `upload_slots_behavior` (211) | int enum | `0` | `FixedSlots=0`, `RateBased=1` (clamped). · `BitTorrent/Session/ChokingAlgorithm` |
| 69 | `"Upload choking algorithm"` [seed_choking_algorithm] | combo `"Round-robin"`, `"Fastest upload"`, `"Anti-leech"` | `upload_choking_algorithm` (212) | int enum | `1` | `RoundRobin=0`, `FastestUpload=1`, `AntiLeech=2` (clamped). · `BitTorrent/Session/SeedChokingAlgorithm` |
| 70 | `"Always announce to all trackers in a tier"` [announce_to_all_trackers] | checkbox | `announce_to_all_trackers` (213) | bool | `false` | · `BitTorrent/Session/AnnounceToAllTrackers` |
| 71 | `"Always announce to all tiers"` [announce_to_all_tiers] | checkbox | `announce_to_all_tiers` (214) | bool | `true` | · `BitTorrent/Session/AnnounceToAllTiers` |
| 72 | `"IP address reported to trackers (requires restart)"` [announce_ip] | line edit | `announce_ip` (215) | string | `""` | Parsed with `QHostAddress` (trimmed); invalid → `""`. · `BitTorrent/Session/AnnounceIP` |
| 73 | `"Port reported to trackers (requires restart) [0: listening port]"` [announce_port] | spin 0–65535 | `announce_port` (216) | int | `0` | · `BitTorrent/Session/AnnouncePort` |
| 74 | `"Max concurrent HTTP announces"` [max_concurrent_http_announces] | spin 0… | `max_concurrent_http_announces` (217) | int | `50` | · `BitTorrent/Session/MaxConcurrentHTTPAnnounces` |
| 75 | `"Stop tracker timeout [0: disabled]"` [stop_tracker_timeout] | spin 0…, `" s"`, `"0 (disabled)"` | `stop_tracker_timeout` (218) | int s | `2` | · `BitTorrent/Session/StopTrackerTimeout` |
| 76 | `"Peer turnover disconnect percentage"` [peer_turnover] | spin 0–100, `" %"` | `peer_turnover` (219) | int % | `4` | · `BitTorrent/Session/PeerTurnover` |
| 77 | `"Peer turnover threshold percentage"` | spin 0–100, `" %"` | `peer_turnover_cutoff` (220) | int % | `90` | · `BitTorrent/Session/PeerTurnoverCutOff` |
| 78 | `"Peer turnover disconnect interval"` | spin 30–3600, `" s"` | `peer_turnover_interval` (221) | int s | `300` | · `BitTorrent/Session/PeerTurnoverInterval` |
| 79 | `"Maximum outstanding requests to a single peer"` [max_out_request_queue] | spin 1… | `request_queue_size` (222) | int | `500` | · `BitTorrent/Session/RequestQueueSize` |
| 80 | `"DHT bootstrap nodes"` [dht_bootstrap_nodes] | line edit, placeholder `"Resets to default if empty"` | `dht_bootstrap_nodes` (223) | string | `"dht.libtorrent.org:25401, dht.transmissionbt.com:6881, router.bittorrent.com:6881"` | Comma-separated `host:port` entries. An empty value resets to the default. · `BitTorrent/Session/DHTBootstrapNodes` |
| 81 [lt2+I2P] | `"I2P inbound quantity"` [i2p_inbound_quantity] | spin 1–16 | `i2p_inbound_quantity` (60) | int | `3` | · `BitTorrent/Session/I2P/InboundQuantity` |
| 82 [lt2+I2P] | `"I2P outbound quantity"` | spin 1–16 | `i2p_outbound_quantity` (61) | int | `3` | · `BitTorrent/Session/I2P/OutboundQuantity` |
| 83 [lt2+I2P] | `"I2P inbound length"` | spin 0–7 | `i2p_inbound_length` (62) | int | `3` | · `BitTorrent/Session/I2P/InboundLength` |
| 84 [lt2+I2P] | `"I2P outbound length"` | spin 0–7 | `i2p_outbound_length` (63) | int | `3` | · `BitTorrent/Session/I2P/OutboundLength` |

---

## 10. WebAPI-only keys (no widget in the desktop Options dialog)

| JSON key | Direction | Notes |
|---|---|---|
| `ssl_enabled` (48), `ssl_listen_port` (49) | GET/SET | SSL-torrent listener (`BitTorrent/Session/SSL/Enabled`, default `false`; `.../SSL/Port`, random 1024–65535 at first start, never equal to `listen_port`). Neither the desktop GUI nor the bundled WebUI has a widget for them. |
| `random_port` (50) | GET/SET | Deprecated alias: GET returns `listen_port == 0`; SET `true` sets port 0. |
| `delete_torrent_content_files` (12) | GET/SET | GUI uses it only as the deletion dialog's "Remember choice" (§1.1). |
| `scan_dirs` (32) | GET/SET | Deprecated watched-folder view (§2.1). |
| `web_ui_password` | SET only | §8.1. |
| `web_ui_api_key` (133) | GET only | §8.1. Changed only through `app/rotateAPIKey` / `app/deleteAPIKey` or the GUI buttons. |
| `current_interface_name` (160) | GET only | Human-readable interface name (§9 row 5). |
| `add_trackers_url_list` (118) | GET only | Fetched tracker list (§5.1). |
| `memory_working_set_limit` (158), `disk_cache` (184), `disk_cache_ttl` (185), `enable_coalesce_read_write` (190), `hashing_threads` (181), `disk_io_type` (187), `i2p_*` (56–63) | GET/SET | Always present in the API whatever the build. Their GUI rows are build- or platform-specific (§9 tags), and setting them has no effect on builds where they don't apply. |

---

## 11. GUI-only preferences and persisted state outside the Options dialog

These are not in the WebAPI. A native client still has to implement them for parity.

### 11.1 Main window: View / Tools menus, toolbar, lock (`src/gui/mainwindow.cpp`)

| Feature / menu item (see features.md §1) | ini key | Type | Default | Semantics |
|---|---|---|---|---|
| View → "Top Toolbar" | `Preferences/General/ToolbarDisplayed` | bool | `true` | Toolbar visibility. |
| View → "Status Bar" | `Preferences/General/StatusbarDisplayed` | bool | `true` | |
| View → "Filters Sidebar" | `GUI/MainWindow/FiltersSidebarVisible` | bool | `true` | |
| (sidebar width) | `GUI/MainWindow/FiltersSidebarWidth` | int px | `120` | Saved from the splitter. |
| View → "Speed in Title Bar" | `Preferences/General/SpeedInTitleBar` | bool | `false` | Title becomes `"[D: %1, U: %2] %3"`-style (see features.md §1). |
| View → "RSS Reader" | `GUI/RSSWidget/Enabled` | bool | `false` | Shows the RSS tab. |
| View → "Search Engine" | `Preferences/Search/SearchEnabled` | bool | `false` | Shows the Search tab. Requires Python; turning it on without Python triggers the Python install prompt (Windows) or a warning. |
| View → Log → "Show" | `GUI/Log/Enabled` | bool | `false` | Shows the Execution Log tab. |
| View → Log → "Normal Messages" / "Information Messages" / "Warning Messages" / "Critical Messages" | `GUI/Log/Types` | flags `Log::MsgTypes` | `ALL` (= −1, all bits; NORMAL=0x1, INFO=0x2, WARNING=0x4, CRITICAL=0x8) | Message-type filter for the execution log view. |
| Toolbar context menu: "Icons Only" / "Text Only" / "Text Alongside Icons" / "Text Under Icons" / "Follow System Style" | `Toolbar/textPosition` | int `Qt::ToolButtonStyle` | `-1` (unset → follow style) | `IconOnly=0`, `TextOnly=1`, `TextBesideIcon=2`, `TextUnderIcon=3`, `FollowStyle=4`. |
| Tools → "On Downloads Done" → "Exit qBittorrent" | `Preferences/Downloads/AutoShutDownqBTOnCompletion` | bool | `false` | Mutually exclusive group with the next four plus "Do nothing". |
| → "Suspend System" | `Preferences/Downloads/AutoSuspendOnCompletion` | bool | `false` | |
| → "Hibernate System" | `Preferences/Downloads/AutoHibernateOnCompletion` | bool | `false` | |
| → "Shutdown System" | `Preferences/Downloads/AutoShutDownOnCompletion` | bool | `false` | |
| → "Reboot System" | `Preferences/Downloads/AutoRebootOnCompletion` | bool | `false` | |
| View → "Lock qBittorrent" (submenu "Set Password" / "Clear Password") | `Locking/password_PBKDF2` (QByteArray PBKDF2), `Locking/locked` | bytes / bool | empty / `false` | UI lock. While locked, restoring the window asks for the password. The lock state persists across restarts. |
| Alternative speed limits toggle (toolbar/status bar) | `BitTorrent/Session/UseAlternativeGlobalSpeedLimit` | bool | `false` | Runtime mode (WebAPI: `transfer/toggleSpeedLimitsMode`). |
| Transfer list filter box: "Use regular expressions" | `TransferList/UseRegexAsFilteringPattern` | bool | `false` | |
| Selected status filter | `TransferListFilters/selectedFilterIndex` | int | `0` | Restored at startup. |
| Sidebar section expanded states | `TransferListFilters/statusFilterState`, `CategoryFilterState`, `TagFilterState`, `trackerFilterState`, `TrackerStatusFilterState` | bool | `true` each | Collapsible sidebar sections. |
| Transfer list header state | `GUI/Qt6/TransferList/HeaderState` | bytes | — | Column order, width, visibility and sort. |
| Main window geometry | `MainWindow/geometry` | bytes | — | |
| Last "Open torrent" directory | `MainWindow/LastDir` | path | home dir | |
| Minimize/close-to-tray one-time notices | `Preferences/General/MinimizeToTrayNotified`, `CloseToTrayNotified` | bool | `false` | Shows the "qBittorrent is minimized/closed to tray" balloon once. |
| Exit confirmation "Always Yes" button | sets `Preferences/General/ExitConfirm=false` | | | |
| Legal notice accepted | `LegalNotice/Accepted` | bool | `false` | First-run legal notice (`src/app/main.cpp`). |
| Window state on start (also written at exit) | `GUI/StartUpWindowState` | enum | `Normal` | |

#### 11.1.1 qBit NeoRS extensions (not in qBittorrent)

| Feature | key | Type | Default | Semantics |
|---|---|---|---|---|
| View → "as List" / "as Cards" | `GUI/TransferList/ViewMode` | int | `0` | 0 = table, 1 = cards (`docs/design/torrent-names.md`). |
| Interface › Torrent Names: "Parse technical info from names" | `GUI/TorrentNames/Enabled` | bool | `true` | Titles and tags from the name rules. |
| "List: show titles in semibold" | `GUI/TorrentNames/SemiboldTitles` | bool | `true` | |
| "Cards: show technical info as tags" | `GUI/TorrentNames/CardTags` | bool | `true` | |
| "Show the year as a tag" | `GUI/TorrentNames/YearAsTag` | bool | `true` | Off: the year is appended to the title. |
| Name rules | `GUI/TorrentNames/Rules` | data (JSON `[NameRule]`) | absent | Absent = built-in rules. |

### 11.2 Properties panel / speed graph

| Item | ini key | Default |
|---|---|---|
| Properties panel visible / current tab / splitter | `TorrentProperties/Visible` (`false`), `TorrentProperties/CurrentTab` (`-1`), `TorrentProperties/SplitterSizes` | |
| Peer / Files / Tracker list header states | `GUI/Qt6/TorrentProperties/PeerListState`, `.../FilesListState`, `.../TrackerListState` | |
| Content filter pattern format | `GUI/PropertiesWidget/FilterPatternFormat` | `Wildcards` (enum `FilterPatternFormat`: PlainText, Wildcards, Regex) |
| Speed graph period | `SpeedWidget/period` | `1` (index into the period list, see features.md §2) |
| Speed graph series enable | `SpeedWidget/graph_enable_<id>` | `true` for id 0 (UP) and 1 (DOWN), else `false` |

### 11.3 Add New Torrent dialog

| Item | ini key | Default |
|---|---|---|
| "Remember last used save path" | `AddNewTorrentDialog/RememberLastSavePath` | `false` |
| Last selected category (pre-selected next time) | `AddNewTorrentDialog/DefaultCategory` | `""` |
| Save-path history (MRU) | `AddNewTorrentDialog/SavePathHistory` | `[]`, max length = `SavePathHistoryLength` (8) |
| Download-path history | `AddNewTorrentDialog/DownloadPathHistory` | `[]` |
| Dialog size / tree header / splitter | `AddNewTorrentDialog/DialogSize`, `GUI/Qt6/AddNewTorrentDialog/TreeHeaderState`, `GUI/Qt6/AddNewTorrentDialog/SplitterState` | |
| File filter pattern format | `GUI/AddNewTorrentDialog/FilterPatternFormat` | `Wildcards` |
| Last torrent-import content dir / geometry | `TorrentImport/LastContentDir` (home), `TorrentImportDlg/dimensions` | |

### 11.4 Torrent Creator dialog (`src/gui/torrentcreatordialog.cpp`; full field list in features.md §3)

| Item | ini key | Default |
|---|---|---|
| Piece size (combo index / bytes) | `TorrentCreator/PieceSize` | `0` (Auto) |
| "Private torrent" | `TorrentCreator/PrivateTorrent` | `false` |
| "Start seeding immediately" | `TorrentCreator/StartSeeding` | `false` |
| "Ignore share ratio limits for this torrent" | `TorrentCreator/IgnoreRatio` | `false` |
| Torrent format (lt2) | `TorrentCreator/TorrentFormat` | `1` (index: V2=0, **Hybrid=1**, V1=2) |
| "Optimize alignment" (lt1) | `TorrentCreator/OptimizeAlignment` | `true` |
| Last input path / last save path | `TorrentCreator/LastAddPath`, `TorrentCreator/LastSavePath` | home dir |
| Trackers / web seeds / comment / source text | `TorrentCreator/TrackerList`, `TorrentCreator/WebSeedList`, `TorrentCreator/Comments`, `TorrentCreator/Source` | `""` |
| Background creation tasks (WebAPI `torrentcreator/*`) | `TorrentCreator/Manager/MaxTasks`, `TorrentCreator/Manager/NumThreads` | `256`, `1` |

### 11.5 RSS / Search / misc dialogs

| Item | ini key | Default |
|---|---|---|
| RSS tab splitters / opened folders | `GUI/Qt6/RSSWidget/SideSplitterState`, `GUI/Qt6/RSSWidget/MainSplitterState`, `GUI/RSSWidget/OpenedFolders` | |
| RSS Downloader dialog geometry / splitters | `RssFeedDownloader/geometrySize`, `GUI/Qt6/RSSFeedDownloader/HSplitterSizes`, `GUI/Qt6/RSSFeedDownloader/RuleDefSplitterState` | |
| Disabled search engines | `SearchEngines/disabledEngines` | `[]` |
| Search results header / regex filter / name filtering mode | `GUI/Qt6/SearchTab/HeaderState`, `SearchTab/UseRegexAsFilteringPattern` (`false`), `Search/FilteringMode` | |
| Network cookies (Tools → Manage Cookies; WebAPI `app/cookies`, `app/setCookies`) | `Network/Cookies` (raw cookie strings) | `[]` |
| Add trackers dialog: "Download trackers list" URL | `AddTrackersDialog/TrackersListURL` | `""` |
| Execution-log / dialog sizes | `AboutDialog/Size`, `StatisticsDialog/Size`, `CookiesDialog/Size`, `GUI/Qt6/CookiesDialog/ViewState`, `SpeedLimitDialog/Size`, `TorrentOptionsDialog/Size`, `TrackerEntriesDialog/Size`, `DownloadFromURLDialog/Size`, `BanListOptionsDialog/Size`, `IPSubnetWhitelistOptionsDialog/Size`, `WatchedFolderOptionsDialog/DialogSize`, `PreviewSelectDialog/Size` + `GUI/Qt6/PreviewSelectDialog/HeaderState`, `GUI/TorrentTagsDialog/Size`, `GUI/UIThemeDialog/Size`, `SearchPluginSelectDialog/Size`, `SearchPluginSourceDialog/Size`, `AddTrackersDialog/Size` | |
| Dynamic DNS last update state | `DNSUpdater/lastUpdateTime`, `DNSUpdater/lastIP` | |
| Tags list (state) | `BitTorrent/Session/Tags` | `[]` |
| Settings migration version | `Meta/MigrationVersion` (`MIGRATION_VERSION_KEY` in `src/app/upgrade.cpp`) | |

---

## 12. Enum reference (all integer / string values)

| Enum | Values | Used by |
|---|---|---|
| `BTProtocol` | `Both=0`, `TCP=1`, `UTP=2` | `bittorrent_protocol` |
| `ChokingAlgorithm` | `FixedSlots=0`, `RateBased=1` | `upload_slots_behavior` |
| `SeedChokingAlgorithm` | `RoundRobin=0`, `FastestUpload=1`, `AntiLeech=2` | `upload_choking_algorithm` |
| `MixedModeAlgorithm` | `TCP=0`, `Proportional=1` | `utp_tcp_mixed_mode` |
| `DiskIOType` | `Default=0`, `MMap=1`, `Posix=2`, `SimplePreadPwrite=3` | `disk_io_type` |
| `DiskIOReadMode` | `DisableOSCache=0`, `EnableOSCache=1` | `disk_io_read_mode` |
| `DiskIOWriteMode` | `DisableOSCache=0`, `EnableOSCache=1`, `WriteThrough=2` (lt2) | `disk_io_write_mode` |
| `ResumeDataStorageType` | `Legacy`, `SQLite` (serialized as **strings**) | `resume_data_storage_type` |
| `TorrentContentRemoveOption` | `Delete`, `MoveToTrash` (strings) | `torrent_content_remove_option` |
| `TorrentContentLayout` | `Original=0`, `Subfolder=1`, `NoSubfolder=2` (strings in the API) | `torrent_content_layout` |
| `Torrent::StopCondition` | `None=0`, `MetadataReceived=1`, `FilesChecked=2` (strings in the API) | `torrent_stop_condition` |
| `ShareLimitAction` | `Default=-1`, `Stop=0`, `Remove=1`, `EnableSuperSeeding=2`, `RemoveWithContent=3` | `max_ratio_act` (ints) |
| `Net::ProxyType` | `None=0`, `HTTP=1`, `SOCKS5=2`, `SOCKS4=5` (strings in the API) | `proxy_type` |
| `Scheduler::Days` | `EveryDay=0`, `Weekday=1`, `Weekend=2`, `Monday=3`, `Tuesday=4`, `Wednesday=5`, `Thursday=6`, `Friday=7`, `Saturday=8`, `Sunday=9` | `scheduler_days` |
| `DNS::Service` | `DynDNS=0`, `NoIP=1`, `None=-1` | `dyndns_service` |
| Encryption (plain int) | `0` allow/prefer, `1` require (forced), `2` disable | `encryption` |
| `TorrentFileGuard::AutoDeleteMode` | `Never=0`, `IfAdded=1`, `Always=2` (stored in the ini as the name) | `auto_delete_mode` |
| `FileLogger::FileLogAgeType` | `DAYS=0`, `MONTHS=1`, `YEARS=2` | `file_log_age_type` |
| `TrayIcon::Style` | `Normal=0`, `MonoDark=1`, `MonoLight=2` | GUI tray icon style |
| `ColorScheme` | `System=0`, `Light=1`, `Dark=2` | GUI color scheme |
| `WindowState` | `Normal=0`, `Minimized=1`, `Hidden=2` (not on macOS) | GUI start-up window state |
| `DoubleClickAction` | `TOGGLE_STOP=0`, `OPEN_DEST=1`, `PREVIEW_FILE=2`, `NO_ACTION=3`, `SHOW_OPTIONS=4` | GUI double-click actions |
| `MemoryPriority` (Windows) | `Normal=0`, `BelowNormal=1`, `Medium=2`, `Low=3`, `VeryLow=4` | GUI advanced row |
| `Log::MsgType` | `ALL=-1`, `NORMAL=0x1`, `INFO=0x2`, `WARNING=0x4`, `CRITICAL=0x8` | GUI log filter |
| Hide-zero mode | `0` Always, `1` Stopped torrents only | GUI |

---

## Appendix A — exact `app/preferences` GET key order (223 keys)

```
  1 locale                              2 performance_warning                 3 status_bar_external_ip
  4 confirm_torrent_deletion            5 file_log_enabled                    6 file_log_path
  7 file_log_backup_enabled             8 file_log_max_size                   9 file_log_delete_old
 10 file_log_age                       11 file_log_age_type                  12 delete_torrent_content_files
 13 torrent_content_layout             14 add_to_top_of_queue                15 add_stopped_enabled
 16 torrent_stop_condition             17 merge_trackers                     18 auto_delete_mode
 19 preallocate_all                    20 incomplete_files_ext               21 use_unwanted_folder
 22 auto_tmm_enabled                   23 torrent_changed_tmm_enabled        24 save_path_changed_tmm_enabled
 25 category_changed_tmm_enabled       26 save_path                          27 temp_path_enabled
 28 temp_path                          29 use_category_paths_in_manual_mode  30 export_dir
 31 export_dir_fin                     32 scan_dirs                          33 excluded_file_names_enabled
 34 excluded_file_names                35 mail_notification_enabled          36 mail_notification_sender
 37 mail_notification_email            38 mail_notification_smtp             39 mail_notification_ssl_enabled
 40 mail_notification_auth_enabled     41 mail_notification_username         42 mail_notification_password
 43 autorun_on_torrent_added_enabled   44 autorun_on_torrent_added_program   45 autorun_enabled
 46 autorun_program                    47 listen_port                        48 ssl_enabled
 49 ssl_listen_port                    50 random_port                        51 upnp
 52 max_connec                         53 max_connec_per_torrent             54 max_uploads
 55 max_uploads_per_torrent            56 i2p_enabled                        57 i2p_address
 58 i2p_port                           59 i2p_mixed_mode                     60 i2p_inbound_quantity
 61 i2p_outbound_quantity              62 i2p_inbound_length                 63 i2p_outbound_length
 64 proxy_type                         65 proxy_ip                           66 proxy_port
 67 proxy_auth_enabled                 68 proxy_username                     69 proxy_password
 70 proxy_hostname_lookup              71 proxy_bittorrent                   72 proxy_peer_connections
 73 proxy_rss                          74 proxy_misc                         75 ip_filter_enabled
 76 ip_filter_path                     77 ip_filter_trackers                 78 banned_IPs
 79 dl_limit                           80 up_limit                           81 alt_dl_limit
 82 alt_up_limit                       83 bittorrent_protocol                84 limit_utp_rate
 85 limit_tcp_overhead                 86 limit_lan_peers                    87 scheduler_enabled
 88 schedule_from_hour                 89 schedule_from_min                  90 schedule_to_hour
 91 schedule_to_min                    92 scheduler_days                     93 dht
 94 pex                                95 lsd                                96 encryption
 97 anonymous_mode                     98 max_active_checking_torrents       99 queueing_enabled
100 max_active_downloads              101 max_active_torrents               102 max_active_uploads
103 dont_count_slow_torrents          104 slow_torrent_dl_rate_threshold    105 slow_torrent_ul_rate_threshold
106 slow_torrent_inactive_timer       107 max_ratio_enabled                 108 max_ratio
109 max_seeding_time_enabled          110 max_seeding_time                  111 max_inactive_seeding_time_enabled
112 max_inactive_seeding_time         113 max_ratio_act                     114 add_trackers_enabled
115 add_trackers                      116 add_trackers_from_url_enabled     117 add_trackers_url
118 add_trackers_url_list             119 web_ui_domain_list                120 web_ui_address
121 web_ui_port                       122 web_ui_upnp                       123 use_https
124 web_ui_https_cert_path            125 web_ui_https_key_path             126 web_ui_username
127 bypass_local_auth                 128 bypass_auth_subnet_whitelist_enabled 129 bypass_auth_subnet_whitelist
130 web_ui_max_auth_fail_count        131 web_ui_ban_duration               132 web_ui_session_timeout
133 web_ui_api_key                    134 alternative_webui_enabled         135 alternative_webui_path
136 web_ui_clickjacking_protection_enabled 137 web_ui_csrf_protection_enabled 138 web_ui_secure_cookie_enabled
139 web_ui_host_header_validation_enabled 140 web_ui_use_custom_http_headers_enabled 141 web_ui_custom_http_headers
142 web_ui_reverse_proxy_enabled      143 web_ui_reverse_proxies_list       144 dyndns_enabled
145 dyndns_service                    146 dyndns_username                   147 dyndns_password
148 dyndns_domain                     149 rss_refresh_interval              150 rss_fetch_delay
151 rss_max_articles_per_feed         152 rss_processing_enabled            153 rss_auto_downloading_enabled
154 rss_download_repack_proper_episodes 155 rss_smart_episode_filters       156 resume_data_storage_type
157 torrent_content_remove_option     158 memory_working_set_limit          159 current_network_interface
160 current_interface_name            161 current_interface_address         162 save_resume_data_interval
163 save_statistics_interval          164 torrent_file_size_limit           165 confirm_torrent_recheck
166 recheck_completed_torrents        167 app_instance_name                 168 refresh_interval
169 resolve_peer_host_names           170 resolve_peer_countries            171 reannounce_when_address_changed
172 enable_embedded_tracker           173 embedded_tracker_port             174 embedded_tracker_port_forwarding
175 mark_of_the_web                   176 ignore_ssl_errors                 177 python_executable_path
178 bdecode_depth_limit               179 bdecode_token_limit               180 async_io_threads
181 hashing_threads                   182 file_pool_size                    183 checking_memory_use
184 disk_cache                        185 disk_cache_ttl                    186 disk_queue_size
187 disk_io_type                      188 disk_io_read_mode                 189 disk_io_write_mode
190 enable_coalesce_read_write        191 enable_piece_extent_affinity      192 enable_upload_suggestions
193 send_buffer_watermark             194 send_buffer_low_watermark         195 send_buffer_watermark_factor
196 connection_speed                  197 socket_send_buffer_size           198 socket_receive_buffer_size
199 socket_backlog_size               200 outgoing_ports_min                201 outgoing_ports_max
202 upnp_lease_duration               203 peer_tos                          204 utp_tcp_mixed_mode
205 hostname_cache_ttl                206 idn_support_enabled               207 enable_multi_connections_from_same_ip
208 validate_https_tracker_certificate 209 ssrf_mitigation                  210 block_peers_on_privileged_ports
211 upload_slots_behavior             212 upload_choking_algorithm          213 announce_to_all_trackers
214 announce_to_all_tiers             215 announce_ip                       216 announce_port
217 max_concurrent_http_announces     218 stop_tracker_timeout              219 peer_turnover
220 peer_turnover_cutoff              221 peer_turnover_interval            222 request_queue_size
223 dht_bootstrap_nodes
```

## Appendix B — SET vs GET differences

* **Accepted by SET but never returned by GET:** `web_ui_password`.
* **Returned by GET but ignored by SET:** `add_trackers_url_list`, `web_ui_api_key`, `current_interface_name`.
* **SET processing order** (relevant because of partial application on error) mostly follows GET order, with these deviations:
  * `status_bar_external_ip` is processed before `performance_warning`.
  * `random_port`/`listen_port` are handled together, then `ssl_enabled`, `ssl_listen_port`, `upnp`.
  * Inside the embedded-tracker block the order is `embedded_tracker_port`, `embedded_tracker_port_forwarding`, then `enable_embedded_tracker`.
  * `web_ui_username` is validated before `web_ui_password`, which is processed next and before `bypass_local_auth`.
* **Derived GET keys:**
  * `max_ratio_enabled` = `max_ratio >= 0`
  * `max_seeding_time_enabled` = `max_seeding_time >= 0`
  * `max_inactive_seeding_time_enabled` = `max_inactive_seeding_time >= 0`
  * `random_port` = `listen_port == 0`
  * `bypass_local_auth` = `!LocalHostAuth`
  * `limit_lan_peers` = `!IgnoreLimitsOnLAN`
  * `auto_tmm_enabled` = `!DisableAutoTMMByDefault`
  * the three `*_tmm_enabled` triggers = `!DisableAutoTMMTriggers/*`
* **Side effects of SET:**
  * `locale` installs a translator immediately.
  * `add_trackers_from_url_enabled=true` triggers a download now and starts a 24 h timer; `false` deletes the cached file.
  * `add_trackers_url` re-fetches.
  * `save_resume_data_interval` restarts its timer (0 stops it).
  * `rss_refresh_interval` restarts the RSS timer.
  * `listen_port`, `ssl_*` and `current_network_interface`/`current_interface_address` reconfigure the listening interfaces.
  * `ip_filter_path` and `ip_filter_enabled` reload the filter.
