# qBittorrent 5.2.4 — Feature Parity Specification (desktop GUI + core + WebAPI)

This is the parity checklist for rewriting qBittorrent 5.2.4 (C++/Qt/libtorrent) as a Rust core with a native SwiftUI macOS app. Anything not listed here will be missing from the product. Every preference key, with its default, enum values and GUI label, lives in the companion file **`preferences.md`**. This file covers behaviour, UI structure, strings and wire formats.

## 0. Versions, sources, conventions

* **qBittorrent version:** `5.2.4`, from `src/base/version.h.in` (`QBT_VERSION_MAJOR 5`, `QBT_VERSION_MINOR 2`, `QBT_VERSION_BUGFIX 4`, `QBT_VERSION_BUILD 0`, `QBT_VERSION_STATUS ""`).
  * `QBT_VERSION` = `"v5.2.4"`; `QBT_VERSION_2` = `"5.2.4"`.
  * The HTTP user agent used for downloads is `"qBittorrent/5.2.4"`.
* **WebAPI version:** `API_VERSION {2, 15, 1}` in `src/webui/webapplication.h`. `GET /api/v2/app/webapiVersion` returns `2.15.1`.
* **Reference tree:** `scratchpad/ref/qbittorrent` (read-only). File paths below are relative to its `src/`.
* **Strings:** text in double quotes is the exact English source string passed to `tr()` or found in a `.ui` file. `&` mnemonics are shown where they matter. `%1`, `%2` are Qt argument placeholders.
* **Enums:** given with their C++ integer values. String forms used on the wire are the Qt meta-enum key names.
* **Platform notes:** **[macOS]**, **[Win]**, **[Linux]**, **[lt2]** (libtorrent 2.x builds) and **[lt1]** (libtorrent 1.2 builds) mark platform- or build-specific behaviour. The SwiftUI target is macOS on libtorrent 2.x semantics.

### Document map

| § | Topic |
|---|---|
| 1 | Main window: layout, toolbar, menus, status bar, filters sidebar, torrent list columns, torrent states, context menu, macOS integration |
| 2 | Properties panel: General, Trackers, Peers, HTTP Sources, Content, Speed tabs; preview dialog |
| 3 | Dialogs: add torrent, add links, import from qBittorrent/Transmission (§3.37), torrent creator, location/rename, speed limits, share limits, torrent options, trackers, statistics, log, about, cookies, banned IPs, subnet whitelist, watched folders, categories, tags, search plugins, RSS feed/rules, shutdown confirmation, UI theme, legal notice, and others |
| 4 | Core behaviours (engine): torrent lifecycle, queueing, share limits, scheduler, watched folders, ATM, file handling, external programs, email, auto-shutdown, magnets, trackers from URL, IP filter, proxy, encryption, port forwarding, GeoIP, favicons, persistence, logging, command line |
| 5 | Search engine (nova3 Python plugins) and RSS (feeds, parser, auto-downloader, UI) |
| 6 | WebAPI v2: transport, authentication, every endpoint with parameters and response fields, `sync/maindata` diff format |

Overlap note: the RSS Downloader dialog, the RSS reader tab and the Search tab are described twice. Dialog-centric descriptions are in §3.25, §3.35 and §3.36; engine-centric ones are in §5.8 and §5.12. The Preview dialog appears in both §2.9 and §3.27, and the Remove-torrent confirmation in both §1.9 and §3.28. Where the two disagree, treat the more detailed section as authoritative. Both were written from the same 5.2.4 source.


---

## 1. Main window

Sources: `src/gui/mainwindow.ui`, `src/gui/mainwindow.cpp`, `src/gui/statusbar.cpp`, `src/gui/transferlist*.cpp`, `src/gui/transferlistfilters/*`, `src/base/torrentfilter.cpp`, `src/base/bittorrent/torrentimpl.cpp`, `src/gui/deletionconfirmationdialog.*`, `src/gui/desktopintegration.cpp`, `src/gui/macos*`, `src/gui/macutilities.mm`, `src/gui/programupdater.cpp`, `src/app/application.cpp`, `src/app/legalnotice.cpp`.

Shortcut notation: Qt's `Ctrl` maps to **Command (⌘)** on macOS and Qt's `Meta` maps to the Control key. So "Ctrl+S" below means ⌘S on macOS. `QKeySequence` standard keys are given with their macOS meaning.

### 1.1 Layout overview

- **Window title**: `"qBittorrent v5.2.4"`. If an instance name is configured (`Application::instanceName()`), append `" — <name>"` (space, U+2014 em dash, space).
  - Session paused: title becomes `"[PAUSED] <title>"`.
  - Otherwise, if "Speed in Title Bar" is on: `"[D: <dl>, U: <ul>] <title>"`. `<dl>`/`<ul>` are `friendlyUnit(payloadRate, isSpeed=true)` values such as `"1.2 MiB/s"`, refreshed on every session stats update.
- **Top toolbar** (QToolBar, not movable, not floatable, top area). See 1.2.
- **Central widget**: a `HidableTabWidget`. Its tab bar is hidden when only 1 tab exists and gets no keyboard focus in that case. On macOS the tabs have no icons and the tab pane frame is not painted. Tabs, in order:
  1. **Transfers**: label `"Transfers (%1)"`, where %1 is the total torrent count (updated on row insert/remove). Icon `folder-remote` (not shown on macOS). Content is a horizontal `QSplitter`:
     - Left: **Filters sidebar** (`TransferListFiltersWidget`). Optional; collapsible; stretch factor 0. Initial width from `GUI/MainWindow/FiltersSidebarWidth` (default 120). Width is saved whenever the splitter moves.
     - Right: a vertical `QSplitter` (children not collapsible) containing the **torrent list** (`TransferListWidget`) above the **properties panel** (`PropertiesWidget`, described in the Properties panel section). The properties panel shows the *current* torrent (`currentTorrentChanged` signal).
  2. **Search**: only present when the Search Engine is enabled. Inserted at index 1. Label `"Search"`, icon `edit-find`.
  3. **RSS**: only when the RSS Reader is enabled. Label `"RSS (%1)"`, where %1 is the root folder unread count (updated live). Icon `application-rss`.
  4. **Execution Log**: only when View → Log → Show is checked. Label `"Execution Log"`, icon `help-contents`. Appended at the end.
- **Status bar**: optional. See 1.4.
- Window geometry is saved to `MainWindow/geometry` and restored on start. If nothing was saved, the window is centered on screen at first show.
- When a tab other than Transfers is selected, the toolbar filter box is hidden. Switching to the Search tab focuses the search input. Switching back to Transfers reloads the dynamic data of the properties panel.

### 1.2 Toolbar

Actions in order (`mainwindow.ui`, plus widgets inserted in code):

| # | Action | Text (iconText shown in toolbar) | Icon id (fallback) | Shortcut |
|---|---|---|---|---|
| 1 | actionOpen | "&Add Torrent File..." (iconText "Open") | `list-add` | `QKeySequence::Open` (Ctrl+O) |
| 2 | actionDownloadFromURL | "Add Torrent &Link..." (iconText "Open URL") | `insert-link` | Ctrl+Shift+O |
| 3 | actionDelete | "&Remove" | `list-remove` | Delete key (macOS: Ctrl+Backspace = ⌘⌫), widget-scoped (see note) |
| — | separator | | | |
| 4 | actionStart | "Sta&rt" | `torrent-start` (`media-playback-start`) | Ctrl+S |
| 5 | actionStop | "Sto&p" | `torrent-stop` (`media-playback-pause`) | Ctrl+P |
| — | queue separator (inserted in code; hidden with queue actions) | | | |
| 6 | actionTopQueuePos | "Top of Queue", tooltip "Move to the top of the queue" | `go-top` | Ctrl+Shift+Plus |
| 7 | actionIncreaseQueuePos | "Move Up Queue", tooltip "Move up in the queue" | `go-up` | Ctrl+Plus |
| 8 | actionDecreaseQueuePos | "Move Down Queue", tooltip "Move down in the queue" | `go-down` | Ctrl+Minus |
| 9 | actionBottomQueuePos | "Bottom of Queue", tooltip "Move to the bottom of the queue" | `go-bottom` | Ctrl+Shift+Minus |
| — | separator | | | |
| 10 | actionCreateTorrent | "Torrent &Creator" | `torrent-creator` (`document-edit`) | `QKeySequence::New` (Ctrl+N) |
| — | separator | | | |
| 11 | actionOptions | "&Options..." (Unix non-mac: text "Preferences") | `configure` (`preferences-system`) | Alt+O |
| — | expanding spacer + **filter widget** (see below) | | | |
| 12 | actionLock | "L&ock qBittorrent" (iconText "Lock"), with drop-down menu | `object-locked` | Ctrl+L |

Notes:
- The queue actions, their toolbar separator and their Edit-menu separator are visible only when the queueing system is enabled (`BitTorrent::Session::isQueueingSystemEnabled()`). Visibility is re-evaluated in `loadPreferences()`. The same switch hides or shows the `#` (queue position) column.
- Queue and Remove actions only act when the Transfers tab is the current tab and the window is visible and not minimized (`currentTabWidget() == transferListWidget`).
- **Lock button menu** (drop-down on actionLock): `"&Set Password"` and `"&Clear Password"` (see 1.3.6).
- actionLock is hidden on non-macOS platforms when desktop integration (tray icon) is inactive. On macOS it is always visible.
- **macOS toolbar specifics**: toolbar separators are replaced by fixed 16 px spacers. An 8 px spacer goes before "Add Torrent Link" and one at the end. `setUnifiedTitleAndToolBarOnMac(true)`.
- **Toolbar context menu** (right-click on toolbar): an exclusive radio group with `"Icons Only"`, `"Text Only"`, `"Text Alongside Icons"`, `"Text Under Icons"`, `"Follow System Style"`. Stored as `Qt::ToolButtonStyle` int in `Toolbar/textPosition`: 0=IconOnly, 1=TextOnly, 2=TextBesideIcon, 3=TextUnderIcon, 4=FollowStyle. Default -1, treated as Follow System Style.
- **Filter widget** (right-aligned in the toolbar, inserted before actionLock):
  - `LineEdit` with placeholder `"Filter torrents..."`, fixed width 200 px, a leading search icon (`edit-find`) and a clear button. Text changes are debounced by **400 ms** before the filter applies. Esc (no modifiers) clears the text.
  - Label `"Filter by:"`.
  - Combo box with items taken from the column headers: `"Name"` (TR_NAME) and `"Save Path"` (TR_SAVE_PATH).
  - Filtering: a case-insensitive `QRegularExpression` on the chosen column's display text. If "Use regular expressions" is off (default), the text is converted with `wildcardToRegularExpression(pattern, UnanchoredWildcardConversion | NonPathWildcardConversion)`. So it is a substring match where `*` and `?` are wildcards that also match `/`.
  - Right-click on the line edit: the standard edit menu, then a separator, then a checkable `"Use regular expressions"` (pref `TransferList/UseRegexAsFilteringPattern`, default false). Toggling it re-applies the filter immediately.
  - The filter text is cleared when the toolbar is hidden.
  - (The 3-way `"Pattern Format"` submenu, with `"Plain text"`/`"Wildcards"`/`"Regular expression"` and `FilterPatternFormat` enum PlainText=0, Wildcards=1, Regex=2 defaulting to Wildcards, is **not** used by the transfer list filter. It is used by the content-filter boxes in the Add Torrent dialog and the Properties → Content tab.)
- Hiding the toolbar on non-macOS fires a `ToolBarChange` event, which toggles actionTopToolBar and persists `Preferences/General/ToolbarDisplayed`.

### 1.3 Menu bar

Top-level order: **File**, **Edit**, **View**, **Tools**, (macOS only: **Window**), **Help**.

#### 1.3.1 File (`"&File"`)
1. "&Add Torrent File..." (Ctrl+O). Opens a multi-select file dialog titled `"Open Torrent Files"`, filter `"Torrent Files (*.torrent)"`, starting in `MainWindow/LastDir` (default: home dir). Each chosen file goes through AddTorrentManager (honors the "show Add Torrent dialog" pref). The parent directory of the first file is saved as the new last dir.
2. "Add Torrent &Link..." (Ctrl+Shift+O). Opens the Download-from-URLs dialog (single instance, non-modal `open()`). Each URL/magnet it returns is added through AddTorrentManager.
3. "Close Window" (`QKeySequence::Close` = ⌘W). **macOS only**, hidden elsewhere. Hides the window; the app keeps running.
4. separator
5. "E&xit" (Ctrl+Q). `QAction::QuitRole`, so on macOS it moves to the application menu as Quit. If the UI is locked and the window is hidden, the user must first enter the lock password. Sets force-exit and closes (see 1.3.8 close semantics).

#### 1.3.2 Edit (`"&Edit"`)
1. "Sta&rt" (Ctrl+S): `start()` on the selected torrents.
2. "Sto&p" (Ctrl+P): `stop()` on the selected torrents.
3. separator
4. "&Remove" (Delete / ⌘⌫): soft delete, i.e. remove selected torrents with the confirmation dialog defaulting to "keep content" (see 1.9).
5. (queue separator, visible only with queueing) "Top of Queue", "Move Up Queue", "Move Down Queue", "Bottom of Queue".
6. separator
7. "Pau&se Session" (Ctrl+Shift+P): visible only when the session is **not** paused. Calls `Session::pause()`, which pauses the libtorrent session as a whole and resets tracker entry statuses. Torrent stopped/started flags are untouched.
8. "R&esume Session" (Ctrl+Shift+S): visible only when the session **is** paused. Calls `Session::resume()`. The session starts paused if the advanced setting "Start session in paused state" (`BitTorrent/Session/StartPaused`) is set.

#### 1.3.3 View (`"&View"`)
1. separator
2. "&Top Toolbar" (checkable, tooltip "Display Top Toolbar"): pref `Preferences/General/ToolbarDisplayed`, default true.
3. "Status &Bar" (checkable): pref `Preferences/General/StatusbarDisplayed`, default true. Destroys or creates the status bar.
4. "Filters Sidebar" (checkable): pref `GUI/MainWindow/FiltersSidebarVisible`, default true. When off, the sidebar widget is destroyed and all sidebar filters are reset: status filter = stored `TransferListFilters/selectedFilterIndex`, category = any, tag = any, tracker = any.
5. "S&peed in Title Bar" (checkable, tooltip "Show Transfer Speed in Title Bar"): pref `Preferences/General/SpeedInTitleBar`, default false.
6. separator
7. "Search &Engine" (checkable): pref `Preferences/Search/SearchEnabled`, default false. When it is checked, Python is validated first:
   - Not found: unchecks it and shows info `"Missing Python Runtime"` / `"Python is required to use the search engine but it does not seem to be installed."` (Windows offers to download and install Python 3.14.2 from python.org with MD5 + SHA3-512 verification).
   - Too old: unchecks it and shows `"Old Python Runtime"` / `"Your Python version (%1) is outdated. Please upgrade to latest version for search engines to work.\nMinimum requirement: %2."`.
   - Valid: adds the Search tab.
   - If the pref was already on at startup, the tab is created through a queued call.
8. "&RSS Reader" (checkable): pref `GUI/RSSWidget/Enabled`, default false. Adds or removes the RSS tab.
9. "&Log" submenu (icon `help-contents`):
   - "Show" (checkable): pref `GUI/Log/Enabled`, default false. Adds or removes the Execution Log tab.
   - separator
   - "Normal Messages", "Information Messages", "Warning Messages", "Critical Messages": checkable, enabled only while Show is checked. They map to the `Log::MsgType` flags NORMAL=0x1, INFO=0x2, WARNING=0x4, CRITICAL=0x8 (ALL=-1). Stored in `GUI/Log/Types`, default ALL. Each toggles which message types the Execution Log tab shows.
10. separator
11. "&Statistics" (Ctrl+I, icon `view-statistics`): opens the single-instance, non-modal Statistics dialog (re-activated if already open).
12. separator
13. "L&ock qBittorrent" (Ctrl+L), with submenu "&Set Password" / "&Clear Password".

#### 1.3.4 Tools (`"&Tools"`)
1. "Torrent &Creator" (Ctrl+N): opens the single-instance Torrent Creator dialog. If it is already open, it is activated and its input path updated.
2. separator
3. "Manage Cookies..." (tooltip "Manage stored network cookies", icon `browser-cookies`): opens the Cookies dialog.
4. "&Options..." (Alt+O; text "Preferences" on Linux/BSD). `PreferencesRole`, so it moves to the app menu on macOS. Opens the single-instance Options dialog.
5. separator
6. "On Downloads &Done" submenu (icon `task-complete`). An exclusive checkable group:
   - "&Do nothing": checked when none of the others are.
   - "&Exit qBittorrent": pref `Preferences/Downloads/AutoShutDownqBTOnCompletion`.
   - "&Suspend System": `Preferences/Downloads/AutoSuspendOnCompletion`.
   - "&Hibernate System": `Preferences/Downloads/AutoHibernateOnCompletion`. **Disabled on macOS** (no Apple Events API).
   - "&Reboot System": `Preferences/Downloads/AutoRebootOnCompletion`.
   - "Sh&utdown System": `Preferences/Downloads/AutoShutDownOnCompletion`.
   - All prefs default false. On Linux without D-Bus, Shutdown/Suspend/Hibernate are disabled.
   - Semantics: `Session::allTorrentsFinished` is emitted after processing newly finished torrents when no torrent is still "unfinished" (unfinished = not finished, not stopped and not errored).
   - Action priority: Suspend > Hibernate > Shutdown > Reboot > Exit.
   - A `ShutdownConfirmDialog` is shown (always on top, Cancel is default, **15 s** countdown with text `"You can cancel the action within %1 seconds."`, auto-accepts at 0). Variants:
     - Exit: `"qBittorrent will now exit."`, button `"E&xit Now"`, title `"Exit confirmation"`, plus a `"Don't show again"` checkbox → pref `ShutdownConfirmDlg/DontConfirmAutoExit`. When that pref is set, Exit happens without the dialog.
     - Shutdown: `"The computer is going to shutdown."`, `"&Shutdown Now"`, `"Shutdown confirmation"`.
     - Suspend: `"The computer is going to enter suspend mode."`, `"&Suspend Now"`, `"Suspend confirmation"`.
     - Hibernate: `"The computer is going to enter hibernation mode."`, `"&Hibernate Now"`, `"Hibernate confirmation"`.
     - Reboot: `"The computer is going to reboot."`, `"&Reboot Now"`, `"Reboot confirmation"`.
   - For a system action, all four system-action prefs are reset to false (one-shot), then the app exits and performs the action. Exit-qBittorrent stays set.

#### 1.3.5 Help (`"&Help"`)
1. "&Documentation" (`QKeySequence::HelpContents`, F1 / ⌘?): opens `https://doc.qbittorrent.org`.
2. "Check for Updates" (tooltip "Check for Program Updates"). **Windows/macOS only**, hidden elsewhere. `ApplicationSpecificRole`, so it goes in the macOS app menu. See 1.3.7.
3. separator
4. "Do&nate!" (tooltip "If you like qBittorrent, please donate!", icon `wallet-open`): opens `https://www.qbittorrent.org/donate`.
5. "&About" (`AboutRole`, so it is in the macOS app menu): single-instance About dialog.

macOS **Window** menu (inserted before Help, built natively by `MacUtils::setupWindowMenu`): "Minimize" (⌘M, `performMiniaturize:`), "Zoom" (`performZoom:`), separator, "Bring All to Front" (`arrangeInFront:`). It is registered as `NSApp.windowsMenu`, so macOS appends the window list. There is also a hidden `actionMinimize` with Ctrl+M (⌘M) on macOS that minimizes the window.

Actions not in any menu (used in the tray/dock menu):
- "Set Global Speed Limits..." (icon `speedometer`): opens the global Speed Limits dialog.
- "Alternative Speed Limits" (checkable): toggles `Session::setAltGlobalSpeedLimitEnabled(!current)`. Its check state follows `speedLimitModeChanged`.
- "Show"/"Hide" (toggle visibility).

#### 1.3.6 UI lock
- "Set Password": `AutoExpandableDialog` titled `"UI lock password"` with prompt `"Please type the UI lock password:"` (password echo). A password shorter than 3 characters gives a warning, `"Invalid password"` / `"The password must be at least 3 characters long"`. It is stored as a PBKDF2 hash in `Locking/password_PBKDF2`.
- "Clear Password": confirmation `"Clear the password"` / `"Are you sure you want to clear the password?"` (Yes/No, default No).
- Triggering Lock: if no password is set, the set-password prompt runs first (lock is aborted on cancel). Then `Locking/locked=true`, the tray/dock menu is disabled, and the window is hidden.
- Unlock happens when the window is re-shown from the tray, a notification is clicked, the dock is activated, or Exit is chosen while hidden. The same password prompt appears; a wrong password gives `"Invalid password"` / `"The password is invalid"`. Success sets `locked=false` and re-enables the tray menu.
- If the app starts in the locked state and the tray is active, the window is not shown at startup.

#### 1.3.7 Program update check (Windows/macOS)
- Automatic check on startup if `Preferences/Advanced/updateCheck` (default true), then every **24 h** (single-shot timer restarted after each check).
- While running, the action text becomes `"Checking for Updates..."` (disabled, tooltip `"Already checking for program updates in the background"`). Afterwards it is restored to `"&Check for Updates"` / `"Check for program updates"`.
- Three parallel requests (proxy applies if "use proxy for general purposes" is set):
  1. FossHub RSS `https://www.fosshub.com/feed/5b8793a7f9ee5a5c3e97a3b2.xml` with User-Agent `"qBittorrent/5.2.4 ProgramUpdater (www.qbittorrent.org)"`. It parses `<item>` elements with `<type>`, `<version>`, `<link>` and picks the item whose type equals `"Mac OS X"` (or `"Windows x64"`).
  2. `https://www.qbittorrent.org/versions.json`
  3. `https://qbittorrent.github.io/qBittorrent-website/versions.json`

  For the two JSON sources, it reads `json["macos"]["version"]` (`"win"` on Windows).
- A version counts as newer if it is greater than 5.2.4.0, or equal while the current build status contains alpha/beta/rc. The highest of the three sources wins.
- New version found: non-modal question box `"qBittorrent Update Available"`, `"A new version is available.<br/>Do you want to download %1?<br/><br/><a href=\"https://www.qbittorrent.org/news\">Open changelog...</a>"`. Yes opens the FossHub item link, `https://www.qbittorrent.org/download`, or `https://qbittorrent.github.io/qBittorrent-website/download`, depending on which source won.
- Nothing found and the check was user-invoked: `"No updates available.\nYou are already using the latest version."`.
- Failures log a warning: `"Failed to download the program update info. URL: \"%1\". Error: \"%2\""`.
- Separately, a file-system watcher on the executable fires when the binary changes. It logs CRITICAL `"qBittorrent was just updated and needs to be restarted for the changes to be effective."` and adds a warning icon plus the label `"qBittorrent needs to be restarted!"` to the status bar.

#### 1.3.8 Window close / minimize / tray behaviours
- **macOS**: closing the window (red button or ⌘W) only hides it, unless force-exit (Quit) is set. Clicking the Dock icon (`applicationShouldHandleReopen` override) emits `activationRequested`, which shows the window, asking for the unlock password if locked.
- **Other OS**, tray active:
  - Close-to-tray if `Preferences/General/CloseToTray` (default true): hide instead of quit. The first time, notify `"qBittorrent is closed to tray"` / `"This behavior can be changed in the settings. You won't be reminded again."` (`CloseToTrayNotified`).
  - Minimize-to-tray if `Preferences/General/MinimizeToTray` (default false), unless a modal window is open. First time, notify `"qBittorrent is minimized to tray"` (same body).
  - Tray left-click (Trigger) toggles visibility.
- **Exit confirmation** (all OS): if `Preferences/General/ExitConfirm` (default true) and any torrent is *active* (payload rate > 0 in either direction), on a user-initiated close or Exit the app shows `"Exiting qBittorrent"`, `"Some files are currently transferring.\nAre you sure you want to quit qBittorrent?"`. Buttons are `"&No"` (default), `"&Yes"`, `"&Always Yes"`; Always Yes sets ExitConfirm=false.
- **Tray/Dock menu** (`DesktopIntegration::menu()`; on macOS it is the Dock menu via `setAsDockMenu()`):
  - non-macOS only: "Show"/"Hide" (text updated on aboutToShow), separator.
  - "&Add Torrent File...", "Add Torrent &Link...", separator.
  - "Alternative Speed Limits" (checkable), "Set Global Speed Limits...", separator.
  - "R&esume Session" / "Pau&se Session" (whichever is visible).
  - non-macOS only: separator, "E&xit".
  - The whole menu is disabled while the UI is locked.
- **Tray tooltip** (non-macOS): `"DL speed: %1\nUP speed: %2"`, or `"Paused"` while the session is paused.
- **Tray icon style** (non-macOS): `Preferences/Advanced/TrayIconStyle`. Normal → `qbittorrent-tray`, MonoDark → `qbittorrent-tray-dark`, MonoLight → `qbittorrent-tray-light`. The tray icon exists if `Preferences/General/SystrayEnabled` (default true).
- **Initial window state** (from the startup-progress dialog state): Normal shows and raises the window; Minimized shows it minimized (and hides it to tray if minimize-to-tray is on); Hidden stays hidden. On macOS it is always shown (minimized if requested).
- **Startup**:
  - Legal notice (once, `LegalNotice/Accepted`; the CLI option `--confirm-legal-notice` suppresses it): message box titled `"Legal Notice"` with the body `"qBittorrent is a file sharing program. When you run a torrent, its data will be made available to others by means of upload. Any content you share is your sole responsibility.\n\nNo further notices will be issued."`.
  - A `QProgressDialog` `"Loading torrents..."` with an `"Exit"` button and range 0–100 is shown while torrents load.

#### 1.3.9 Other main-window behaviours
- **Tab shortcuts**: Alt+1 selects Transfers. Alt+2 selects Search, enabling it if needed. Alt+3 selects RSS, enabling it if needed. Alt+4 selects Execution Log, enabling it if needed.
- **Filter focus**: `QKeySequence::Find` (Ctrl+F) or Ctrl+E, while the transfer list has focus, toggles focus between the toolbar filter box and, if the Properties "Content" tab is current, the content filter box. The target box's text is selected.
- **Paste** (`QKeySequence::Paste`) anywhere in the main window: if the clipboard has text, it is split on `\n`. Every trimmed line that `isTorrentLink()` accepts is added through AddTorrentManager. `isTorrentLink` is true when the line starts with `magnet:` (case-insensitive), or ends with `.torrent`, or starts with any `QNetworkAccessManager::supportedSchemes()` scheme + `:` (in practice `http:`, `https:`, `data:`; `ftp:` if the Qt build supports it) and does not start with `file:`.
- **Drag & drop onto the torrent list**: accepts any drop with text or URLs (copy action). Local file URLs become paths; others are used as strings. Plain text is split by newline. Items passing `isTorrentLink` are all added. If none are torrent links, a Torrent Creator dialog opens with the **first** dropped path pre-filled.
- **macOS file-open events** (Finder "Open With", magnet scheme handler): `QFileOpenEvent` with a file path or URL is processed as a torrent source.
- **Desktop notifications** (if `GUI/Notifications/Enabled`, default true; on macOS via `UNUserNotificationCenter` with the default sound, with permission requested at startup):
  - `"Download completed"` / `"'%1' has finished downloading."` on each torrent finished.
  - `"Torrent added"` / `"'%1' was added."` (only if `GUI/Notifications/TorrentAdded`, default false).
  - `"Add torrent failed"` / `"Couldn't add torrent '%1', reason: %2."`.
  - `"I/O Error"` / `"An I/O error occurred for torrent '%1'.\n Reason: %2"`.
  - `"Search Engine"` / `"Search has finished"` or `"Search has failed"`, only when the Search tab is not current.
  - Clicking a notification shows and raises the window, unlocking first if needed.
- **Power management**: every 60 s, and on any preference change, the app re-evaluates whether to inhibit system sleep. It inhibits if any torrent is moving, OR (`PreventFromSuspendWhenDownloading`, default false) any torrent is not finished, not stopped, not errored and has metadata, OR (`PreventFromSuspendWhenSeeding`, default false) any torrent is finished and not stopped.
- `"Options saved."` is logged each time preferences change, and the main window reloads its prefs: alternating row colors (`Preferences/General/AlternatingRowColors`, default true) apply to the torrent list and the properties lists, plus queue visibility and the update timer.

### 1.4 Status bar

Left part: transient messages and the restart-required indicator. A permanent container on the right holds, left to right with vertical separators between them:

1. **Free disk space** label: `"Free space: N/A"` initially, then `"Free space: " + friendlyUnit(bytes)`. It measures the **default save path** and is re-checked every **30 s** on the I/O thread. Visible only if `Preferences/General/StatusbarFreeDiskSpaceDisplayed` (default false).
2. **External IP** label. Visible only if `Preferences/General/StatusbarExternalIPDisplayed` (default false). Text:
   - `"External IP: N/A"` when none is known.
   - `"External IP: %1%2"` when only one is known.
   - `"External IPs: %1, %2"` when IPv4 and IPv6 are both known.

   It uses the last external IPv4/IPv6 addresses reported by the session.
3. **DHT** label: `"DHT: %1 nodes"`. Visible only while DHT is enabled.
4. **Connection status** flat icon button with a pointing-hand cursor. Clicking it opens Options on the Connection page.
   - Not listening: icon `disconnected`, tooltip `"<b>Connection Status:</b><br>Offline. This usually means that qBittorrent failed to listen on the selected port for incoming connections."`.
   - Listening with incoming connections: icon `connected`, tooltip `"<b>Connection Status:</b><br>Online"`.
   - Listening with no incoming connections: icon `firewalled`, tooltip `"<b>Connection Status:</b><br><i>No direct connections. This may indicate network configuration problems.</i>"`.
5. **Alternative speed** toggle button. Alt mode on: icon `slow`, shown pressed, tooltip `"Click to switch to regular speed limits"`. Off: icon `slow_off`, tooltip `"Click to switch to alternative speed limits"`. Clicking toggles the alt-speed mode.
6. **Download speed** flat button (icon `downloading`, min width 200, left-aligned text). Text is `"<dlrate>"`, then `" [<limit>]"` if the global DL limit is > 0, then `" (<session total payload downloaded>)"`. Example: `"1.5 MiB/s [2.0 MiB/s] (350.2 MiB)"`. Clicking opens the global Speed Limits dialog.
7. **Upload speed** flat button (icon `upload`), same format with upload values. Clicking opens the Speed Limits dialog.

Refreshed on every `statsUpdated` and on alt-speed mode change.

### 1.5 Filters sidebar

`TransferListFiltersWidget` is a scroll area (no horizontal scrollbar, Base background) with collapsible sections in this order: **Status**, **Categories**, **Tags**, [**Tracker status**], **Trackers**. Each section header is a bold, ALL-CAPS checkbox drawn as a tree expand arrow. Unchecking a section collapses it **and disables that filter**: it resets to "All"/any.

Section expanded-state prefs (all default true):
- `TransferListFilters/statusFilterState`
- `TransferListFilters/CategoryFilterState`
- `TransferListFilters/TagFilterState`
- `TransferListFilters/TrackerStatusFilterState`
- `TransferListFilters/trackerFilterState`

All sidebar filters combine (AND) with each other and with the toolbar text filter.

#### 1.5.1 Status filter (`StatusFilterWidget`, rows = `TorrentFilter::Status` enum values)

| Row/enum | Label | Icon (fallback) |
|---|---|---|
| 0 All | `"All (%1)"` (%1 = total torrents) | `filter-all` (`filterall`) |
| 1 Downloading | `"Downloading (%1)"` | `downloading` |
| 2 Seeding | `"Seeding (%1)"` | `upload` (`uploading`) |
| 3 Completed | `"Completed (%1)"` | `checked-completed` (`completed`) |
| 4 Running | `"Running (%1)"` | `torrent-start` (`media-playback-start`) |
| 5 Stopped | `"Stopped (%1)"` | `stopped` (`media-playback-pause`) |
| 6 Active | `"Active (%1)"` | `filter-active` (`filteractive`) |
| 7 Inactive | `"Inactive (%1)"` | `filter-inactive` (`filterinactive`) |
| 8 Stalled | `"Stalled (%1)"` (= StalledUploading + StalledDownloading counts) | `filter-stalled` (`filterstalled`) |
| 9 StalledUploading | `"Stalled Uploading (%1)"` | `stalledUP` |
| 10 StalledDownloading | `"Stalled Downloading (%1)"` | `stalledDL` |
| 11 Checking | `"Checking (%1)"` | `force-recheck` (`checking`) |
| 12 Moving | `"Moving (%1)"` | `set-location` |
| 13 Errored | `"Errored (%1)"` | `error` |

- Counts are updated incrementally on `torrentsUpdated`, `torrentsLoaded` and `torrentAboutToBeRemoved`.
- The selected row persists in `TransferListFilters/selectedFilterIndex` (default 0) when the widget is destroyed.
- **"Auto hide zero status filters"** (`TransferListFilters/HideZeroStatusFilters`, default false) hides every row except All whose count is 0. If the current row becomes hidden, the selection jumps to All.
- When a status filter is applied and nothing is selected in the list, the first visible row is selected.
- Context menu: `"Start torrents"` (`torrent-start`), `"Stop torrents"` (`torrent-stop`), `"Remove torrents"` (`list-remove`). All act on **all torrents currently visible** in the list, i.e. after every active filter. Remove shows the deletion confirmation with content-removal defaulting to the pref.

#### 1.5.2 Categories (`CategoryFilterWidget`, a tree view)

- Fixed rows: row 0 `"All (%1)"` (all torrents), row 1 `"Uncategorized (%1)"` (torrents with an empty category). Then the categories as a **tree**. Subcategories are always supported in 5.2; `/` is the separator, and parents are auto-created when a child is added. Each node shows `"<short name> (<count>)"`.
- A parent's count = torrents whose category equals the parent exactly, plus the counts of all descendants.
- Icon `view-categories` (`inode-directory`). Categories are sorted with natural, case-insensitive compare. All/Uncategorized always stay on top.
- Indentation is 0 when no subcategories exist. New rows auto-expand their parents.
- Filter semantics: All = no filter; Uncategorized = `category.isEmpty()`. A category X matches torrents whose category == X **or starts with `X/`** (subcategories are included).
- Context menu:
  - `"Add category..."` (always): opens the new-category dialog.
  - Only when a real category is selected:
    - `"Add subcategory..."`: new-category dialog with the parent prefilled.
    - `"Edit category..."`: edits the save path and download path.
    - `"Remove category"`: no confirmation. Removes the category and all its subcategories. Torrents in it or its subcategories move to the removed category's **parent** category ("" for top level).
  - `"Remove unused categories"` (always): removes every category whose count is 0.
  - separator, `"Start torrents"`, `"Stop torrents"`, `"Remove torrents"` (on visible torrents).
- Category name validity regex: `^([^\\\/]|[^\\\/]([^\\\/]|\/(?=[^\/]))*[^\\\/])$`. In words: no backslash, no leading or trailing `/`, no `//`.

#### 1.5.3 Tags (`TagFilterWidget`)

- Rows: row 0 `"All (%1)"`, row 1 `"Untagged (%1)"`, then each tag as `"<tag> (<count>)"` with icon `tags` (`inode-directory`). Sorted naturally and case-insensitively.
- Filter: All = none; Untagged = torrents with an empty tag set; tag T = torrents having T.
- Context menu:
  - `"Add tag..."`: prompt `"Add tag"` / `"Tag:"`, which loops until the name is valid. Invalid → `"Invalid tag name"` / `"Tag name '%1' is invalid"`. Duplicate → `"Tag exists"` / `"Tag name already exists."`.
  - `"Remove tag"` (only on a real tag, no confirmation).
  - `"Remove unused tags"`.
  - separator, `"Start torrents"`, `"Stop torrents"`, `"Remove torrents"`.
- Tag validity: the name is trimmed; it must be non-empty and contain no `,`.

#### 1.5.4 Trackers (`TrackersFilterWidget`)

Special rows:
- When "Use separate tracker status filter" is **off** (`TransferListFilters/SeparateTrackerStatusFilter`, default false), the rows are 0 `"All (%1)"` (icon `trackers`), 1 `"Trackerless (%1)"` (icon `trackerless`), 2 `"Tracker error (%1)"` (`tracker-error`), 3 `"Other error (%1)"` (`tracker-error`), 4 `"Warning (%1)"` (`tracker-warning`).
- When **on**, only All and Trackerless remain here, and the three status rows move to the separate **Tracker status** section.

Host rows:
- Then one row per tracker **host** (`QUrl(url).host()`, or the raw URL if parsing fails): `"<host> (<count>)"`, sorted by case-sensitive natural compare.
- Count = number of torrents with at least one tracker on that host.
- Rows are created on demand and removed when the count drops to 0. If the removed row was selected, the selection resets to All.

Favicons:
- Only when the Advanced setting "Download tracker's favicon" is on (`GUI/DownloadTrackerFavicon`, default false). Otherwise rows use the generic `trackers` icon.
- The app fetches `<scheme>://<host2>/favicon.ico`. `<scheme>` is http unless the tracker URL scheme starts with http. `<host2>` is the IP if the host is an IP, otherwise the **last two labels** of the hostname (e.g. `tracker.example.org` → `example.org`).
- If the download fails or the image cannot be decoded, it retries `.png`.
- Icons are saved to temp files, which are deleted on widget destruction. The general-purpose proxy pref applies.

Filter semantics:
- All: no filter. Trackerless: torrents with no trackers.
- Host H: any tracker whose host == H.
- Tracker error: announce status has `HasTrackerError`. Other error: `HasOtherError`. Warning: `HasWarning`.

Announce status flags (`TorrentAnnounceStatusFlag`): HasNoProblem=0, HasWarning=1, HasTrackerError=2, HasOtherError=4. They are computed per torrent over its trackers:
- Tracker state Working with any Working endpoint having a non-empty message → HasWarning.
- State NotWorking or Unreachable → HasOtherError.
- State TrackerError → HasTrackerError.
- NotContacted → no flag.

Context menu:
- If a host row is selected: `"Remove tracker"` (`edit-clear`), then a separator. It removes every tracker URL with that host from **all** torrents. If `GUI/ConfirmActions/RemoveTrackerFromAllTorrents` (default true), it first asks `"Removal confirmation"` / `"Are you sure you want to remove tracker \"%1\" from all torrents?"` with a `"Don't ask me again."` checkbox, which clears the pref.
- Then `"Start torrents"`, `"Stop torrents"`, `"Remove torrents"`.

#### 1.5.5 Tracker status (only with SeparateTrackerStatusFilter = true)

Rows:
- 0 `"All (%1)"` (all torrents, icon `trackers`)
- 1 `"Warning (%1)"` (`tracker-warning`)
- 2 `"Tracker error (%1)"` (`tracker-error`)
- 3 `"Other error (%1)"` (`tracker-error`)

These filter by announce-status flag and combine (AND) with the Trackers host filter. When combined with a host filter, matching becomes per tracker on that host: Working with a warning message → HasWarning; NotWorking/Unreachable → HasOtherError; TrackerError → HasTrackerError; NotContacted never matches; and filter "no problem" matches Working without a message.

Context menu: `"Start torrents"`, `"Stop torrents"`, `"Remove torrents"`.

### 1.6 Torrent list (`TransferListWidget` / `TransferListModel`)

View properties:
- Uniform row heights, no root decoration, extended multi-selection, sortable, drop-only drag/drop.
- First column movable; header sections reorderable and resizable. Header state saved to `GUI/Qt6/TransferList/HeaderState` on move, resize and sort change.
- Shift+mouse wheel scrolls horizontally.
- macOS uses a custom shift-click handler: Shift+click selects the range from the last clicked row; Cmd+Shift+click adds that range to the selection. No focus rect is drawn.
- Rows keep the height of the Name column (icon + text) even when Name is scrolled out of view.
- The `Name` column shows the state icon.
- **Text color per state** (if `GUI/TransferList/UseTorrentStatesColors`, default true): see 1.7.
- **"Hide zero and infinity values"** (`Preferences/General/HideZeroValues`, default false) with mode combo `Preferences/General/HideZeroComboValues`: 0 = "Always", 1 = "Stopped torrents only" (applies only when state == StoppedDownloading). When it applies, zero values, infinite values, etc. are rendered as empty strings, as noted per column.

#### 1.6.1 Columns (enum `TransferListModel::Column`, in order)

H = hidden by default (applies when no saved header state exists). R = right-aligned (`AlignRight|AlignVCenter`). T = tooltip shows the full display text.

| # | Enum | Header title | Def. | Align | Display value / formatting | Sort |
|---|---|---|---|---|---|---|
| 0 | TR_QUEUE_POSITION | `#` | shown (hidden if queueing disabled) | R | `queuePosition()+1`; `"*"` when position < 0 (not queued: finished/seeding torrents) | int; negatives sort after valid values |
| 1 | TR_NAME | `"Name"` | shown | L, T, state icon | torrent name; editable via Rename | natural, case-insensitive |
| 2 | TR_SIZE | `"Size"` | shown | R | `wantedSize()` (bytes of selected files) → friendlyUnit; hide-zero: empty if 0 | int64 |
| 3 | TR_TOTAL_SIZE | `"Total Size"` | H | R | `totalSize()` (all files incl. unwanted; -1 without metadata → `"Unknown"`) | int64 |
| 4 | TR_PROGRESS | `"Progress"` | shown | — | Painted as a progress bar with text `"100%"` if progress ≥ 1, else floor to 1 decimal + `"%"` (e.g. `"99.9%"`). Progress = `total_wanted_done/total_wanted` (0 if total_wanted == 0; libtorrent progress while checking). The bar is drawn *disabled* for states Error, StoppedDownloading, Unknown. Bar color = theme `ProgressBar` color, or the row text color if `GUI/TransferList/ProgressBarFollowsTextColor` (default false). Fusion style on Win/mac. | real |
| 5 | TR_STATUS | `"Status"` | shown | L, T | status string (1.7); Error appends `": " + errorMessage` | by state enum int |
| 6 | TR_SEEDS | `"Seeds"` | shown | R | `"%1 (%2)"` = connected seeds (`num_seeds`) and total seeds in swarm (`num_complete` if > -1, else `list_seeds`); hide-zero: empty if both 0 | connected, then total |
| 7 | TR_PEERS | `"Peers"` | shown | R | `"%1 (%2)"` = connected leechers (`num_peers - num_seeds`) and total leechers (`num_incomplete` if > -1, else `list_peers - list_seeds`) | connected, then total |
| 8 | TR_DLSPEED | `"Down Speed"` | shown | R | payload DL rate, friendlyUnit speed (`"x KiB/s"`); hide-zero: empty if 0 | int |
| 9 | TR_UPSPEED | `"Up Speed"` | shown | R | payload UL rate | int |
| 10 | TR_ETA | `"ETA"` | shown | R | `userFriendlyDuration(eta, MAX_ETA=8640000)` → `"∞"` when ≥ 8640000 s (100 days) or < 0; hide-zero: empty when ∞ | int64 |
| 11 | TR_RATIO | `"Ratio"` | shown | R | `realRatio()` floored to 2 decimals; `"∞"` if ratio == -1 or ≥ MAX_RATIO (+∞); hide-zero: empty if ≤ 0 | real |
| 12 | TR_POPULARITY | `"Popularity"` (header tooltip `"Ratio / Time Active (in months), indicates how popular the torrent is"`) | H | R | `realRatio / activeMonths` (0 if active months == 0), same formatting as ratio | real |
| 13 | TR_CATEGORY | `"Category"` | shown | L, T | category full name | natural |
| 14 | TR_TAGS | `"Tags"` | shown | L, T | tags joined with `", "` | per tag natural compare, then by count |
| 15 | TR_ADD_DATE | `"Added On"` | H | L | `QLocale().toString(local time, ShortFormat)` | datetime; invalid dates last |
| 16 | TR_SEED_DATE | `"Completed On"` | H | L | completed time, same format (empty if never) | datetime |
| 17 | TR_TRACKER | `"Tracker"` | H | L, T | libtorrent `current_tracker`, else the first tracker URL, else empty | natural |
| 18 | TR_DLLIMIT | `"Down Limit"` | H | R | > 0 → friendlyUnit speed; else `"∞"`; hide-zero: empty if ≤ 0 | int |
| 19 | TR_UPLIMIT | `"Up Limit"` | H | R | same | int |
| 20 | TR_AMOUNT_DOWNLOADED | `"Downloaded"` | H | R | `all_time_download` | int64 |
| 21 | TR_AMOUNT_UPLOADED | `"Uploaded"` | H | R | `all_time_upload` | int64 |
| 22 | TR_AMOUNT_DOWNLOADED_SESSION | `"Session Downloaded"` | H | R | `total_payload_download` (since session start) | int64 |
| 23 | TR_AMOUNT_UPLOADED_SESSION | `"Session Uploaded"` | H | R | `total_payload_upload` | int64 |
| 24 | TR_AMOUNT_LEFT | `"Remaining"` | H | R | `wantedSize - completedSize` | int64 |
| 25 | TR_TIME_ELAPSED | `"Time Active"` | H | L | if seeding time ≤ 0: `userFriendlyDuration(activeTime)`; else `"%1 (seeded for %2)"` | activeTime |
| 26 | TR_SAVE_PATH | `"Save Path"` | H | L, T | save path (native separators) | natural |
| 27 | TR_COMPLETED | `"Completed"` | H | R | `total_wanted_done` bytes | int64 |
| 28 | TR_RATIO_LIMIT | `"Ratio Limit"` | H | R | effective ratio limit: torrent value, or category/global if "default" (-2) is set; -1 → `"∞"` | real |
| 29 | TR_SEEN_COMPLETE_DATE | `"Last Seen Complete"` | H | L | date, short format | datetime |
| 30 | TR_LAST_ACTIVITY | `"Last Activity"` | H | R | `"%1 ago"` with duration (0 is shown as `"< 1m ago"`); `"∞"` if never (< 0); hide-zero: empty if < 0 or ≥ MAX_ETA. Time since activity = min(time since last upload, time since last download) when both are known, else whichever is known (−1 if neither). | int64 |
| 31 | TR_AVAILABILITY | `"Availability"` | shown | R | `distributed_copies` floored to 3 decimals; `"N/A"` if < 0; hide-zero: empty if 0 | real |
| 32 | TR_DOWNLOAD_PATH | `"Incomplete Save Path"` | H | L, T | download (incomplete) path | natural |
| 33 | TR_INFOHASH_V1 | `"Info Hash v1"` | H | L, T | 40-hex v1 hash or `"N/A"` | hash bytes |
| 34 | TR_INFOHASH_V2 | `"Info Hash v2"` | H | L, T | 64-hex v2 hash or `"N/A"` | hash bytes |
| 35 | TR_REANNOUNCE | `"Reannounce In"` | H | R | `userFriendlyDuration(next_announce secs)`; hide-zero: empty if 0 | int64 |
| 36 | TR_PRIVATE | `"Private"` | H | L | `"Yes"`/`"No"` with metadata; `"N/A"` without; hide-zero: empty if not private | bool; N/A last |
| 37 | TR_CREATE_DATE | `"Created On"` | H | L | torrent creation date, short format | datetime |

Formatting helpers (exact rules):
- `friendlyUnit(bytes, isSpeed)`: divide by 1024 while ≥ 1024, up to EiB. Units are `B`, `KiB`, `MiB`, `GiB`, `TiB`, `PiB`, `EiB`, plus `/s` for speeds, separated from the number by a no-break space. Decimal precision: B 0, KiB/MiB 1, GiB 2, larger 3. Values are **floored**, not rounded (`fromDouble`), and use the system locale decimal separator. Negative input → `"Unknown"`.
- `userFriendlyDuration(seconds, maxCap=-1, resolution=Minutes)`:
  - < 0, or ≥ maxCap when a cap is given → `"∞"`
  - 0 → `"0"`
  - < 60 → `"< 1m"`
  - < 1 h → `"%1m"`
  - < 1 day → `"%1h %2m"`
  - < 365 days → `"%1d %2h"`
  - otherwise `"%1y %2d"`
- `C_INFINITY` = `"∞"`.
- Ratio: `realRatio = all_time_upload / download`. `download` = `total_done` if `all_time_download < total_done * 0.01`, else `all_time_download`. If download == 0: 0 when upload == 0, else +∞.
- ETA:
  - Stopped → MAX_ETA.
  - Finished: min(ratio ETA, seeding-time ETA, inactive-seeding ETA) from the effective share limits. Ratio ETA = `(realDL*maxRatio - totalUpload)/avgUpSpeed`, where realDL = totalDownload or wantedSize. Seeding ETA = `limit*60 - finishedTime` (clamped ≥ 0). Inactive ETA = `limit*60 - timeSinceActivity` (clamped ≥ 0). MAX_ETA if no limits apply.
  - Downloading: `(wanted - completed) / avg DL speed` (sampled average), MAX_ETA if the speed is 0.

**Sorting**:
- Sort role is the underlying data (not the text). Comparison is case-insensitive natural compare for text.
- Numeric "invalid" (negative) values sort after valid ones in ascending order.
- Ties are broken by a **secondary sort column**: the previously used sort column and order, persisted in `TransferList/SubSortColumn` (default Name) and `TransferList/SubSortOrder` (0 asc / 1 desc).

**Header context menu** (titled "Column visibility", tooltips visible):
- One checkable entry per column, labelled with the header title. The Popularity entry shows its header tooltip.
- The `#` entry is omitted when queueing is disabled.
- Unchecking the last visible column is refused (at least 1 column stays visible).
- Showing a column whose width is ≤ 5 auto-resizes it to contents.
- separator, `"Resize columns"` (tooltip `"Resize all non-hidden columns to the size of their contents"`).

**Keyboard shortcuts** (widget-scoped, on the list):
- F2: rename (single selection).
- Delete (macOS ⌘⌫): soft delete, i.e. a confirmation dialog defaulting to keep files.
- Shift+Delete: delete with the "remove content" checkbox pre-checked.
- Return / Enter: same as double-click.
- Ctrl+R: force recheck.
- Ctrl+M: force start. (On macOS ⌘M is also the window "Minimize" shortcut.)

**Double-click / Enter action**. Uses `Preferences/Downloads/DblClOnTorDl` (default 0) when the torrent is not finished, `Preferences/Downloads/DblClOnTorFn` (default 1) when finished. Single selection only. Values:

| Value | Name | Options combo text | Behavior |
|---|---|---|---|
| 0 | TOGGLE_STOP | "Start / stop torrent" | stopped → start, else stop |
| 1 | OPEN_DEST | "Open destination folder" | open content location (see "Open destination folder") |
| 2 | PREVIEW_FILE | "Preview file, otherwise open destination folder" | open Preview dialog if any previewable file exists, else open destination |
| 3 | NO_ACTION | "No action" | nothing |
| 4 | SHOW_OPTIONS | "Open torrent options dialog" | open Torrent Options for the selection |

(Combo order in Options is Start/stop, Open dest, Preview, Torrent options, No action, mapped via item data. Labels are "Downloading torrents:" and "Completed torrents:".)

#### 1.6.2 Torrent names and cards (qBit NeoRS extension — not in qBittorrent)

Not part of qBittorrent 5.2.4; GUI-only, no WebAPI change. Details in
`docs/design/torrent-names.md`.
- Torrent names are split into a title and technical tags by an ordered list of editable regular-expression rules (181 built-in, by category: Season & Episode, Volume & Part, Resolution, HDR & Dolby Vision, Source, Video Codec, Audio, Language & Dub, Subtitles, Edition, Platform, Version, Region, Format, Year, Release Group, Other, Hidden), covering video, music, software, games, ROM sets, audiobooks, books, comics and adult release names. A content kind (Video, Music, Software, Game, ROM Collection, Audiobook, Book, Comic, Adult, Other) is guessed from the name, else the category name.
- List: the Name column shows the title semibold and the technical part regular (switchable).
- View › "as List" (⌘1) / "as Cards" (⌘2), also a List / Cards switch in the main toolbar: cards show the kind symbol, the title and a large state symbol, the name's tags in fixed per-category styles followed by the category and tags, then, pinned to the bottom, the progress bar, the status in bold, ratio and size, and speeds and ETA only while transferring; cards are as tall as their content; same selection, sorting, filters, context menu, actions and properties pane as the list.
- Sort By (like Mail's View › Sort By): a pull-down in the main toolbar next to the List / Cards switch, shown in both views (customizable like the other items), and View › Sort By, with the fields Queue Position (only while queueing is on), Name, Size, Progress, Status, Seeds, Peers, Down Speed, Up Speed, ETA, Ratio, Category, Added On, Completed On and Last Activity, then Ascending / Descending; also "Sort by …" / "Sort Ascending" / "Sort Descending" in the Command Palette. They edit the list's column-header sort (choosing a field makes it the primary key, ascending, the former primary the secondary; a field chosen in the header that is not in the list appears checked at the end), persisted in `TransferList/SortOrder`.
- Settings › Interface › Torrent Names: master switch, per-view options, rules editor (reorder, add, remove, reset, JSON import/export, validation), preview.

### 1.7 Torrent states

`BitTorrent::TorrentState` (`src/base/bittorrent/torrent.h`):

| Int | Enum | WebAPI `state` string | GUI status text | Name-column icon | Theme color id (light / dark default) |
|---|---|---|---|---|---|
| -1 | Unknown | `unknown` | (none) | `error` | — |
| 0 | ForcedDownloading | `forcedDL` | `"[F] Downloading"` | `downloading` | TransferList.ForcedDownloading (#1a7f37 / #3fb950) |
| 1 | Downloading | `downloading` | `"Downloading"` | `downloading` | TransferList.Downloading (#1a7f37 / #3fb950) |
| 2 | ForcedDownloadingMetadata | `forcedMetaDL` | `"[F] Downloading metadata"` | `downloading` | TransferList.ForcedDownloadingMetadata (#1a7f37 / #3fb950) |
| 3 | DownloadingMetadata | `metaDL` | `"Downloading metadata"` | `downloading` | TransferList.DownloadingMetadata (#1a7f37 / #3fb950) |
| 4 | StalledDownloading | `stalledDL` | `"Stalled"` | `stalledDL` | TransferList.StalledDownloading (#2da44e / #238636) |
| 5 | ForcedUploading | `forcedUP` | `"[F] Seeding"` | `upload` (`uploading`) | TransferList.ForcedUploading (#0969da / #58a6ff) |
| 6 | Uploading | `uploading` | `"Seeding"` | `upload` | TransferList.Uploading (#0969da / #58a6ff) |
| 7 | StalledUploading | `stalledUP` | `"Seeding"` | `stalledUP` | TransferList.StalledUploading (#0969da / #1f6feb) |
| 8 | CheckingResumeData | `checkingResumeData` | `"Checking resume data"` | `force-recheck` (`checking`) | TransferList.CheckingResumeData (#1a7f37 / #3fb950) |
| 9 | QueuedDownloading | `queuedDL` | `"Queued"` | `queued` | TransferList.QueuedDownloading (#7d4e00 / #845306) |
| 10 | QueuedUploading | `queuedUP` | `"Queued"` | `queued` | TransferList.QueuedUploading (#7d4e00 / #845306) |
| 11 | CheckingUploading | `checkingUP` | `"Checking"` | `force-recheck` | TransferList.CheckingUploading (#1a7f37 / #3fb950) |
| 12 | CheckingDownloading | `checkingDL` | `"Checking"` | `force-recheck` | TransferList.CheckingDownloading (#1a7f37 / #3fb950) |
| 13 | StoppedDownloading | `stoppedDL` | `"Stopped"` | `stopped` (`media-playback-pause`) | TransferList.StoppedDownloading (#57606a / #8b949e) |
| 14 | StoppedUploading | `stoppedUP` | `"Completed"` | `checked-completed` (`completed`) | TransferList.StoppedUploading (#8250df / #a371f7) |
| 15 | Moving | `moving` | `"Moving"` | `set-location` | TransferList.Moving (#1a7f37 / #3fb950) |
| 16 | MissingFiles | `missingFiles` | `"Missing Files"` | `error` | TransferList.MissingFiles (#cf222e / #f85149) |
| 17 | Error | `error` | `"Errored"` + `": <error message>"` | `error` | TransferList.Error (#cf222e / #f85149) |

The colors are the GitHub Primer palette defaults (`src/gui/color.h`) and are overridable by UI themes. Text colors apply only if `UseTorrentStatesColors` is on.

**State computation** (`TorrentImpl::updateState`, first match wins):
1. libtorrent state `checking_resume_data` → CheckingResumeData
2. move in progress → Moving
3. has missing files → MissingFiles
4. has error → Error
5. no metadata: stopped → StoppedDownloading; queued → QueuedDownloading; else forced ? ForcedDownloadingMetadata : DownloadingMetadata
6. libtorrent `checking_files` and not stopped → CheckingUploading if it had previously reached finished status, else CheckingDownloading
7. finished (lt state `finished` or `seeding`): stopped → StoppedUploading; queued → QueuedUploading; forced → ForcedUploading; upload payload rate > 0 → Uploading; else StalledUploading
8. otherwise: stopped → StoppedDownloading; queued → QueuedDownloading; forced → ForcedDownloading; DL payload rate > 0 → Downloading; else StalledDownloading

Definitions:
- *stopped* = the user stop flag.
- *queued* = queueing enabled and not stopped and lt flags auto_managed && paused.
- *forced* = not stopped and operating mode Forced.

**Status-filter bucket membership** (`TorrentFilter::matchStatus`):

| Filter | Match rule |
|---|---|
| All | always |
| Downloading | `isDownloading()`: state ∈ {Downloading, DownloadingMetadata, ForcedDownloadingMetadata, StalledDownloading, CheckingDownloading, StoppedDownloading, QueuedDownloading, ForcedDownloading} |
| Seeding | `isUploading()`: state ∈ {Uploading, StalledUploading, CheckingUploading, QueuedUploading, ForcedUploading} |
| Completed | `isCompleted()`: state ∈ {Uploading, StalledUploading, CheckingUploading, StoppedUploading, QueuedUploading, ForcedUploading} |
| Running | `!isStopped()` (stop flag, independent of state) |
| Stopped | `isStopped()` |
| Active | `uploadPayloadRate() > 0 \|\| downloadPayloadRate() > 0` |
| Inactive | `!Active` |
| Stalled | state ∈ {StalledUploading, StalledDownloading} |
| Stalled Uploading | state == StalledUploading |
| Stalled Downloading | state == StalledDownloading |
| Checking | state ∈ {CheckingUploading, CheckingDownloading, CheckingResumeData} |
| Moving | state == Moving |
| Errored | state ∈ {MissingFiles, Error} |

Note: CheckingResumeData, Moving, MissingFiles and Error are in none of Downloading/Seeding/Completed.

### 1.8 Torrent context menu (`TransferListWidget::displayListMenu`)

The menu shows only when at least one row is selected. Tooltips are visible. The menu is computed over all selected torrents:
- `needsStart`: any torrent is stopped, forced, errored, has missing files, or is checking.
- `needsStop`: any is not stopped, or is checking.
- `needsForce`: any is not forced, errored, or has missing files.
- `needsPreview`: any has metadata.
- `oneHasMetadata`, `oneNotFinished`.
- `oneCanForceReannounce`: any that is not stopped, not checking and not queued.
- Tri-state aggregates for sequential, first/last piece priority (only over unfinished torrents), super seeding (only when all are finished and have metadata), Auto TMM, category, and tags (in all vs in any).
- `hasInfohashV1` / `hasInfohashV2`.

Menu items, in order:
1. `"&Start"` (icon `torrent-start`), if needsStart. Calls `start()` in auto-managed mode, which clears error/upload-mode and reloads if files were missing.
2. `"Sto&p"` (icon `torrent-stop`), if needsStop.
3. `"Force Star&t"` (icon `torrent-start-forced`), if needsForce. Calls `start(TorrentOperatingMode::Forced)`, which bypasses the queue.
4. separator
5. `"&Remove"` (icon `list-remove`): soft delete (1.9).
6. separator
7. `"Set loc&ation..."` (icon `set-location`). Opens a directory picker titled `"Choose save path"`, starting at the first torrent's save path. On accept, for each selected torrent: **disable Auto TMM**, then `setSavePath(new)`, which moves files. Ignored if the directory does not exist.
8. `"Re&name..."` (icon `edit-rename`), **only if exactly 1 is selected**. Prompt `"Rename"` / `"New name:"` prefilled with the current name. Newlines are replaced by spaces. Sets the torrent display name (not the files).
9. `"Edit trac&kers..."` (icon `edit-rename`). Opens the Tracker Entries dialog prefilled with trackers (URL + tier) **common to all** selected torrents. On accept, `replaceTrackers(list)` runs on every selected torrent.
10. Submenu `"Categor&y"` (icon `view-categories`):
    - `"&New..."` (`list-add`): new-category dialog, then assigns the new category.
    - `"&Reset"` (`edit-clear`): sets the category to "".
    - separator
    - every category (natural, case-insensitive order; `&` escaped). Clicking assigns it. If all selected torrents share one category, that entry is shown checked.
11. Submenu `"Ta&gs"` (icon `tags`):
    - `"&Add..."` (`list-add`): prompt `"Add tags"` / `"Comma-separated tags:"`, split on `,`, each tag validated (error `"Invalid tag"` / `"Tag name: '%1' is invalid"`, re-prompts), then added to all selected torrents. Unknown tags are created.
    - `"&Remove All"` (`edit-clear`): if `Preferences/Advanced/confirmRemoveAllTags` (default true), asks `"Remove All Tags"` / `"Remove all tags from selected torrents?"`. Removes all tags from the selection.
    - separator
    - one **tri-state** checkable entry per existing tag: Checked if all selected have it, PartiallyChecked if some do, Unchecked if none. Clicking cycles Unchecked/Partial → Checked (add tag to all) and Checked → Unchecked (remove from all). The menu stays open while toggling.
12. `"Automatic Torrent Management"`: tri-state, tooltip `"Automatic mode means that various torrent properties (e.g. save path) will be decided by the associated category"`. Enabling asks `"Enable automatic torrent management"` / `"Are you sure you want to enable Automatic Torrent Management for the selected torrent(s)? They may be relocated."` (Yes default). Disabling asks nothing.
13. separator
14. `"Torrent &options..."` (icon `configure`): Torrent Options dialog for the selection.
15. `"Super seeding mode"`: tri-state, **only if every selected torrent is finished and at least one has metadata**. Applies to torrents with metadata.
16. separator
17. `"Pre&view file..."` (icon `view-preview`), if any has metadata. For each selected torrent: if it has previewable files, open the **Preview selection** dialog; otherwise show the error `"Unable to preview"` / `"The selected torrent \"%1\" does not contain previewable files"`.
    - Previewable = MIME type `audio/*` or `video/*` by extension, or an extension in {3GP, AAC, AC3, AIF, AIFC, AIFF, ASF, AU, AVI, FLAC, FLV, M3U, M4A, M4P, M4V, MID, MKV, MOV, MP2, MP3, MP4, MPC, MPE, MPEG, MPG, MPP, OGG, OGM, OGV, QT, RA, RAM, RM, RMV, RMVB, SWA, SWF, TS, VOB, WAV, WMA, WMV}.
    - The dialog is titled `"Preview selection"`, with the label `"The following files from torrent \"%1\" support previewing, please select one of them:"`. Columns are `"Name"`, `"Size"`, `"Progress"`, plus a hidden file index. The OK button is `"Preview"`. If only one file qualifies, it is previewed immediately.
    - Preview flushes the cache, then opens the file with the system default app. If the file does not exist: `"Preview impossible"` / `"Sorry, we can't preview this file: \"%1\"."`.
18. `"Download in sequential order"`: tri-state, if any is unfinished.
19. `"Download first and last pieces first"`: tri-state, if any is unfinished.
20. separator (if 17, 18 or 19 was added)
21. `"Force rec&heck"` (icon `force-recheck`), if any has metadata. If `Preferences/Advanced/confirmTorrentRecheck` (default true), asks `"Recheck confirmation"` / `"Are you sure you want to recheck the selected torrent(s)?"` (Yes default). Calls `force_recheck` on torrents with metadata.
22. `"Force r&eannounce"` (icon `reannounce`): always present. Enabled only if `oneCanForceReannounce`; when disabled its tooltip is `"Can not force reannounce if torrent is Stopped/Queued/Errored/Checking"`. Calls `force_reannounce(0, -1)` plus `force_dht_announce()`.
23. separator
24. `"Open destination &folder"` (icon `directory`). Opens the content path, or the save path if there is no metadata.
    - macOS: Finder reveal of all paths (`activateFileViewerSelectingURLs`).
    - Other OS: for a single-file torrent, open the folder and select the file; else open the folder. De-duplicated.
    - Content path = save location/file for single-file torrents; the root folder for multi-file torrents with a common root; else the save location.
25. Submenu `"&Queue"` (icon `queued`), **only if queueing is enabled and any torrent is unfinished**: `"Move to &top"`, `"Move &up"`, `"Move &down"`, `"Move to &bottom"`.
26. Submenu `"&Copy"` (icon `edit-copy`). Values are joined by `\n`:
    - `"&Name"`: names.
    - `"Info &hash v1"`: enabled only if any has v1.
    - `"Info h&ash v2"`: enabled only if any has v2.
    - `"&Magnet link"`: `magnet:?xt=urn:btih:<v1>[&xt=urn:btmh:1220<v2>][&dn=<pct-enc name if ≠ id>][&xl=<totalSize if metadata>]` + `&tr=<pct-enc>` for each tracker + `&ws=<pct-enc>` for each URL seed.
    - `"Torrent &ID"`: the ID is the v1 hash, or the v2 hash truncated to 20 bytes (libtorrent `get_best()`).
    - `"&Comment"`: non-empty comments joined by `"\n---------\n"`.
    - `"Content &Path"`: non-empty content paths.
27. `"E&xport .torrent..."` (icon `edit-copy`, tooltip `"Exported torrent is not necessarily the same as the imported"`). Opens a directory picker, `"Choose folder to save exported .torrent files"`. Each torrent is written as `<valid file name of torrent name>.torrent`; on collision `"<name> (N).torrent"` with N from 1. Failure logs a warning (`"Export .torrent file failed. Torrent: \"%1\". Save path: \"%2\". Reason: \"%3\""`) and then shows `"Export .torrent file error"` / `"Errors occurred when exporting .torrent files. Check execution log for details."`. Torrents without metadata fail with `"Missing metadata"`.

Other list behaviors:
- Selecting a row (current-index change) loads it into the properties panel and scrolls it into view.

### 1.9 Remove torrent(s) confirmation (`DeletionConfirmationDialog`)

- Shown when `Preferences/Advanced/confirmTorrentDeletion` (default true). Otherwise torrents are removed directly: soft delete keeps content, Shift+Delete removes content, and the sidebar "Remove torrents" keeps content.
- Title `"Remove torrent(s)"`, with a warning icon (`dialog-warning`, large).
- Message for 1 torrent: `"Are you sure you want to remove '%1' from the transfer list?"` (name HTML-escaped). For N torrents: `"Are you sure you want to remove these %1 torrents from the transfer list?"`.
- Checkbox `"Also remove the content files"`, initially checked if invoked with deleteFiles = true (Shift+Delete) **or** `Preferences/General/DeleteTorrentsFilesAsDefault` (default false).
- `"Remember choice"` tool button (lock icon `object-locked`), initially disabled. It is enabled whenever the checkbox state differs from the stored default. Clicking it saves the checkbox state as the new `DeleteTorrentsFilesAsDefault` and disables itself.
- Buttons: `"Remove"` (OK) and Cancel. **Cancel has initial focus.**
- On accept, the current selection (or the visible torrents, for the sidebar action) is re-fetched and removed with `TorrentRemoveOption::RemoveContent` or `KeepContent`. Removing content honors the Advanced setting "Torrent content removing mode" (`TorrentContentRemoveOption`: Delete=0, MoveToTrash=1).
- Remove is ignored if the Transfers tab is not current (for toolbar and keyboard invocations).

### 1.10 macOS-specific integration

- **Dock tile speed badge** (`MacUtils::Badger` / `BadgeView`):
  - Enabled by `Preferences/Desktop/ShowSpeedInDock` (default true). Disabling it clears the badge immediately (rates set to 0).
  - Draws the app icon, then rounded badges 128×30 pt (radius 15) in white bold 26 pt text with a shadow.
  - Download badge: green `#377b2c` (alpha 0.9) with `↓`, at the bottom.
  - Upload badge: blue `#183ed5` (alpha 0.9) with `↑`, stacked above the download badge (2 pt gap) when both are shown.
  - A badge appears only when its rate ≥ 0.1 B/s, i.e. non-zero.
  - Text = `friendlyUnitCompact(rate)`: threshold 1000 (so 1000–1023 roll to the next unit), precision 2 decimals if < 10, 1 if < 100, else 0, with a **single-letter unit** (B/K/M/G/T/P/E), e.g. `"1.25 M"`.
  - Redraws only when values change. Updated on every session stats tick.
- **Menu bar status item** (`MacUtils::StatusItem`):
  - Enabled by `Preferences/Desktop/ShowMacOSMenuBarIcon` (default true).
  - Square status item with the 16×16 template icon `qbittorrent_mac` and tooltip `"qBittorrent"`.
  - Its menu has one disabled info item, `"↓ <dl friendlyUnit speed> · ↑ <ul friendlyUnit speed>"` (e.g. `"↓ 1.2 MiB/s · ↑ 300.0 KiB/s"`), followed by a separator.
  - Updated each stats tick while enabled. Visibility toggles live with the pref.
- **Dock menu**: the desktop-integration menu (1.3.8) installed via `QMenu::setAsDockMenu()`, without Show/Hide/Exit.
- **Dock click**: `applicationShouldHandleReopen:hasVisibleWindows:` is overridden. It emits `activationRequested`, which shows the main window if hidden, asking for the UI-lock password if locked.
- **Notifications**: `UNUserNotificationCenter`. Authorization (alert + sound) is requested at startup; on error it logs the warning `"Permission for notifications not granted. Error: \"%1\""`. Each notification uses a UUID identifier, the title, the body and the default sound.
- **File association helpers** (used by the Options dialog, Behavior page):
  - `isTorrentFileAssocSet()`: is the default app for UTType `.torrent` this bundle?
  - `setTorrentFileAssoc()`: makes this bundle the default app for the `.torrent` content type.
  - `isMagnetLinkAssocSet()` / `setMagnetLinkAssoc()`: the same for the `magnet` URL scheme.
- **File icons**: `pixmapForExtension(ext, size)` returns the Finder icon for a file extension (`NSWorkspace iconForContentType`), used in the content trees.
- **Open files in Finder**: `openFiles(paths)` calls `activateFileViewerSelectingURLs` on a background queue.
- **Dock badge label helpers**: `badgeLabelText()` / `setBadgeLabelText()` exist (`NSApp.dockTile.badgeLabel`) but are unused in 5.2.4.
- **Window menu**: native, see 1.3.5. **Menu roles**: Quit, About, Preferences and Check for Updates move to the application menu.
- **Close semantics**: the close button and ⌘W hide; Quit (⌘Q) exits (1.3.8).
- No tray icon on macOS. `DesktopIntegration::isActive()` is always true there, so close-to-tray and minimize-to-tray prefs do not apply.

---

## 2. Properties panel (bottom pane)

Sources: `src/gui/properties/*`, `src/gui/trackerlist/*`, `src/gui/torrentcontent*.cpp`, `src/gui/previewselectdialog.*`, `src/gui/trackersadditiondialog.*`, `src/base/bittorrent/peerinfo.cpp`, `downloadpriority.h`, `trackerentrystatus.h`, `torrentimpl.cpp`, `abstractfilestorage.cpp`.

The properties panel sits below the torrent list in a vertical `QSplitter` (Transfers tab only). It shows details of the **current** torrent (the one with keyboard focus in the transfer list, `TransferListWidget::currentTorrentChanged` → `PropertiesWidget::loadTorrentInfos`). With no current torrent, every field is cleared.

### 2.1 Common formatting helpers (used throughout this section)

- **Sizes**: `friendlyUnit(bytes)` uses binary units, threshold 1024: `B`, `KiB`, `MiB`, `GiB`, `TiB`, `PiB`, `EiB`. Decimals: B = 0, KiB/MiB = 1, GiB = 2, TiB and above = 3. A non-breaking space goes between number and unit. Negative input gives `Unknown`. Speeds append `/s` (e.g. `1.5 MiB/s`).
- **Durations**: `userFriendlyDuration(seconds, maxCap)`:
  - `< 0` or `>= maxCap` gives `∞` (U+221E; `C_INFINITY`).
  - `0` gives `0`.
  - `< 60` gives `%1s`, or `< 1m` when resolution = Minutes.
  - `< 60 min` gives `%1m`.
  - `< 24 h` gives `%1h %2m`.
  - `< 365 d` gives `%1d %2h`.
  - Otherwise `%1y %2d`.
  - `MAX_ETA = 8640000` s (100 days) is the ETA cap.
- **Dates**: `QLocale().toString(dt, QLocale::ShortFormat)` (locale short date + time).
- **Ratio/Popularity**: 2 decimals; values `>= Torrent::MAX_RATIO` (MAX_RATIO is +infinity) display `∞`.
- `C_UTP` = `μTP` (Greek mu).
- **Keyboard**: `Utils::KeySequence::deleteItem()` is `Cmd+Backspace` on macOS (`Qt::CTRL|Key_Backspace`) and `Delete` elsewhere.
- **Refresh cadence**: the session emits `torrentsUpdated` once per refresh interval (`BitTorrent/Session/RefreshInterval`, default **1500 ms**). If the current torrent is among the updated torrents and the Transfers tab is visible, `PropertiesWidget::loadDynamicData()` runs. It refreshes only the **visible** page:
  - General: all dynamic labels + bars.
  - Peers: peer list reload.
  - Content: file tree progress/priority/availability.
  - Trackers update via their own session signals. HTTP Sources update on selection change.
  - Nothing refreshes while the panel is collapsed.

### 2.2 Tab bar (`proptabbar.cpp`)

A horizontal row of push buttons (not a QTabBar) below the stacked pages, left-aligned, spacing 3. A stretch spacer sits between "Content" and "Speed", so **Speed is right-aligned**. Icons are shown on non-macOS only; on macOS the buttons are text-only.

| Index (`PropertyTab` enum) | Label | Icon (theme id, fallback) | Shortcut |
|---|---|---|---|
| 0 `MainTab` | `General` | `help-about` / `document-properties` | Alt+G |
| 1 `TrackersTab` | `Trackers` | `trackers` / `network-server` | Alt+C |
| 2 `PeersTab` | `Peers` | `peers` | Alt+R |
| 3 `URLSeedsTab` | `HTTP Sources` | `network-server` | Alt+B |
| 4 `FilesTab` | `Content` | `directory` | Alt+Z |
| 5 `SpeedTab` | `Speed` | `chart-line` | Alt+D |

**Collapse/expand behaviour (`setCurrentIndex`)**
- Clicking the **currently selected** tab button, or `setCurrentIndex(-1)`, un-presses it and sets the current index to -1. It then emits `visibilityToggled(false)`, and the panel **collapses**:
  - the stacked pages hide;
  - the splitter handle is hidden and disabled, with handle width 0;
  - the splitter sizes become `[total - tabBarHeight, tabBarHeight]`;
  - the widget's max height is clamped to the tab bar height.
  - The pre-collapse splitter sizes are remembered.
- Clicking any tab while collapsed emits `visibilityToggled(true)`. The panel re-expands, restoring the remembered sizes, handle width and visibility, then forces a `loadDynamicData()`.
- Clicking a different tab un-presses the previous one, presses the new one, and switches the stacked page (`tabChanged(index)`).
- An index greater than or equal to the button count is coerced to 0.

**Persistence** (Preferences, saved on every tab change or visibility toggle and on content header changes)

| Key | Contents | Default |
|---|---|---|
| `TorrentProperties/Visible` (bool) | Whether the panel is expanded | **false** (panel starts collapsed) |
| `TorrentProperties/CurrentTab` (int) | Current tab index | **-1** (none) |
| `TorrentProperties/SplitterSizes` (string) | `"<top>,<bottom>"`; when collapsed, the remembered pre-collapse sizes are saved | — |
| `GUI/Qt6/TorrentProperties/FilesListState` | Content tree header state (column order, widths, hidden columns, sort) | — |
| `GUI/Qt6/TorrentProperties/PeerListState` | Peer list header state | — |
| `GUI/Qt6/TorrentProperties/TrackerListState` | Tracker list header state | — |
| `GUI/PropertiesWidget/FilterPatternFormat` | Content filter pattern format | Wildcards |

Settings are read the first time the main window is shown (`readSettings`).

### 2.3 General tab (`propertieswidget.ui` / `.cpp`)

The page is a scroll area containing, top to bottom:
1. the progress/availability bars block,
2. a horizontal line (`lineBelowBars`),
3. the group box **"Transfer"**,
4. the group box **"Information"**.

All value labels are selectable text. Labels are right-aligned.

#### 2.3.1 Bars block

| Row | Label | Widget | Value label at right |
|---|---|---|---|
| 0 | `Progress:` | `DownloadedPiecesBar` (height 18 px, expanding width) | Torrent progress × 100, 1 decimal, then `%` (e.g. `42.7%`) |
| 1 | `Availability:` | `PieceAvailabilityBar` (height 18 px) | `distributedCopies()` (libtorrent `distributed_copies`) with **3 decimals** |

- The Availability row (label, bar and value) is shown **only** when the torrent has metadata **and** is not finished, not stopped, not queued and not checking. Otherwise it is hidden.
- The line below the bars is hidden only when both bars are hidden.
- The Progress bar and value update only when metadata exists. Piece availability is fetched asynchronously (`piece_availability`). Downloading (partial) pieces are fetched asynchronously (`get_download_queue`).

**Pieces bar rendering (`PiecesBar` base)**
- 1 px border (theme color `PiecesBar.Border`, default palette `Dark`).
- The image is one row of pixels, `width-2` wide, stretched to bar height.
- Background (missing) color: theme `PiecesBar.MissingPiece`, default palette `Base`.
- Piece color: theme `PiecesBar.Piece`, default palette `Highlight`.
- A 256-step gradient table blends background → piece color.
- **Pixel ↔ piece mapping**: the pieces bitfield is resampled to the pixel width with fractional coverage. Each pixel gets a 0..1 value equal to the fraction of covered pieces that are set.
- **Hover highlight**: hovering over a pixel whose piece belongs to exactly **one** file highlights that file's entire byte range. The overlay is palette `Highlight.darker()` at alpha 0.35. Hovering a piece shared by multiple files removes the highlight.
- **Tooltip**: the simple tooltip is an HTML legend of colored swatches, plus a second line:
  - `Hold Shift key for detailed information` when Shift is not held;
  - `Wait until metadata become available to see detailed information` when Shift is held and there is no metadata.
- **Detailed tooltip (Shift held, metadata present)**: a header, then a table of `<size> <file path>` for every file overlapping the piece under the cursor. The header is chosen as follows:
  - `Files in this piece:` when more than one file overlaps;
  - `File in this piece:` when the single file's size equals the piece length;
  - otherwise `File in these pieces:`.
- Colors are recomputed on palette change.

**DownloadedPiecesBar**
- Inputs: completed pieces bitfield + downloading (partial) pieces bitfield.
- Partial color: theme `PiecesBar.PartialPiece`, default = HSL(hue of pure green, saturation and lightness of the piece color).
- **Pixel color**:
  - If the partial value `dl` is greater than 0: `fill = have + dl`, then `mix(piece, partial, dl/fill)`, then mix that over the background by `fill`.
  - Otherwise the gradient color for `have`.
- Legend rows: `Missing pieces` (background), `Partial pieces` (partial color), `Completed pieces` (piece color).

**PieceAvailabilityBar**
- Input: per-piece peer count (`piece_availability`).
- Resampled per pixel, then **normalized by the maximum count in the torrent**, so the most-available piece is full color. If the max is 0, the whole bar is background.
- Legend rows: `Unavailable pieces` (background), `Available pieces` (piece color).

#### 2.3.2 Group "Transfer" (6-column grid: label/value ×3 per row)

| Row/col | Label (exact) | Value / format |
|---|---|---|
| r0 c0 | `Time Active:` (comment: duration torrent is active, not stopped) | If finished: `%1 (seeded for %2)` = (`activeTime`, `finishedTime`), e.g. `4m39s (seeded for 3m10s)`. Otherwise `userFriendlyDuration(activeTime)`. |
| r0 c2 | `ETA:` | `userFriendlyDuration(eta, MAX_ETA)`, so `∞` when ETA ≥ 100 days (see ETA rule below) |
| r0 c4 | `Connections:` | `%1 (%2 max)` = `num_connections`, `connections_limit` (`∞` if < 0) |
| r1 c0 | `Downloaded:` | `%1 (%2 this session)` = all-time download (`all_time_download`), session payload download (`total_payload_download`) |
| r1 c2 | `Uploaded:` | `%1 (%2 this session)` = `all_time_upload`, `total_payload_upload` |
| r1 c4 | `Seeds:` | `%1 (%2 total)` = connected seeds (`num_seeds`), swarm seeds. The swarm count is the tracker scrape `num_complete`, or `list_seeds` if that is -1. |
| r2 c0 | `Download Speed:` | `%1 (%2 avg.)`. The first value is the payload download rate. avg = `all_time_download / (activeTime - finishedTime)`; when the duration is 0 it divides by -1, giving a negative number that displays `Unknown`. |
| r2 c2 | `Upload Speed:` | `%1 (%2 avg.)`. The first value is the payload upload rate. avg = `all_time_upload / activeTime`, with the same 0-duration rule. |
| r2 c4 | `Peers:` | `%1 (%2 total)` = connected leechers (`num_peers - num_seeds`), swarm leechers. The swarm count is `num_incomplete`, or `list_peers - list_seeds` if that is -1. |
| r3 c0 | `Download Limit:` | Per-torrent limit as speed, or `∞` if ≤ 0 |
| r3 c2 | `Upload Limit:` | Same, for upload |
| r3 c4 | `Wasted:` | `total_failed_bytes + total_redundant_bytes` |
| r4 c0 | `Share Ratio:` | `realRatio` with 2 decimals, or `∞`. realRatio = `all_time_upload / D`, where D = `total_done` if `all_time_download < 1%` of `total_done`, else `all_time_download`. If D = 0, the ratio is 0 when upload is also 0, else ∞. |
| r4 c2 | `Reannounce In:` | `userFriendlyDuration(next_announce seconds)` |
| r4 c4 | `Last Seen Complete:` | Short date/time, or `Never` if invalid |
| r5 c0 | `Popularity:` (tooltip on label and value: `Ratio / Time Active (in months), indicates how popular the torrent is`) | `realRatio / activeMonths`, 2 decimals, or `∞`. It is 0 if active months = 0. |

**ETA rule** (`TorrentImpl::eta`)
- Stopped gives `MAX_ETA`.
- **Seeding/finished**: if no effective ratio, seeding-time or inactive-seeding-time limits are set, the result is `MAX_ETA`. Otherwise it is the minimum of:
  - ratio ETA = `(D*ratioLimit - all_time_upload) / avgUpload`, used only if avgUpload > 0;
  - seeding-time ETA = `limitMinutes*60 - finishedTime`, clamped at 0 or above;
  - inactive ETA = `limitMinutes*60 - timeSinceActivity`, clamped at 0 or above.
- **Downloading**: `(wantedSize - completedSize) / avgDownload`; `MAX_ETA` if avgDownload = 0.
- The averages come from a 30-sample rolling payload-rate monitor.

#### 2.3.3 Group "Information"

| Row/col | Label | Value |
|---|---|---|
| r0 c0 | `Total Size:` | `friendlyUnit(totalSize)` (metadata only) |
| r0 c2 | `Pieces:` | `%1 x %2 (have %3)` = piece count, piece size (friendlyUnit), pieces have. Example: `152 x 4.0 MiB (have 25)`. Metadata only. |
| r0 c4 | `Created By:` | Torrent `created by` field (metadata only) |
| r1 c0 | `Added On:` | Short date/time of `addedTime` |
| r1 c2 | `Completed On:` | Short date/time, or empty if not completed |
| r1 c4 | `Created On:` | Torrent creation date, short format (metadata only; blank when the date is 0) |
| r2 c0 | `Private:` (spans 5 cols) | `Yes` / `No` with metadata; `N/A` without metadata |
| r3 c0 | `Info Hash v1:` (span) | 40-hex v1 hash, or `N/A` if the torrent is v2-only |
| r4 c0 | `Info Hash v2:` (span) | 64-hex v2 hash, or `N/A` if v1-only |
| r5 c0 | `Save Path:` (span) | `savePath()` native string; updates live on `torrentSavePathChanged` |
| r6 c0 | `Comment:` (span; rich text, word-wrapped, links open externally) | Comment, HTML-escaped, then passed through `parseHtmlLinks` |

**`parseHtmlLinks` (comment linkification)**
- The text is HTML-escaped first. Then a regex wraps URLs as `<a href="\2">\2</a>`. A match must be preceded by whitespace or line start. It recognizes three forms:
  - (1) `http(s)://` + `domain.`+ + `[a-zA-Z0-9?%=&/_.:#;-]+`;
  - (2a) a schemeless `domain.`+ followed by one of these TLDs (either case): `aero arpa asia biz cat com coop edu gov info int jobs mil mobi museum name net org pro ro ru tel travel`;
  - (2b) two or more `domain.` parts + a 2+ letter TLD + an optional path.
- Hrefs without a scheme get an `http://` prefix.
- The result is wrapped in `<p style="white-space: pre-wrap;">…</p>`, which preserves newlines and spaces.

### 2.4 Trackers tab (`trackerlist/*`)

Layout: a `TrackerListWidget` (QTreeView) on the left. On the right is a vertical column with two small icon buttons (`go-up` and `go-down`, no text) between spacers:
- **Up** = `decreaseSelectedTrackerTiers`: tier − 1 for each selected tracker, floor 0.
- **Down** = `increaseSelectedTrackerTiers`: tier + 1, saturating at INT_MAX.

Both rewrite the full tracker list via `replaceTrackers`. This is the **only tier-editing UI**; there is no drag-and-drop in the tracker list.

#### 2.4.1 Columns (`TrackerListColumn` enum order)

| # | Header (exact) | Display | Sort role value | Alignment |
|---|---|---|---|---|
| 0 `COL_URL` | `URL/Announce Endpoint` | Tracker URL; for an endpoint child row, the local endpoint name (`ip:port` of the local listen socket) | Name | Left |
| 1 `COL_TIER` | `Tier` | Tier number; blank for sticky rows and endpoint rows | Tier (-1 for endpoints) | Right |
| 2 `COL_PROTOCOL` | `BT Protocol` | Endpoint rows: `v1` / `v2`; blank for tracker rows | btVersion (-1 for trackers) | Right. **Hidden by default** in libtorrent-2 builds (set before restoring header state). |
| 3 `COL_STATUS` | `Status` | Status text (see below) | Status text | Left |
| 4 `COL_PEERS` | `Peers` | Count or `N/A` (-1) | int | Right |
| 5 `COL_SEEDS` | `Seeds` | Scrape `complete`, or `N/A` | int | Right |
| 6 `COL_LEECHES` | `Leeches` | Scrape `incomplete`, or `N/A` | int | Right |
| 7 `COL_TIMES_DOWNLOADED` | `Times Downloaded` | Scrape `downloaded`, or `N/A` | int | Right |
| 8 `COL_MSG` | `Message` | Tracker message or error text | string | Left |
| 9 `COL_NEXT_ANNOUNCE` | `Next Announce` | `userFriendlyDuration(secs, -1, Seconds)`, clamped at 0 or above | secs | Right |
| 10 `COL_MIN_ANNOUNCE` | `Min Announce` | Same, for min announce | secs | Right |

- Display and tooltip show the same text.
- Header text uses elide-right and the last section does not stretch.
- Columns are movable, including the first.
- Default sort is column 0 ascending. Sorting is case-insensitive on the sort role.
- Sticky rows (DHT/PeX/LSD) are intended to stay pinned at the top: the comparator only orders them among themselves, in ascending order.
- Shift + mouse wheel scrolls horizontally.
- Announce countdowns are recomputed from a timestamp refreshed by a single-shot timer every **4 s** (`ANNOUNCE_TIME_REFRESH_INTERVAL`).

#### 2.4.2 Tracker status (`TrackerEndpointState`, int values)

| Enum | Int | Status text |
|---|---|---|
| `NotContacted` | 1 | `Not contacted yet` |
| `Working` | 2 | `Working` |
| `NotWorking` | 4 | `Not working` |
| `TrackerError` | 5 | `Tracker error` |
| `Unreachable` | 6 | `Unreachable` |
| (`isUpdating` flag) | — | `Updating...` (takes precedence over the state) |
| unknown | — | `Invalid state!` |

**Endpoint state derivation** (per local endpoint × BT protocol version v1/v2)
- If updating, the endpoint shows `Updating...`.
- Else if `fails > 0`:
  - `TrackerError` if the last error is `tracker_failure`;
  - `Unreachable` if it is `announce_skipped`;
  - otherwise `NotWorking`.
- Else `Working` if the entry is verified, otherwise `NotContacted`.
- The endpoint message is the tracker `message`, else the text of `last_error`, else empty.

**Tracker-entry aggregation** (only when it has at least 1 endpoint)
- If any endpoint is updating, the entry shows `Updating...`. Otherwise its state is the first match of:
  - `Working` if any endpoint is working;
  - `TrackerError` if any endpoint has a tracker error;
  - `Unreachable` if all endpoints are unreachable;
  - `NotWorking` if all are unreachable or not working.
- Peers, Seeds, Leeches and Times Downloaded are the **max** across endpoints.
- Next/Min announce and the message come from the endpoint with the **earliest** next announce among endpoints whose state equals the aggregated state.
- For Working entries, a non-empty endpoint message fills the entry message if it is empty.

**Endpoint child rows**
- Each tracker row is expandable; it does not expand on double-click.
- It has one child per (local listen endpoint, BT version), named by the endpoint (`ip:port`).
- When a tracker row is **expanded**, the delegate blanks its aggregated Peers, Seeds, Leeches, Times Downloaded, Message, Next Announce and Min Announce cells, because the children show the per-endpoint values.

#### 2.4.3 Sticky rows (always rows 0–2, grey text `QColorConstants::Svg::grey`)

| Row | URL text | Status |
|---|---|---|
| 0 `ROW_DHT` | `** [DHT] **` | `Disabled` if session DHT is off; `Disabled for this torrent` if the torrent is private or has DHT disabled per-torrent; else `Working` |
| 1 `ROW_PEX` | `** [PeX] **` | Same logic, with session PeX and per-torrent PeX disabled |
| 2 `ROW_LSD` | `** [LSD] **` | Same logic, with session LSD and per-torrent LSD disabled |

- **Message**: `This torrent is private` for private torrents, else empty.
- **Seeds / Leeches**: counted once when the torrent is selected (not live), from the peer list. Peers still `connecting` are skipped. A peer is counted per source flag (from DHT / from PeX / from LSD) as a seed or as a leecher.
- **Peers** and **Times Downloaded** are `N/A`. Tier is blank. Next/Min Announce show `0`.

#### 2.4.4 Context menu (right-click on list)

The menu items, in order:
1. `Add trackers...` (icon `list-add`): always present. Opens the "Add trackers" dialog.
2. The following appear **only if at least one real tracker row is selected**. Sticky rows and endpoint children are excluded from "selected trackers".
   - `Edit tracker URL...` (icon `edit-rename`)
   - `Remove tracker` (icon `edit-clear` / `list-remove`)
   - `Copy tracker URL` (icon `edit-copy`): newline-joined URLs.
   - `Force reannounce to selected trackers` (icon `reannounce` / `view-refresh`): only if the torrent is **not stopped**. Forces a reannounce to each selected tracker by index. If the DHT sticky row is in the selection, it forces a DHT announce.
3. A separator, then `Force reannounce to all trackers` (same icon): only if the torrent is not stopped. Calls `forceReannounce()` (all trackers) plus `forceDHTAnnounce()`.

**Keyboard and mouse**
- `F2` and double-click: edit the selected tracker. With multi-select, only the last selected one is edited.
- `deleteItem` key: remove the selected trackers.
- `Ctrl/Cmd+C`: copy the URLs.

**Edit tracker dialog** (`AutoExpandableDialog::getText`)
- Title `Tracker editing`, label `Tracker URL:`, prefilled with the current URL. The input is trimmed.
- Errors (both titled `Tracker editing failed`):
  - `The tracker URL entered is invalid.` when the QUrl is invalid;
  - `The tracker URL already exists.` when the URL matches another tracker.
- Unchanged input is a no-op. Otherwise the URL is replaced **in place**, keeping its tier, via `replaceTrackers`.

**Header context menu (column visibility)**
- Title `Column visibility`. One checkable item per column, using the header text.
- Unchecking the last visible column is refused.
- Showing a column narrower than 5 px auto-resizes it.
- After a separator: `Resize columns`, with tooltip `Resize all non-hidden columns to the size of their contents`.
- The state is saved on any move, resize or sort change.
- If no column is visible on load, URL is forced visible. Zero-width visible columns are auto-resized.

#### 2.4.5 "Add trackers" dialog (`trackersadditiondialog.ui`)

- Window title `Add trackers`. The OK button is relabelled `Add`.
- Label `List of trackers to add (one per line):` above a plain-text edit.
- `µTorrent compatible list URL:` line edit + button `Download trackers list` (icon `downloading`).
- **Download**: an empty URL warns `Trackers list URL error` / `The trackers list URL cannot be empty`. During the download the button is disabled and a wait cursor is shown. The download uses the "proxy for general purposes" preference.
  - On failure: `Download trackers list error` / `Error occurred when downloading the trackers list. Reason: "%1"`.
  - On success: the UTF-8 body (trimmed) is appended to the text box, with a newline inserted first if needed.
- **Parsing** (`parseTrackerEntries`):
  - Split on `\n` and trim each line.
  - **Blank lines increment the tier** (tier starts at 0 and saturates at INT_MAX).
  - Each URL's tier is offset by `baseTier = (tier of last existing tracker) + 1`, or 0 if the torrent has none (clamping add).
  - Then `addTrackers` runs; duplicates are ignored by the core.
- Persisted: `AddTrackersDialog/Size`, `AddTrackersDialog/TrackersListURL`.

### 2.5 Peers tab (`peerlistwidget.cpp`, `peerlistsortmodel.cpp`, `peerinfo.cpp`)

A flat QTreeView with:
- no root decoration;
- uniform rows;
- extended selection;
- no editing;
- movable columns (including the first);
- no last-section stretch;
- elided headers;
- Shift+wheel horizontal scroll.

#### 2.5.1 Columns (`PeerListColumns` order)

| # | Header (exact) | Display | Sort key | Align |
|---|---|---|---|---|
| 0 `COUNTRY` | `Country/Region` | Flag icon only (`UIThemeManager::getFlagIcon(countryCode)`); tooltip = full country name (`GeoIPManager::CountryName`). The country comes from a GeoIP (DB-IP) lookup of the peer IP. | **Tooltip text** (country name); sort role switches to ToolTipRole when this column is the sort column | — |
| 1 `IP` | `IP/Address` | Peer IP string. For I2P peers, the base32 destination `xxxx.b32.i2p`. If host-name resolution is enabled and resolved, the host name, with the IP as tooltip. | Natural (numeric-aware) string compare | Left |
| 2 `PORT` | `Port` | Port number; `N/A` for I2P | int | Right |
| 3 `CONNECTION` | `Connection` | `μTP` if the uTP socket flag is set; `BT` for standard BitTorrent connections; `Web` for any web/HTTP seed connection | string | — |
| 4 `FLAGS` | `Flags` | Space-separated flag letters (see 2.5.2); tooltip = one `X = meaning` per line | string | — |
| 5 `CLIENT` | `Client` (i.e. client application) | libtorrent client string, simplified (non-printables removed), HTML-escaped; tooltip the same | Natural compare | — |
| 6 `PEERID_CLIENT` | `Peer ID Client` | First 8 bytes of the peer ID as ASCII. Empty if the peer ID is all zeros (unknown yet). `Unknown` if any of the 8 bytes is non-printable (<32 or >126). **Hidden by default** (when there is no saved header state). | string | — |
| 7 `PROGRESS` | `Progress` (% downloaded) | Peer progress × 100, 1 decimal, then `%` | real | Right |
| 8 `DOWN_SPEED` | `Down Speed` | Payload download speed (`/s`) | int | Right |
| 9 `UP_SPEED` | `Up Speed` | Payload upload speed | int | Right |
| 10 `TOT_DOWN` | `Downloaded` | Total downloaded from the peer | int | Right |
| 11 `TOT_UP` | `Uploaded` | Total uploaded to the peer | int | Right |
| 12 `RELEVANCE` | `Relevance` (how many pieces it has that we don't) | Relevance × 100, 1 decimal, then `%` | real | Right |
| 13 `DOWNLOADING_PIECE` | `Files` (files being downloaded right now) | Files spanned by the piece currently downloading from this peer, joined by `;`; tooltip joined by newlines | string | — |
| 14 `IP_HIDDEN` | (always hidden) | Raw IP, used for copy/ban | — | — |

**Value rules**
- **Relevance** = (pieces the peer has that we lack) / (our missing piece count). It is 0 if we are missing nothing.
- **Hide zero values**: when Options › Behavior `Hide zero and infinity values` is on **and** its combo is index 0 (`Always`), the four speed and total columns show blank for values ≤ 0. Combo index 1 is `Stopped torrents only`, which does not apply here.

**Country column**
- Visible only when Advanced setting `Resolve peer countries` is on (`Preferences/Connection/ResolvePeerCountries`, default **true**). Otherwise the column is hidden and omitted from the column menu.
- Toggling the setting live shows or hides the column and reloads the peers.

**Host name resolution**
- Advanced `Resolve peer host names` (`Preferences/Connection/ResolvePeerHostNames`, default **false**).
- When on, reverse DNS (`Net::ReverseResolution`) runs asynchronously. Resolved names replace the IP column text for every row with that IP.

**Row identity and refresh**
- A row is keyed by (IP address, port, connection type string). Rows are updated in place.
- Rows for peers that are gone are removed. New peers are appended.
- I2P peers are fully removed and re-added on every refresh.
- The refresh is asynchronous (`fetchPeerInfo`) and occurs only while the Peers tab is visible.

#### 2.5.2 Peer flags (`PeerInfo::determineFlags`, in this exact order)

Each set flag appends `"<letter> "` to the flags string and `"<letter> = <text>\n"` to the tooltip. The trailing char is chopped.

| Letter | Condition (libtorrent `peer_info` flags/source) | Tooltip text (exact) |
|---|---|---|
| `d` | `interesting` and `remote_choked` | `Interested (local) and choked (peer)` |
| `D` | `interesting` and not `remote_choked` | `Interested (local) and unchoked (peer)` |
| `u` | `remote_interested` and `choked` | `Interested (peer) and choked (local)` |
| `U` | `remote_interested` and not `choked` | `Interested (peer) and unchoked (local)` |
| `K` | not `remote_choked` and not `interesting` | `Not interested (local) and unchoked (peer)` |
| `?` | not `choked` and not `remote_interested` | `Not interested (peer) and unchoked (local)` |
| `O` | `optimistic_unchoke` | `Optimistic unchoke` |
| `S` | `snubbed` | `Peer snubbed` |
| `I` | not `local_connection` (i.e. incoming) | `Incoming connection` |
| `H` | source & `dht` | `Peer from DHT` |
| `X` | source & `pex` | `Peer from PEX` |
| `L` | source & `lsd` | `Peer from LSD` |
| `E` | `rc4_encrypted` | `Encrypted traffic` |
| `e` | `plaintext_encrypted` | `Encrypted handshake` |
| `P` | `utp_socket` | `μTP` |
| `h` | `holepunched` | `Peer is using NAT hole punching` |

Other PeerInfo accessors exist that are not shown as flags: `isSeed`, `isConnecting`, `isOnParole`, `isUploadOnly`, `isEndgameMode`, `useSSLSocket`, `isHandshake`, `isSupportsExtensions`. The WebAPI exposes the same flags string and description.

#### 2.5.3 Context menu (peer rows)

Tooltips are visible on disabled items. Items in order:
1. `Add peers...` (icon `peers-add`). Disabled, with tooltip, when:
   - private: `Cannot add peers to a private torrent`;
   - checking: `Cannot add peers when the torrent is checking`;
   - queued: `Cannot add peers when the torrent is queued`.

   It opens the Add Peers dialog and calls `connectPeer` for each address. Afterwards:
   - if any failed: info box `Adding peers` / `Some peers cannot be added. Check the Log for details.`;
   - else if more than 0 were added: `Adding peers` / `Peers are added to this torrent.`
   - Log lines: `Peer "%1" is added to torrent "%2"` (info) or `Failed to add peer "%1" to torrent "%2". Reason: %3` (warning).
2. `Copy IP:port` (icon `edit-copy`). Copies newline-joined `ip:port`; IPv6 is formatted as `[ip]:port` (detected by the absence of `.`). Hotkey Copy (Ctrl/Cmd+C).
3. A separator, then `Ban peer permanently` (icon `peers-remove`).
   - It first asks a question titled `Ban peer permanently`: `Are you sure you want to permanently ban the selected peers?`.
   - On Yes, it calls `Session::banIP(ip)` for each peer and logs `Peer "%1" is manually banned`, then reloads the list.
   - Hotkey `deleteItem`.

Both Copy and Ban are disabled when nothing is selected (tooltip `No peer was selected`).

**Header menu**: `Column visibility` as in Trackers (Country is omitted when country resolution is off), plus `Resize columns`.

#### 2.5.4 "Add Peers" dialog (`peersadditiondialog.ui`)

- Title `Add Peers`. Label `List of peers to add (one IP per line):`, a plain-text edit, and a hint `Format: IPv4:port / [IPv6]:port`.
- On OK:
  - Empty input warns `No peer entered` / `Please type at least one peer.`
  - Each line (split on `\n` after an overall trim) is parsed by `PeerAddress::parse`:
    - `[v6]:port` is IPv6;
    - otherwise the line must contain `:` and split into exactly 2 parts for IPv4;
    - the IP must parse and the port must be a non-zero ushort.
  - Any invalid line warns `Invalid peer` / `The peer '%1' is invalid.` and the whole list is discarded; the dialog stays open.

### 2.6 HTTP Sources tab (web seeds)

- A `QListWidget` of URL-seed URLs (BEP19 web seeds; libtorrent `url_seeds`), with extended selection. It is loaded asynchronously (`fetchURLSeeds`) when the torrent is selected.
- **Context menu**:
  - `Add web seed...` (icon `list-add`): always present.
  - If any rows are selected, these follow:
    - `Remove web seed` (icon `edit-clear` / `list-remove`)
    - a separator
    - `Copy web seed URL` (icon `edit-copy`): newline-joined
    - `Edit web seed URL...` (icon `edit-rename`)
- **Add**: `AutoExpandableDialog` titled `Add web seed` (context "Add HTTP source"), label `Add web seed:`, prefilled `http://www.`.
  - A duplicate (exact, case-insensitive fixed-string match) warns `qBittorrent` / `This web seed is already in the list.`
  - Otherwise it calls `addUrlSeeds({url})` and reloads. There is no URL syntax validation.
- **Edit**: requires exactly one selected item; triggered by `F2` or double-click.
  - Dialog title `Web seed editing`, label `Web seed URL:`.
  - Duplicate check as above. Quirk: submitting the unchanged URL also triggers the "already in the list" warning.
  - Otherwise removes the old seed, adds the new one and reloads.
- **Remove**: `deleteItem` key or menu. Removes the selected seeds without confirmation.

### 2.7 Content tab (file tree; `torrentcontentwidget.cpp`, `torrentcontentmodel*.cpp`, `torrentcontentfiltermodel.cpp`, `torrentcontentitemdelegate.cpp`)

**Toolbar row** above the tree:
- `Select All` button: sets each **top-level** item checked, which cascades to Normal priority for everything.
- `Select None` button: sets each top-level item unchecked, giving Ignored.
- A spacer.
- A filter `LineEdit`, fixed width 300, placeholder `Filter files...`. Text changes are debounced by **400 ms**.

**Filter context menu**: the standard edit menu + a separator + a `Pattern Format` submenu with radio items `Plain text`, `Wildcards`, `Regular expression`.
- `FilterPatternFormat` enum: PlainText = 0, Wildcards = 1, Regex = 2. The default is **Wildcards**, persisted in `GUI/PropertiesWidget/FilterPatternFormat`.
- **Plain text**: case-insensitive fixed substring.
- **Wildcards**: converted with `QRegularExpression::wildcardToRegularExpression(pattern, UnanchoredWildcardConversion | NonPathWildcardConversion)`, case-insensitive. So `*` and `?` match anywhere, including `/`, and the match is unanchored.
- **Regex**: case-insensitive.
- The filter matches against the item **name** only, not the full path.
  - Files are shown if their name matches.
  - Folders are shown if the folder name matches **or** any descendant file or folder name matches (recursive).
- A non-empty filter expands everything. Clearing it collapses everything and then expands only the first top-level row.

**Tree view properties**
- Sortable; default sort is column 0 ascending; movable header sections.
- Uniform rows; extended selection; all columns show focus.
- Edit triggers: **AllEditTriggers**. In addition, a single **click** on any cell calls `edit()`, so one click on the Priority cell opens the priority combo.
- Double-click does not expand or collapse folders.
- On model reset or torrent change, `expandRecursively` expands down while each level has exactly one child (e.g. single root folder → auto-expanded).
- Shift+wheel scrolls horizontally.
- The file tree is built only once the torrent has metadata; magnets show an empty tree until metadata arrives, then it reloads.

#### 2.7.1 Columns (`TreeItemColumns`)

| # | Header (exact) | Display | Underlying (sort) value | Notes |
|---|---|---|---|---|
| 0 `COL_NAME` | `Name` | File or folder name with icon; **checkbox** | name; **natural sort**, and folders always sort before files in ascending order | Folder icon = system folder icon. File icon by extension (macOS: `NSWorkspace` icon for the extension, 32×32, cached per extension). |
| 1 `COL_SIZE` | `Total Size` | `friendlyUnit(size)` | bytes | Right-aligned; folder = sum of all descendant file sizes (including ignored) |
| 2 `COL_PROGRESS` | `Progress` | Drawn as a **progress bar** with text `100%` when ≥ 1, else 1 decimal + `%` | progress×100 | The bar is drawn disabled (greyed) when the priority is Ignored. Zero-size items report progress 1. |
| 3 `COL_PRIO` | `Download Priority` | Priority text; **editable** with a combo box | int priority | See enum below |
| 4 `COL_REMAINING` | `Remaining` | `friendlyUnit(remaining)` | bytes | Right-aligned; 0 for Ignored items; file remaining = `size × (1 - progress)` |
| 5 `COL_AVAILABILITY` | `Availability` | `100%`, a 1-decimal percentage, or `N/A` when < 0 | fraction | File availability = fraction of the file's pieces with availability > 0. All files are -1 (`N/A`) when libtorrent returns an empty availability array (seeding). A file spanning no pieces = 1. |

**Folder aggregation**
- Recomputed on each refresh. Children with priority **Ignored are excluded**.
- Folder progress = Σ(child progress × child size) / Σ size. It is 1.0 with remaining 0 if the total size is 0.
- Folder remaining = Σ child remaining.
- Folder availability = size-weighted mean over children with availability ≥ 0. It is -1 (`N/A`) if no child has data or the total size is 0.

**Column menu**
- `Column visibility` with one checkable item per column. **Name cannot be hidden** (its item is disabled).
- Then a separator and `Resize columns`.
- Header changes are persisted in `GUI/Qt6/TorrentProperties/FilesListState`.

#### 2.7.2 Priorities (`BitTorrent::DownloadPriority`, int values)

| Enum | Int | Display text | Combo index |
|---|---|---|---|
| `Ignored` | **0** | `Do not download` | 0 |
| `Normal` | **1** | `Normal` | 1 |
| `High` | **6** | `High` | 2 |
| `Maximum` | **7** | `Maximum` | 3 |
| `Mixed` | **-1** | `Mixed` (folders only; the combo item appears only when the current value is Mixed) | 4 |

- Values other than 0/1/6/7 display as `Normal` in this model. The WebAPI file priority uses the same ints.
- Changing the combo commits immediately.

**Folder priority**
- A folder's priority is the common priority of its children, or `Mixed` if they differ.
- Setting a folder to a non-Mixed priority pushes that priority recursively to all children. Parents are updated upward.

**Checkbox (Name column, tristate on folders via `ItemIsAutoTristate`)**
- Displayed state:
  - Unchecked if the priority is Ignored.
  - Checked for any non-Ignored, non-Mixed priority.
  - For Mixed folders: **PartiallyChecked** if any child is Ignored or Mixed, otherwise **Checked** (e.g. a mix of Normal and High shows fully checked).
- Setting state:
  - Checked sets Normal (1);
  - Unchecked sets Ignored (0);
  - PartiallyChecked sets Mixed (no-op on children).
- **Space / Select key**: toggles all selected rows together. If the current row is Checked, all become Unchecked; otherwise all become Checked.

Every priority change calls `prioritizeFiles(full list)` on the torrent. The core then handles the `.unwanted` folder moves and `.!qB` naming (`makeActualPath`).

#### 2.7.3 Context menu (file rows)

Shown only when at least one row is selected. The menu closes automatically if the model resets.

**Single selection**
- `Open` (icon `folder-documents`), and `Open containing folder` (icon `directory`). These appear only if the torrent has an actual storage location.
  - **Open**: flushes the disk cache, then opens the full path with the OS default app. For a file the path is the actual file path, including any `.!qB` or `.unwanted` component. For a folder it is storage location / relative folder path.
  - **Open containing folder**: on macOS, reveals the item in Finder (`MacUtils::openFiles`). Elsewhere it opens the parent folder with the item selected.
- `Rename...` (icon `edit-rename`)
- A separator.
- Submenu `Priority`, containing:
  - `Do not download`
  - `Normal`
  - `High`
  - `Maximum`
  - a separator
  - `By shown file order`

**Multiple selection**
- `Do not download`
- `Normal priority`
- `High priority`
- `Maximum priority`
- a separator
- `Priority by shown file order`

**"By shown file order"**
- Splits the selected rows, in their **current visual (sorted/filtered) order**, into 3 groups of size `max(n/3, 1)` (integer division).
- Group 0 gets Maximum (7), group 1 gets High (6), and all remaining rows get Normal (1).

**Keys and double-click**
- `F2`: rename (exactly one row selected).
- `Enter`/`Return`: open the selected file (exactly one row).
- **Double-click** = **Open** in the Properties panel. The widget's built-in default is Rename, which the Add-torrent dialog uses. Nothing happens without metadata.

**Drag & drop**
- Drag mode DragOnly. Content drag exports `text/uri-list` of `file://` URLs for the files and folders at their actual paths.
- Whether a plain drag starts dragging is set by Options › Behavior `Drag content from qBittorrent`, with tooltip `Replaces multi-selection (hold Alt key to invert)` (`Preferences/General/TorrentContentDragEnabled`, default **false**).
- Holding **Alt** inverts that setting for the current press.

#### 2.7.4 Rename semantics

**Dialog**
- `AutoExpandableDialog` titled `Renaming`, label `New name:`, prefilled with the current name. The input is trimmed.
- For **files** the base name is pre-selected, excluding the extension. The extension is the MIME-database suffix if known (e.g. `.tar.gz`), else from the last `.`.
- For folders the whole text is selected.

**Validation** (`Utils::Fs::isValidName`, non-Windows rules), applied when the name changed. Rejected names:
- an empty name;
- `.` or `..`;
- a name longer than 255 UTF-8 bytes;
- a name containing a control char (<32), DEL (127) or `/`.

Failure shows a warning box `Rename error` with `The name is invalid: "%1"`.

**File rename**: `renameFile(old relative path, new relative path)`. It throws, with the message shown in the `Rename error` box, on:
- `The old path is invalid: '%1'.`
- `The new path is invalid: '%1'.`
- `Absolute path isn't allowed: '%1'.`
- `The file already exists: '%1'.` (another file in the torrent already has that path)
- `No such file: '%1'.`

**Folder rename**: renames every file under the folder prefix. Errors (each ending with `'%1'.`):
- the same invalid/absolute checks as for files;
- `The folder already exists: '%1'.` (any file already under the new prefix, compared **case-sensitively** so a case-only rename works);
- `No such folder: '%1'.`

**Actual on-disk name**: computed by `makeActualPath`.
- Append `.!qB` if "Append .!qB extension" is enabled, the file size > 0, and the file is incomplete.
- Insert a `.unwanted/` parent if the unwanted-folder option is on and the priority is Ignored.

**Async failure**: logged as `File rename failed. Torrent: "%1", file: "%2", reason: "%3"`.

### 2.8 Speed tab (`speedwidget.cpp`, `speedplotview.cpp`)

**Speed graphs disabled**
- Shown when Advanced setting `Enable speed graphs` is off (`SpeedWidget/Enabled`, default **true**).
- The tab then shows a centered label: bold `Speed graphs are disabled`, followed by the paragraph `You can enable it in Advanced Options`.
- The widget is swapped live when preferences change.

**Controls** (top row)
- Bold label `Period:`, followed by a combo box. The combo's `TimePeriod` values:

  | Int | Text |
  |---|---|
  | 0 `MIN1` | `1 Minute` |
  | 1 `MIN5` | `5 Minutes` |
  | 2 `MIN30` | `30 Minutes` |
  | 3 `HOUR3` | `3 Hours` |
  | 4 `HOUR6` | `6 Hours` |
  | 5 `HOUR12` | `12 Hours` |
  | 6 `HOUR24` | `24 Hours` |

  Default period index **1** (5 Minutes), stored as `SpeedWidget/period`.
- Right-aligned: a combo-looking button with the single item `Select Graphs`. It pops a menu of 10 checkable graph actions.

**Series** (`GraphID` int; pen color/style)
- Upload series are blue `rgb(50,153,255)`; download series are green `rgb(134,196,63)`.
- All pens are 1.5 px wide.

| ID | Name | Data source (session status) | Line style | Default enabled |
|---|---|---|---|---|
| 0 `UP` | `Total Upload` | `uploadRate` (all upload incl. overhead) | solid | **yes** |
| 1 `DOWN` | `Total Download` | `downloadRate` | solid | **yes** |
| 2 `PAYLOAD_UP` | `Payload Upload` | `payloadUploadRate` | dash | no |
| 3 `PAYLOAD_DOWN` | `Payload Download` | `payloadDownloadRate` | dash | no |
| 4 `OVERHEAD_UP` | `Overhead Upload` | `ipOverheadUploadRate` | dash-dot | no |
| 5 `OVERHEAD_DOWN` | `Overhead Download` | `ipOverheadDownloadRate` | dash-dot | no |
| 6 `DHT_UP` | `DHT Upload` | `dhtUploadRate` | dash-dot-dot | no |
| 7 `DHT_DOWN` | `DHT Download` | `dhtDownloadRate` | dash-dot-dot | no |
| 8 `TRACKER_UP` | `Tracker Upload` | `trackerUploadRate` | dot | no |
| 9 `TRACKER_DOWN` | `Tracker Download` | `trackerDownloadRate` | dot | no |

- Enabled state is persisted per graph as `SpeedWidget/graph_enable_<id>`. The default is true only for ids 0 and 1.
- The period and graph state are saved when the widget is destroyed.

**Sampling**
- One sample is pushed on every session `statsUpdated`, i.e. every refresh interval (default 1500 ms).
- The sample is fed into **5 averagers**, each a circular buffer of capacity duration/resolution:

  | Averager | Window | Resolution | Used by periods |
  |---|---|---|---|
  | 5-minute | 5 min | 1 s | 1 Minute, 5 Minutes |
  | 30-minute | 30 min | 6 s | 30 Minutes |
  | 6-hour | 6 h | 36 s | 3 Hours, 6 Hours |
  | 12-hour | 12 h | 72 s | 12 Hours |
  | 24-hour | 24 h | 144 s | 24 Hours |

- Each averager accumulates samples until the elapsed time reaches its resolution, then emits the mean, stamped with the elapsed duration.
- The elapsed time is capped at `max(1.25 × refreshInterval, resolution)` to survive system sleep.
- Old points are dropped once the window is exceeded. All averagers run continuously, regardless of the selected period.

**Rendering**
- Y-axis maximum = the max value of the **enabled** series within the visible period, rounded to a "nice" scale:
  - 0 stays 0;
  - a value ≤ 12 becomes 12 B;
  - otherwise the unit is scaled by 1024 until the value is ≤ 1024, then:
    - a value > 100 is rounded up to a multiple of 40;
    - a value > 10 is rounded up to a multiple of 4;
    - otherwise it is rounded up to the first of {1.2, 1.6, 2, 2.4, 2.8, 3.2, 4, 6, 8}, else 10.
- Five Y labels: 100%, 75%, 50%, 25% and 0% of the scale. Each is formatted as a number plus a speed unit; the number has unit precision when below 10, else 0 decimals.
- Dashed grey grid `rgba(128,128,128,128)`: 5 horizontal lines and 6 vertical time divisions. There are no time labels.
- Antialiased polylines, newest point at the right edge.
- Legend at the top-left: enabled series only, each a line sample in its pen plus its name, on a 50%-transparent background.

### 2.9 Preview selection dialog (`previewselectdialog.*`)

**Opening**
- Opened by the transfer-list action "Preview file..." or by the double-click action "Preview" when the torrent contains previewable files.
- Otherwise an error appears: `Unable to preview` / `The selected torrent "%1" does not contain previewable files`.

**Previewable files** (`Utils::Misc::isPreviewable`): a file qualifies if either:
- its MIME type, by extension, starts with `audio` or `video`; or
- its uppercase extension is in this list: `.3GP .AAC .AC3 .AIF .AIFC .AIFF .ASF .AU .AVI .FLAC .FLV .M3U .M4A .M4P .M4V .MID .MKV .MOV .MP2 .MP3 .MP4 .MPC .MPE .MPEG .MPG .MPP .OGG .OGM .OGV .QT .RA .RAM .RM .RMV .RMVB .SWA .SWF .TS .VOB .WAV .WMA .WMV`.

**Dialog layout**
- Title `Preview selection`.
- Label `The following files from torrent "%1" support previewing, please select one of them:`. The OK button is relabelled `Preview`.
- Columns: `Name` (file name only, not the path), `Size` (friendly; sorts by bytes), `Progress` (progress-bar delegate, `100%` or 1 decimal), plus a hidden file-index column.
- Rows are sorted by name and the first row is pre-selected.
- Alternating row colors follow the preference. The header context menu offers `Resize columns`.
- On first show, the Name column is set to 60% of the view width.

**Behaviour**
- **If there is ≤ 1 previewable file, the dialog auto-triggers Preview immediately on show.**
- Preview (button or double-click) flushes the cache and builds `actualStorageLocation / actualFilePath(index)`.
- If that path does not exist, a critical box appears: `Preview impossible` / `Sorry, we can't preview this file: "%1".` In the single-file case the box is parented to the main window and the dialog is rejected.
- Otherwise the file is opened with the OS default application (`Utils::Gui::openPath`) through a queued connection, and the dialog is accepted.
- Persisted: `PreviewSelectDialog/Size` and `GUI/Qt6/PreviewSelectDialog/HeaderState`.

---

## 3. Dialogs

Source of truth: `src/gui/*.ui`, `src/gui/*.cpp`, `src/gui/{rss,search,properties,trackerlist,log}/*`, `src/app/legalnotice.cpp`. All UI strings below are quoted verbatim (Qt `tr()` source strings, English). `&` in a label marks the mnemonic (Alt-key accelerator) and is not shown.

### 3.0 Common conventions used by many dialogs

- **Persisted dialog geometry.** Almost every dialog stores its size on close and restores it on open under a `SettingsStorage` key `<DialogName>/Size` (or `DialogSize`). Tree/table header states are stored under `GUI/Qt6/<DialogName>/...HeaderState`. List of keys used: `AddNewTorrentDialog/DialogSize`, `AddNewTorrentDialog/DefaultCategory`, `AddNewTorrentDialog/RememberLastSavePath`, `AddNewTorrentDialog/SavePathHistory`, `AddNewTorrentDialog/DownloadPathHistory`, `GUI/Qt6/AddNewTorrentDialog/TreeHeaderState`, `GUI/Qt6/AddNewTorrentDialog/SplitterState`, `GUI/AddNewTorrentDialog/FilterPatternFormat`, `TorrentOptionsDialog/Size`, `DownloadFromURLDialog/Size`, `TorrentCreator/*` (see 3.5), `SpeedLimitDialog/Size`, `TrackerEntriesDialog/Size`, `TrackersAdditionDialog/Size`, `TrackersAdditionDialog/TrackersListURL`, `StatisticsDialog/Size`, `AboutDialog/Size`, `CookiesDialog/Size`, `GUI/Qt6/CookiesDialog/ViewState`, `BanListOptionsDialog/Size`, `IPSubnetWhitelistOptionsDialog/Size`, `WatchedFolderOptionsDialog/DialogSize`, `GUI/TorrentTagsDialog/Size`, `SearchPluginSelectDialog/Size`, `SearchPluginSourceDialog/Size`, `RssFeedDownloader/geometrySize`, `GUI/Qt6/RSSFeedDownloader/HSplitterSizes`, `GUI/Qt6/RSSFeedDownloader/RuleDefSplitterState`, `PreviewSelectDialog/Size`, `GUI/Qt6/PreviewSelectDialog/HeaderState`, `GUI/UIThemeDialog/Size`, `BanListOptionsDialog/Size`, `IPSubnetWhitelistOptionsDialog/Size`.
- **AutoExpandableDialog** (`autoexpandabledialog.ui/.cpp`) is the generic single-line text input used for rename / "new name" / "add tag" / URL prompts. Behaviour: window title + label + one `QLineEdit` + OK/Cancel; the initial text is fully selected, except when `excludeExtension=true` (file rename) where only the base name (text before the last extension) is selected; on show, the dialog widens so the full text, title (×1.8) and label fit. Supports `Password` echo mode (UI lock). Returns `(text, ok)`.
- **FileSystemPathEdit / FileSystemPathComboEdit / FileSystemPathLineEdit** (`fspathedit*.cpp`): a line edit (or editable combo with history) plus a browse button. Browse button text is brief (`...`) or full `&Browse...`; tooltip `Browse...`. Default dialog captions: `Choose a file` / `Choose a folder`. File filter default: `Any file (*)`. Modes: `FileOpen`, `FileSave`, `DirectoryOpen`, `DirectorySave`, `ReadOnly`. Live validation shows a warning with one of: `Path does not exist`, `Path does not point to a directory`, `Path does not point to a file`, `Don't have read permission to path`, `Don't have write permission to path`. A "placeholder" path (grey text) is used to display the *effective default* when the field is empty.
- Speed spin boxes: unit suffix ` KiB/s`, range 0..2000000, value `0` is shown as special text `∞` (unlimited); when multiple torrents with different values are edited, the spin box shows `≠` (value −1) until the user touches it. Sliders are synced to the spin boxes; if a typed value exceeds the slider maximum, the slider maximum grows.
- Enum integers referenced in this section:
  - `TorrentContentLayout`: `0 = Original`, `1 = Subfolder` ("Create subfolder"), `2 = NoSubfolder` ("Don't create subfolder").
  - `Torrent::StopCondition`: `0 = None`, `1 = MetadataReceived`, `2 = FilesChecked`.
  - `DownloadPriority`: `0 = Ignored` ("Do not download"), `1 = Normal`, `6 = High`, `7 = Maximum`, `-1 = Mixed` (folder with mixed children; display only).
  - `ShareLimitAction`: `-1 = Default`, `0 = Stop`, `1 = Remove`, `3 = RemoveWithContent`, `2 = EnableSuperSeeding` (values fixed for config compatibility).
  - Share-limit sentinels: ratio `-2 = DEFAULT_RATIO_LIMIT` (use default/category), `-1 = NO_RATIO_LIMIT`; seeding / inactive seeding time (minutes) `-2 = DEFAULT_SEEDING_TIME_LIMIT`, `-1 = NO_SEEDING_TIME_LIMIT`.
  - `TorrentFileGuard::AutoDeleteMode` (setting `Core/AutoDeleteAddedTorrentFile`): `0 = Never`, `1 = IfAdded`, `2 = Always`.
  - `TorrentOperatingMode`: `0 = AutoManaged`, `1 = Forced`.

---

### 3.1 Add New Torrent dialog (`addnewtorrentdialog.ui/.cpp`, `guiaddtorrentmanager.cpp`)

#### 3.1.1 When it is shown (GUIAddTorrentManager::addTorrent)

Input `source` is a `.torrent` file path, `file://` URL, magnet URI, bare info-hash, or HTTP(S)/other supported URL.

1. If the caller passed `AddTorrentOption::SkipDialog`, or option is `Default` and preference `AddNewTorrentDialog/Enabled` (default **true**; GUI label Options › Downloads `Display torrent content and some options`) is **false** → torrent is added directly with default params (no dialog).
2. If `source` has a network scheme supported by the download manager (`http:`, `https:`, `ftp:`, `data:`, etc. — whatever `QNetworkAccessManager::supportedSchemes()` returns): log `Downloading torrent... Source: "%1"`, download it with size limit = `BitTorrent/TorrentFileSizeLimit` (default 100 MiB) using proxy if "use proxy for general purposes"; duplicates of the same URL in flight are ignored. On success the data is parsed as a torrent; if the server redirected to a magnet (`RedirectedToMagnet`), the magnet is parsed; on failure → "add failed" (see below).
3. Else try `TorrentDescriptor::parse(source)` (magnet URI or bare info-hash). If it starts with `magnet:` but fails to parse → failure.
4. Else treat as local file path (decode `file://`), wrap in a **TorrentFileGuard** (auto-deletion of the source .torrent depends on `Core/AutoDeleteAddedTorrentFile`: Never/IfAdded/Always) and load it.
5. Failure handling: log warning `Failed to add torrent. Source: "%1". Reason: "%2"`; desktop notification titled `Add torrent failed` with body `Couldn't add torrent '%1', reason: %2.`.

`processTorrent()`:
- If a dialog for the same info-hash is already open: feed it metadata if newly available, un-minimize, show, activate, raise. Return.
- If the torrent **already exists** in the session:
  - If preference `GUI/ConfirmActions/MergeTrackers` (default true; Options label `Ask to merge trackers for manually added torrent`) is on:
    - If metadata is now available, it is set on the existing torrent (in case it had none).
    - Private torrent (existing or new is private): warning box titled `Torrent is already present`, text `Trackers cannot be merged because it is a private torrent.`
    - Otherwise question box titled `Torrent is already present`, text `Torrent '%1' is already in the transfer list. Do you want to merge trackers from new source?`, buttons Yes/No, default button = Yes if session option "merge trackers" (`isMergeTrackersEnabled`) is on, else No. Yes → add all trackers and URL seeds from the new source.
  - Else (no confirmation) → core `handleDuplicateTorrent`: sets metadata if available; result message is one of `Merging of trackers is disabled` / `Trackers cannot be merged because it is a private torrent` / `Trackers are merged from new source` (merging trackers + url seeds); logs `Detected an attempt to add a duplicate torrent. Source: %1. Existing torrent: "%2". Torrent infohash: %3. Result: %4` and emits add-failed with error type `DuplicateTorrent`.
  - No dialog is shown in either case.
- If no metadata (magnet): start background metadata download (`Session::downloadMetadata`).
- Create the dialog. **Parenting:** on macOS the dialog is never attached to the main window (independent top-level `Qt::Window`); elsewhere it is attached iff `AddNewTorrentDialog/Attached` (default false; Advanced setting `Attach "Add new torrent" dialog to main window`, not shown on macOS). Non-attached dialogs are separate windows so several can be open simultaneously (one per info-hash).
- Position: centered on the main window, then clamped so it fits within the screen's available geometry (with a 10 px × 40 px frame offset for the top-left).
- If `AddNewTorrentDialog/TopLevel` (default true; Options label `Bring torrent dialog to the front`) the dialog activates and raises itself when shown.
- Accept → if "Do not delete .torrent file" was checked, the source file guard is told not to auto-remove; then `addTorrentToSession(source, descriptor, params)`. Reject → release the file guard (file deleted only in `Always` mode). When the dialog finishes, it is removed from the open-dialog map.

#### 3.1.2 Layout & fields (default size 900×680; horizontal splitter: left = options, right = content tree; non-collapsible)

Left pane (top to bottom):

| UI element | Exact label | Type | Default / initial value | Behaviour |
|---|---|---|---|---|
| Torrent management mode | `Torrent Management Mode:` | combo: `Manual` (index 0), `Automatic` (index 1) | `params.useAutoTMM` else `!Session::isAutoTMMDisabledByDefault()` (Options "Default Torrent Management Mode") | Tooltip: `Automatic mode means that various torrent properties(eg save path) will be decided by the associated category`. Automatic → "Save at" group disabled and populated with the category's save path / download path; Manual → paths from history. Any value ≠ 1 treated as Manual. |
| Save path group | `Save at` | group box containing `FileSystemPathComboEdit` (mode DirectorySave, caption `Choose save path`, max 20 visible items) | See "Save path population" | Changing it updates the size/free-space label. |
| Incomplete path | `Use another path for incomplete torrent` | checkable group box with a path combo (DirectorySave, caption `Choose save path`) | checked = `params.useDownloadPath` else `Session::isDownloadPathEnabled()` | Unchecked → path combo shows nothing (index −1); re-checking restores the last chosen index. |
| Remember | `Remember last used save path` | check box (right aligned under paths) | value of `AddNewTorrentDialog/RememberLastSavePath` (default false) | Stored on accept. |
| Options group | `Torrent options` | group box | | |
| Category | `Category:` | **editable** combo (insert policy InsertAtTop) | Items in order: `params.category` (if non-empty), the stored default category `AddNewTorrentDialog/DefaultCategory` (if non-empty), empty string `""`, then all session categories sorted with natural case-insensitive order (skipping duplicates). Index 0 is pre-selected. | Typing a non-existent name creates that category when the torrent is added (core `addCategory`; invalid names are dropped silently). In Automatic mode, changing category updates save/download paths. |
| Default category | `Set as default category` | check box | unchecked | On accept, if checked, current category text is stored to `AddNewTorrentDialog/DefaultCategory`. |
| Tags | `Tags:` | read-only line edit + tool button `...` (tooltip `Add/remove tags`) | `params.tags` joined with `", "`; placeholder `Click [...] button to add/remove tags.` | Button opens **Torrent Tags** dialog (3.22). |
| Start | `Start torrent` | check box | `!params.addStopped` else `!Session::isAddTorrentStopped()` | Toggling enables/disables the Stop condition label + combo. |
| Stop condition | `Stop condition:` | combo | `params.stopCondition` else `Session::torrentStopCondition()` | Items: `None`, (`Metadata received` only when the torrent has **no** metadata yet, inserted at index 1), `Files checked`. If metadata is present and the effective stop condition is MetadataReceived, "Start torrent" is unchecked and the combo is set to None. Tooltip (HTML): **None** - No stop condition is set. / **Metadata received** - Torrent will stop after metadata is received. *Torrents that have metadata initially will be added as stopped.* / **Files checked** - Torrent will stop after files are initially checked. *This will also download metadata if it wasn't there initially.* (exact strings: `No stop condition is set.`, `Torrent will stop after metadata is received.`, `Torrents that have metadata initially will be added as stopped.`, `Torrent will stop after files are initially checked.`, `This will also download metadata if it wasn't there initially.`) |
| Queue top | `Add to top of queue` | check box | `params.addToQueueTop` else `Session::isAddTorrentToQueueTop()` | |
| Skip check | `Skip hash check` | check box | `params.skipChecking` (false) | |
| Sequential | `Download in sequential order` | check box | `params.sequential` (false) | |
| First/last | `Download first and last pieces first` | check box | `params.firstLastPiecePriority` (false) | |
| Keep .torrent | `Do not delete .torrent file` | check box | unchecked | **Visible only when** `Core/AutoDeleteAddedTorrentFile != Never`. Tooltip: `When checked, the .torrent file will not be deleted regardless of the settings at the "Download" page of the Options dialog` |
| Content layout | `Content layout:` | combo: `Original` (0), `Create subfolder` (1), `Don't create subfolder` (2) | `params.contentLayout` else `Session::torrentContentLayout()` | Changing it immediately re-shapes the file tree (adds/strips the root folder). For `Create subfolder` on a single-file torrent the root folder name is the file name without its extension. |
| Info group | `Torrent information` | group box | | see 3.1.3 |

Note: the Add New Torrent dialog in 5.2.4 has **no** share-limit or speed-limit fields (those exist only in AddTorrentParamsWidget for watched folders/RSS rules, and in Torrent Options after adding).

Bottom row:
- `Never show again` check box (left). On **accept**, sets `AddNewTorrentDialog/Enabled = !checked` (i.e. checking it disables the dialog for future adds; re-enable in Options).
- Metadata area: indeterminate progress bar (hidden text) + status label + `Save as .torrent file...` push button.
- Button box: `OK` / `Cancel`.

Default focus: Manual mode → save path; Automatic → category combo.

Window title: torrent name when metadata is present; for magnets without metadata: the magnet `dn` name or `Magnet link` if none.

#### 3.1.3 Torrent information panel

| Label | Value |
|---|---|
| `Size:` | `"%1 (Free space on disk: %2)"` — %1 = sum of sizes of files whose priority > Ignored (friendly units), or `Not available` when no metadata / zero; %2 = free disk space of the selected save path (if that folder doesn't exist yet, the nearest existing ancestor is queried). Recomputed on every priority change and save path change. |
| `Date:` | creation date in locale short format, or `Not available`; magnets without metadata show `Not Available`. |
| `Info hash v1:` | v1 hex string or `N/A` (selectable text). |
| `Info hash v2:` | v2 hex string or `N/A` (selectable text). |
| `Comment:` | HTML-escaped comment with URLs auto-linked (`parseHtmlLinks`), rich text, word-wrap, external links open in browser, inside a scroll area. Magnets without metadata: `Not Available`. |

#### 3.1.4 Content (file tree, right pane)

- Toolbar row: `Select All`, `Select None` buttons, then a filter line edit with placeholder `Filter files...`. `Ctrl+F` (QKeySequence::Find) focuses and selects the filter. The filter's context menu = standard edit menu + separator + submenu `Pattern Format` with radio items `Plain text`, `Wildcards` (default), `Regular expression` (persisted in `GUI/AddNewTorrentDialog/FilterPatternFormat`).
- `TorrentContentWidget` tree (multi-select, sortable). Columns shown: Name (with check box), Total Size, Priority (Progress, Remaining and Availability are force-hidden; column visibility is **locked** in this dialog). Header state persisted only when metadata exists.
- Double-click action = **Rename** (not open). `F2` renames. Space/Select toggles the check state.
- Context menu (single selection): `Rename...` (icon edit-rename), separator, submenu `Priority` → `Do not download`, `Normal`, `High`, `Maximum`, separator, `By shown file order`. (No `Open`/`Open containing folder` because the content has no storage location yet.) Multi-selection: `Do not download`, `Normal priority`, `High priority`, `Maximum priority`, separator, `Priority by shown file order`.
- "By shown file order": selected rows in displayed order are split into 3 equal groups (group size = max(n/3, 1)): first group → Maximum, second → High, rest → Normal.
- Rename prompt: AutoExpandableDialog titled `Renaming`, label `New name:`, base-name preselected for files; result trimmed. Rename failures show warning `Rename error` with core messages: `The old path is invalid: '%1'.`, `The new path is invalid: '%1'.`, `Absolute path isn't allowed: '%1'.`, `The file already exists: '%1'.`, `No such file: '%1'.`, `The folder already exists: '%1'.`, `No such folder: '%1'.`. Renames are kept in `params.filePaths`; case-only renames are allowed.
- Excluded file names: if Options › Downloads "Excluded file names" is enabled, the filter is applied on load and matching files are pre-set to `Do not download`.

#### 3.1.5 Magnet / metadata retrieval

- Without metadata: label `Retrieving metadata...` with indeterminate progress bar; `Save as .torrent file...` hidden; Date/Comment `Not Available`; size `Not available`.
- When metadata arrives (core `metadataDownloaded` matched by info-hash): label `Parsing metadata...` → tree populated → label `Metadata retrieval complete` (progress bar hidden; label stays visible). If stop condition was `Metadata received`, "Start torrent" is unchecked, combo switches to `None` and the `Metadata received` item is removed. `Save as .torrent file...` becomes visible; for torrents with a v2 info-hash it is **disabled** with tooltip `Cannot create v2 torrent until its data is fully downloaded.`
- Cancel while waiting for metadata → `Session::cancelDownloadMetadata(id)`.

#### 3.1.6 "Save as .torrent file..."

Save dialog caption `Save as torrent file`, default `~/<torrent name>.torrent`, filter `Torrent file (*.torrent)`; extension auto-appended. Error: critical box `I/O Error` / `Couldn't export torrent metadata file '%1'. Reason: %2.`

#### 3.1.7 Save path population (Manual mode)

- Save path combo items = `AddNewTorrentDialog/SavePathHistory` (list), or just the session default save path if history is empty. Selection: `params.savePath` if given (inserted at top if not in history); else if "Remember last used save path" is **off** → session default save path (inserted if missing); else index 0 (most recent).
- Download path combo similarly from `AddNewTorrentDialog/DownloadPathHistory` / session download path.
- On accept (Manual): selected save path moved/prepended to history; history truncated to `AddNewTorrentDialog/SavePathHistoryLength` (default **8**, clamped 0..99, Advanced setting "Save path history length"); same for download path if used. Changing the preference trims the stored history immediately.
- On accept (Automatic): `savePath`, `downloadPath` cleared, `useDownloadPath = unset` → category decides.

#### 3.1.8 Accept result (`AddTorrentParams` produced)

`skipChecking`, `category` (combo text), `addToQueueTop`, `addStopped = !Start torrent`, `stopCondition`, `contentLayout`, `sequential`, `firstLastPiecePriority`, `useAutoTMM`, `savePath`, `useDownloadPath`, `downloadPath`, `tags`, `filePaths` (renamed paths after layout applied), `filePriorities`.

#### 3.1.9 Persistence on close (`done()`)
Dialog size, splitter state, and (if metadata) tree header state.

---

### 3.2 AddTorrentParamsWidget (embedded editor of add-torrent defaults) (`addtorrentparamswidget.ui/.cpp`)

Used by **Watched Folder Options** (3.20) and the **RSS Downloader** rule editor (3.25). Every field has a `Default` choice meaning "use the global/category setting" (`std::optional` unset).

| Label | Widget | Items / values | Notes |
|---|---|---|---|
| `Torrent Management Mode:` | combo | `Default` (unset), `Manual` (false), `Automatic` (true) | Tooltip as in 3.1. |
| group `Save at` | | | Disabled (and note shown) whenever effective mode is Automatic or TMM is Default. |
| (note) | label | `Note: the current defaults are displayed for reference.` | Visible when the group is disabled. |
| (save path) | `FileSystemPathLineEdit` (caption `Choose save path`) | | Placeholder shows `Session::suggestedSavePath(category, useAutoTMM)`. |
| `Use another path for incomplete torrents:` | combo | `Default` (unset), `Yes` (true), `No` (false) | When Automatic: selection reflects the category's download-path option. |
| (download path) | `FileSystemPathLineEdit` (caption `Choose save path`) | | Enabled only for `Yes`; placeholder shows suggested download path when effective "use download path" is true. |
| `Category:` | editable combo | current category (if any), `""`, then sorted categories | Changing category refreshes default paths and share-limit defaults. |
| `Tags:` | read-only line edit + `...` button | placeholder `Click [...] button to add/remove tags.`, tooltip `Add/remove tags` | Opens Torrent Tags dialog. |
| `Content layout:` | combo | `Default`, `Original`, `Create subfolder`, `Don't create subfolder` | |
| `Skip hash check` | check box | | |
| `Start torrent:` | combo | `Default`, `Yes`, `No` | `Yes` → addStopped=false. |
| `Stop condition:` | combo | `Default`, `None`, `Metadata received`, `Files checked` | |
| `Add to top of queue:` | combo | `Default`, `Yes`, `No` | |
| group `Torrent share limits` | TorrentShareLimitsWidget (3.9) | | Defaults shown are the category's (or global if no category). Output: ratioLimit, seedingTimeLimit, inactiveSeedingTimeLimit, shareLimitAction. |

Misc fields are laid out with a flow layout (wrap): Content layout, Skip hash check, Start torrent, Stop condition, Add to top of queue.

---

### 3.3 Add torrent links dialog (`downloadfromurldialog.ui/.cpp`) — action `Add Torrent &Link...`

- Window title `Download from URLs`; group title `Add torrent links`; multi-line plain-text box (no word wrap) with label/hint `One link per line (HTTP links, Magnet links and info-hashes are supported)`.
- OK button relabeled `Download`; Cancel.
- **Clipboard prefill:** on open, the clipboard text is split by `\n`, each line trimmed, and lines that are "downloadable" are inserted (original order, newline-terminated), cursor moved to end, focus on the button box. A line is downloadable if any of:
  - starts with `<scheme>:` for any scheme supported by Qt network access manager (http, https, ftp, file, data, qrc…), case-insensitive;
  - starts with `magnet:` (case-insensitive);
  - exactly 64 chars of `[0-9A-Fa-f]` (v2 SHA-256 info-hash; only with libtorrent 2);
  - exactly 40 chars of `[0-9A-Fa-f]` (v1 SHA-1 hex);
  - exactly 32 chars of `[2-7A-Za-z]` (v1 SHA-1 Base32).
- **Submit** (`Download` or `Ctrl+Return`): lines trimmed, empty lines dropped, **de-duplicated** preserving first occurrence. If none: warning `No URL entered` / `Please type at least one URL.` (dialog stays open). Otherwise each URL is passed to the add-torrent manager (respecting the Add New Torrent dialog preference).
- No per-line validation on submit (invalid entries fail later through the normal add-failed path).

### 3.4 Open torrent files (action `&Add Torrent File...`)

`QFileDialog::getOpenFileNames` titled `Open Torrent Files`, start dir = `Preferences::getMainLastDir()`, filter `Torrent Files (*.torrent)`, multi-select. Each file goes through 3.1.1. The parent folder of the first file is stored as the new last dir.

---

### 3.5 Torrent Creator (`torrentcreatordialog.ui/.cpp`, `base/bittorrent/torrentcreator.cpp`)

Window title `Torrent Creator`. Non-modal, scrollable. Accepts drag-and-drop of a file/folder (first URL only; `text/plain` or `text/uri-list`).

| Section / label | Widget | Default | Notes |
|---|---|---|---|
| group `Select file/folder to share` → `Path:` | read-only path line edit | `TorrentCreator/LastAddPath` else home dir | |
| `[Drag and drop area]` | label | | |
| `Select file` | button | | `QFileDialog::getOpenFileName` titled `Select file` (symlinks not resolved on non-Windows so the link name is kept). |
| `Select folder` | button | | `getExistingDirectory` titled `Select folder`. |
| group `Settings` → `Torrent format:` | combo | index from `TorrentCreator/TorrentFormat`, default **1 = Hybrid** | Items in order: `V2` (0), `Hybrid` (1), `V1` (2). Shown only with libtorrent 2.x. Maps to `TorrentFormat {V1, V2, Hybrid}`; lt flags: V1 → `v1_only`, V2 → `v2_only`, Hybrid → none. |
| `Piece size:` | combo | index from `TorrentCreator/PieceSize`, default 0 (`Auto`) | Items: `Auto` (0 → libtorrent chooses), then `16 KiB`, `32 KiB`, `64 KiB`, `128 KiB`, `256 KiB`, `512 KiB`, `1 MiB`, `2 MiB`, `4 MiB`, `8 MiB`, `16 MiB`, `32 MiB`, `64 MiB`, `128 MiB` (= `1024 << i` for i = 4..17). |
| `Calculate number of pieces:` | button + result label | | Runs `TorrentCreator::calculateTotalPieces(path, pieceSize, format)` in a background thread; label shows `Calculating...` then the integer; button disabled meanwhile. |
| `Private torrent (Won't distribute on DHT network)` | check box | `TorrentCreator/PrivateTorrent` (false) | Sets `private` flag. |
| `Start seeding immediately` | check box | `TorrentCreator/StartSeeding` (false) | Enables the next box. |
| `Ignore share ratio limits for this torrent` | check box | `TorrentCreator/IgnoreRatio` (false) | Enabled only when Start seeding is checked. |
| group-checkbox `Optimize alignment` | checkable group | `TorrentCreator/OptimizeAlignment` (true) | **libtorrent 1.2 only** (hidden with lt2). |
| `Align to piece boundary for files larger than:` | spin box, suffix ` KiB`, min −1 (special text `Disabled`), max INT_MAX | `TorrentCreator/PaddedFileSizeLimit` (−1) | lt 1.2 only; value×1024 bytes, −1 = disabled. |
| group `Fields` → `Tracker URLs:` | multi-line text | `TorrentCreator/TrackerList` | Placeholder `You can separate tracker tiers / groups with an empty line.` |
| `Web seed URLs:` | multi-line text | `TorrentCreator/WebSeedList` | One per line; blank lines skipped; each trimmed. |
| `Comments:` | multi-line text | `TorrentCreator/Comments` | Stored as `comment`. |
| `Source:` | line edit | `TorrentCreator/Source` | Written as `info.source` (only if non-empty) — changes info-hash, used by private trackers. |
| `Progress:` | progress bar 0..100 | 0 | Reset to 0 whenever the input path changes. |
| Buttons | OK relabeled `Create Torrent`, Cancel | | |

**Create flow:**
1. Path must be readable, else critical `Torrent creation failed` / `Reason: Path to file/folder is not readable.`
2. Save dialog `Select where to save the new torrent`, default `<TorrentCreator/LastSavePath or home>/<input name>.torrent`, filter `Torrent Files (*.torrent)`; `.torrent` appended if missing; parent dir stored to `TorrentCreator/LastSavePath`.
3. All inputs disabled + wait cursor; creator runs on a 1-thread pool. Cancel/close (`rejected`) requests interruption (error `Operation aborted`).
4. Tracker text processing: trimmed, runs of 3+ newlines collapsed to exactly one blank line, split by `\n`; an empty line increments the tier (tier starts at 0), each non-empty line trimmed and added with the current tier.
5. File collection: for a single file → that file. For a folder → all sub-directories recursively, **sorted by natural case-insensitive order**, files within each directory sorted the same way; **files and folders whose name starts with `.` are excluded**; Windows `.lnk` shortcuts skipped. Paths are relative to the input's parent dir.
6. Piece hashing reports progress `piece_index * 100 / num_pieces`.
7. `created by` = `qBittorrent v5.2.4` (literally `"qBittorrent " QBT_VERSION`), comment, private flag; creation date set by libtorrent.
8. Success → info box `Torrent creator` with text `Torrent created:\n<path>`. If "Start seeding immediately": the .torrent is loaded and added with forced params: addStopped=false, contentLayout=Original, savePath = input's parent dir, **skipChecking=true**, stopCondition=None, useAutoTMM=false, useDownloadPath=false; if "Ignore share ratio limits" → ratioLimit=−1, seedingTimeLimit=−1, inactiveSeedingTimeLimit=−1 (unlimited). Load failure → critical `Add torrent failed` / `Add torrent to transfer list failed.\nReason: "%1"`.
9. Failure → info box `Torrent creation failed` with `Create new torrent file failed. Reason: %1.`
10. All settings (path, piece size index, private, start seeding, ignore ratio, format/alignment, tracker list, web seeds, comments, source, size) persisted on close.

The Tools menu item that opens it is `Torrent &Creator`; a default input path may be passed (e.g. from drag & drop).

---

### 3.6 Set location (context menu `Set loc&ation...`)

Not a custom dialog: a `QFileDialog` (directory mode, options DontConfirmOverwrite | ShowDirsOnly | HideNameFilterDetails), caption `Choose save path`, starting at the **first selected torrent's** current save path. On accept (if the folder exists) for every selected torrent: **Automatic Torrent Management is disabled** and `setSavePath(newLocation)` triggers a storage move. Nothing happens if the chosen path doesn't exist.

### 3.7 Rename torrent (context menu `Re&name...`)

Only when exactly one torrent is selected. AutoExpandableDialog titled `Rename`, label `New name:`, prefilled with current name (fully selected). If OK and non-empty: any `\r\n`, `\n`, `\r` replaced by a single space; sets the torrent's display name (does not rename files). File/folder renaming inside a torrent uses the `Renaming` / `New name:` prompt from the content tree (3.1.4).

### 3.8 Global Speed Limits dialog (`speedlimitdialog.ui/.cpp`)

Opened from main-window action `Set Global Speed Limits...` (also in the tray menu), and by clicking the status-bar download/upload speed labels. Title `Global Speed Limits`.

| Group | Rows |
|---|---|
| `Speed limits` (icon `slow_off`) | `Upload:` slider + spin; `Download:` slider + spin |
| `Alternative speed limits` (icon `slow`) | `Upload:` slider + spin; `Download:` slider + spin |

Spin: suffix ` KiB/s`, max 2,000,000, `0` displays `∞`. Initial values = session limits / 1024 (bytes/s → KiB/s, floor, min 0). Slider max = max(10000, current value). On OK, only fields whose value changed are written back (`value × 1024` bytes/s) via `setGlobalUploadSpeedLimit`, `setGlobalDownloadSpeedLimit`, `setAltGlobalUploadSpeedLimit`, `setAltGlobalDownloadSpeedLimit`. Per-torrent limits are set in **Torrent Options** (3.10), not here.

### 3.9 Share limits widget (`torrentsharelimitswidget.ui/.cpp`)

Embedded in Torrent Options, Category dialog, AddTorrentParamsWidget.

| Label | Mode combo items (index) | Value widget |
|---|---|---|
| `Ratio:` | 0 = `Default` / `From category`, 1 = `Unlimited`, 2 = `Set to` | double spin, step 0.05, max INT_MAX, initial assigned value 1.00 |
| `Seeding time:` | same | int spin minutes, max 9999999, suffix ` min`, initial assigned value 1440 |
| `Inactive seeding time:` | same | same as seeding time (1440 initial) |
| `Action when the limit is reached:` | 0 = `Default`/`From category` (optionally `Default (%1)` / `From category (%1)` with the inherited action name), 1 = `Stop torrent`, 2 = `Remove torrent`, 3 = `Remove torrent and its content`, 4 = `Enable super seeding for torrent` | — |

- The "Default" item text is `Default` when the inherited defaults are global (no category), and `From category` when the context has a category (UsedDefaults::Category).
- Value spin boxes are enabled only in `Set to` mode. In Default mode the spin box displays the inherited value (if ≥ 0) greyed, else empty; Unlimited → empty. Switching away from `Set to` remembers the typed value for when the user switches back.
- Output mapping: Default → −2 (ratio/time) / ShareLimitAction::Default (−1); Unlimited → −1; Set to → value; combo uninitialized (multi-selection with differing values) → "no change".
- Inheritance: category values of −2/Default inherit from the parent category recursively; the root inherits the global Options › BitTorrent "Seeding Limits" (global max ratio, max seeding minutes, max inactive seeding minutes, global action).

### 3.10 Torrent Options dialog (`torrentoptionsdialog.ui/.cpp`) — context menu `Torrent &options...`

Title `Torrent Options`. Works on **one or many** selected torrents; for every field, if the selected torrents disagree the control shows a mixed state (tri-state check box partially checked / spin shows `≠` / category placeholder) and is left untouched on OK unless the user changes it.

| Label | Widget | Behaviour |
|---|---|---|
| `Automatic Torrent Management` | tri-state check box | Tooltip `Automatic mode means that various torrent properties (e.g. save path) will be decided by the associated category`. Checked → Save group disabled and shows category paths; Unchecked → editable with each torrent's original paths; Partial → disabled, blank. |
| group `Save at` | path combo (caption `Choose save path`) | Shows the common save path if all equal. |
| `Use another path for incomplete torrent` | tri-state check box + path combo | When checked with empty path, prefilled with session default download path. Unchecked on OK → clears the download path of each torrent. |
| `Category:` | editable combo, unlimited visible items | If categories differ, first item is `--Currently used categories--` (placeholder text) meaning "keep each torrent's category"; then `""`, then all categories sorted naturally. On OK: if text changed (or a non-index-0 item chosen), a missing category is **created** (`addCategory`) and assigned to every torrent. Changing category in Automatic mode updates displayed paths and share-limit defaults. |
| group `Torrent Speed Limits` → `Upload:` / `Download:` | slider + spin (` KiB/s`, max 2,000,000, 0=`∞`) | Slider max = current global (or alternative, if alt limits active) limit in KiB/s, or 10000 if global unlimited; grows if a torrent limit exceeds it. Label `These will not exceed the global limits`. On OK, changed values are applied `× 1024` bytes/s. |
| group `Torrent Share Limits` | TorrentShareLimitsWidget | Defaults from the common category (if all same) else global. |
| `Disable DHT for this torrent` | tri-state check box | Disabled and forced checked if **all** selected torrents are private; tooltip `Not applicable to private torrents`. Applied only to non-private torrents. |
| `Download in sequential order` | tri-state | |
| `Disable PeX for this torrent` | tri-state | Same private rule/tooltip. |
| `Download first and last pieces first` | tri-state | |
| `Disable LSD for this torrent` | tri-state | Same private rule/tooltip. |
| OK / Cancel | | Only changed fields are applied, per torrent. |

### 3.11 Edit trackers dialog (`trackerentriesdialog.ui/.cpp`) — context menu `Edit trac&kers...`

- Title `Edit trackers`. Label text (verbatim):
  ```
  One tracker URL per line.

  - You can split the trackers into groups by inserting blank lines.
  - All trackers within the same group will belong to the same tier.
  - The group on top will be tier 0, the next group tier 1 and so on.
  - Below will show the common subset of trackers of the selected torrents.
  ```
- Plain text edit prefilled with the **intersection** of tracker URLs across the selected torrents (equality by URL only), grouped by tier: tier 0 URLs, then for each tier 1..maxTier a blank line followed by that tier's URLs (empty tiers produce extra blank lines).
- Parsing (`parseTrackerEntries`): split by `\n`, each line trimmed; an empty line increments the tier counter (overflow-guarded); non-empty lines become `{url, tier}`.
- OK → `replaceTrackers(list)` on **every** selected torrent (replaces whole tracker list).

### 3.12 Add trackers dialog (`trackersadditiondialog.ui/.cpp`) — Trackers tab context menu `Add trackers...`

- Title `Add trackers`; label `List of trackers to add (one per line):` with a text edit; label `µTorrent compatible list URL:` with a line edit (persisted `TrackersAdditionDialog/TrackersListURL`) and button `Download trackers list`; OK relabeled `Add`.
- Download: empty URL → warning `Trackers list URL error` / `The trackers list URL cannot be empty`. Otherwise button disabled + wait cursor; fetch via download manager (proxy per "general purposes" setting). Failure → warning `Download trackers list error` / `Error occurred when downloading the trackers list. Reason: "%1"`. Success → response body (UTF-8, trimmed) appended as-is to the text box (newline inserted first if needed).
- OK: text parsed with `parseTrackerEntries` (blank lines = new tier); every entry's tier is offset by `baseTier = (last existing tracker's tier + 1)` (or 0 if none), clamped; added via `addTrackers` (existing URLs ignored by core).

### 3.13 Small prompts in properties / filters

| Context | Dialog | Strings & validation |
|---|---|---|
| Trackers tab › Edit tracker URL | AutoExpandableDialog | Title `Tracker editing`, label `Tracker URL:`, prefilled with last selected tracker. Invalid URL → warning `Tracker editing failed` / `The tracker URL entered is invalid.`; existing URL → `The tracker URL already exists.`; same URL → no-op. Replaces the URL keeping tier. |
| HTTP Sources tab › Add web seed | AutoExpandableDialog | Title `Add web seed` (comment "Add HTTP source"), label `Add web seed:`, default text `http://www.`. Duplicate → warning `qBittorrent` / `This web seed is already in the list.` |
| HTTP Sources tab › Edit web seed | AutoExpandableDialog | Title `Web seed editing`, label `Web seed URL:`. Duplicate → same warning. Implemented as remove old + add new. |
| Peers tab › Add peers | `PeersAdditionDialog` (`properties/peersadditiondialog.ui`) | Title `Add Peers`, label `List of peers to add (one IP per line):`, hint `Format: IPv4:port / [IPv6]:port`. Empty → `No peer entered` / `Please type at least one peer.`; each line parsed by `PeerAddress::parse`; invalid → `Invalid peer` / `The peer '%1' is invalid.` (dialog stays open). After adding: info `Adding peers` with `Some peers cannot be added. Check the Log for details.` or `Peers are added to this torrent.` |
| Peers tab › Ban peer permanently | question box | `Ban peer permanently` / `Are you sure you want to permanently ban the selected peers?`; each IP banned and logged `Peer "%1" is manually banned`. |
| Trackers filter (sidebar) › Remove tracker | question box | `Removal confirmation` / `Are you sure you want to remove tracker "%1" from all torrents?` with check box `Don't ask me again.` (sets `confirmRemoveTrackerFromAllTorrents=false`). Skipped entirely if that preference is false. |
| Tag filter (sidebar) › `Add tag...` | AutoExpandableDialog loop | Title `Add tag`, label `Tag:`; invalid → `Invalid tag name` / `Tag name '%1' is invalid`; existing → `Tag exists` / `Tag name already exists.` |
| Category filter (sidebar) › `Add category...` / `Add subcategory...` / `Edit category...` | Category dialog (3.21) | |

### 3.14 Statistics dialog (`statsdialog.ui/.cpp`) — action `&Statistics`

Title `Statistics`; refreshes on every core `statsUpdated` signal (session stats alert, ~1 s). Single `Close`/OK button.

| Group | Label | Value & computation |
|---|---|---|
| `User statistics` | `All-time upload:` | `friendlyUnit(allTimeUpload)`; allTimeUpload = persisted previous total (`AlltimeUL`) + this session's total uploaded bytes (`net.sent_bytes`-based totals; only increases). |
| | `All-time download:` | same for download (`AlltimeDL`). |
| | `All-time share ratio:` | `allTimeUL / allTimeDL` with 2 decimals if both > 0, else `-`. |
| | `Session waste:` | `friendlyUnit(net.recv_redundant_bytes + net.recv_failed_bytes)` (this session). |
| | `Connected peers:` | `peer.num_peers_connected`. |
| `Cache statistics` | `Read cache hits:` | **libtorrent 1.2 only** (hidden with lt2): `100 × num_blocks_cache_hits / max(hits + num_blocks_read, 1)` formatted `%1%` (2 decimals) or `0%`. |
| | `Total buffer size:` | `friendlyUnit(disk.disk_blocks_in_use × 16 KiB)`. |
| `Performance statistics` | `Write cache overload:` | `100 × peer.num_peers_down_disk / peersCount` as `%1%` (2 decimals), or `0%`. |
| | `Read cache overload:` | `100 × peer.num_peers_up_disk / peersCount`, same format. |
| | `Queued I/O jobs:` | `disk.queued_disk_jobs`. |
| | `Average time in queue:` | `%1 ms` where value = `disk.disk_job_time / (disk.num_write_ops + disk.num_read_ops + disk.num_blocks_hashed)` (0 if no jobs). |
| | `Total queued size:` | `friendlyUnit(cacheStatus.queuedBytes)` — **note:** in 5.2.4 `queuedBytes` is never assigned, so this always shows `0 B` (WebAPI `total_queued_size` likewise 0). |

All-time totals are persisted to the statistics store every `saveStatisticsInterval` minutes (Advanced setting) and on exit.

### 3.15 Execution Log (`executionlogwidget.ui/.cpp`, `log/*`)

A main-window **tab** (`Execution Log`, shortcut `Alt+4` to switch to it) shown when View › `&Log` › `Show` is checked (setting `GUI/Log/Enabled`). Message-type filter via the same `&Log` submenu check items `Normal Messages`, `Information Messages`, `Warning Messages`, `Critical Messages` (stored as flags in `GUI/Log/Types`, default ALL = −1; these items are disabled while the log is hidden).

- Inner tab widget with two tabs: `General` and `Blocked IPs`.
- **General**: list of log messages, newest first. Each row rendered as `<time> - <message>` where time = `QLocale::system()` short date-time of the message timestamp; time drawn in color `Log.TimeStamp`; message color by type: NORMAL (`0x1`) → `Log.Normal` (default window text), INFO (`0x2`) → `Log.Info` (light #0969da / dark #58a6ff), WARNING (`0x4`) → `Log.Warning` (light #bc4c00 / dark #db6d28), CRITICAL (`0x8`) → `Log.Critical` (light #cf222e / dark #f85149).
- **Blocked IPs**: rows `%1 was blocked. Reason: %2.` (blocked by IP filter etc., with reason) or `%1 was banned` (manual/auto ban), color `Log.BannedPeer` (dangerFg).
- Capacity: 20,000 rows per view (`MAX_VISIBLE_MESSAGES`); the core logger also keeps a 20,000-entry ring buffer for messages and for peers (`MAX_LOG_MESSAGES`). Oldest rows drop off.
- Multi-select list. Context menu: `Copy` (only if a row is current) and `Clear` (clears the view's model only, not the core buffer). `Ctrl+C` copies selected rows as `time - message` joined with `\n`.
- Message types enum: `ALL = -1, NORMAL = 1, INFO = 2, WARNING = 4, CRITICAL = 8`.

### 3.16 About dialog (`aboutdialog.ui/.cpp`) — Help › `&About`

- Title `About qBittorrent`. Header: tray icon (32 px) + `<h2>qBittorrent v5.2.4 (64-bit)</h2>` (bitness = pointer size).
- Tabs:
  1. `About`: mascot image + text: `An advanced BitTorrent client programmed in C++, based on Qt toolkit and libtorrent-rasterbar.` / `Copyright © 2006-2026 The qBittorrent project` / table: `Home Page:` https://www.qbittorrent.org, `Forum:` https://forum.qbittorrent.org, `Bug Tracker:` https://bugs.qbittorrent.org.
  2. `Authors`: group `Current maintainer` — `Name:` Sledgehammer999, `Nationality:` Greece, `E-mail:` sledgehammer999@qbittorrent.org; group `Original author` — `Name:` Christophe Dumez, `Nationality:` France, `E-mail:` chris@qbittorrent.org.
  3. `Special Thanks`: HTML from bundled `thanks.html`.
  4. `Translators`: HTML from bundled `translators.html`.
  5. `License`: HTML from bundled `gpl.html` (GPL v2+ text with OpenSSL exception).
  6. `Software Used`: `qBittorrent was built with the following libraries:` then rows `Qt:`, `Libtorrent:`, `Boost:`, `OpenSSL:`, `zlib:` with version strings; button `Copy to clipboard` copies `"Qt: <v>\nLibtorrent: <v>\nBoost: <v>\nOpenSSL: <v>\nzlib: <v>\n"`. Footer label: `The free IP to Country Lite database by DB-IP is used for resolving the countries of peers. The database is licensed under the Creative Commons Attribution 4.0 International License` + link https://db-ip.com/.
- Size persisted; first open uses `adjustSize()`.

### 3.17 Manage Cookies (`cookiesdialog.ui/.cpp`, `cookiesmodel.cpp`) — Tools menu `Manage Cookies...`

- Title `Manage Cookies`. Tree view of all cookies in the download manager's cookie jar; columns `Domain`, `Path`, `Name`, `Value`, `Expiration Date` — all cells editable in place (expiration as date-time editor).
- Tool buttons: add (`list-add` icon) inserts a new empty cookie **after the current row** with expiration = now + 2 years and selects it; delete (`list-remove`) removes all selected rows.
- OK → `DownloadManager::setAllCookies(list)`; Cancel discards. Header state + size persisted.
- Persistence: cookie jar saved to preferences (`Network/Cookies`) at shutdown and loaded at startup; **session cookies and expired cookies are dropped** on both load and save.

### 3.18 Banned IPs (`banlistoptionsdialog.ui/.cpp`) — Options › Connection "Manually banned IP addresses..."

- Title `List of banned IP addresses`. Sorted (ascending) list of the session's banned IPs; line edit + `Ban IP` button (enabled only when the text is a valid IPv4/IPv6 address), `Delete` button removes selected.
- Adding: invalid → warning `Warning` / `The entered IP address is invalid.`; normalized via `QHostAddress::toString()` (RFC 5952 for IPv6) to avoid duplicates; duplicate → `Warning` / `The entered IP is already banned.`
- OK applies `Session::setBannedIPs(sortedList)` only if modified (else behaves as Cancel).

### 3.19 IP subnet whitelist (`ipsubnetwhitelistoptionsdialog.ui/.cpp`) — Options › WebUI "Bypass authentication for clients in whitelisted IP subnets" › `IP subnet whitelist...`

- Title `List of whitelisted IP subnets`. Sorted list; line edit with placeholder `Example: 172.17.32.0/24, fdff:ffff:c8::/40`; `Add subnet` button enabled only when text parses as a subnet (`QHostAddress::parseSubnet`, accepts `addr/prefix` and also bare IPs / IPv4 netmask forms that Qt accepts); `Delete` removes selected.
- Invalid on click → critical `Error` / `The entered subnet is invalid.` Stored normalized (`subnetToString`). OK saves `Preferences::setWebUIAuthSubnetWhitelist` only if modified.

### 3.20 Watched Folder Options (`watchedfolderoptionsdialog.ui/.cpp`) — Options › Downloads › watched folders `Add...` / `Options...` / double-click

- Title `Watched Folder Options`.
- `Recursive mode` check box, tooltip: `Will watch the folder and all its subfolders. In Manual torrent management mode it will also add subfolder name to the selected Save path.`
- Group `Torrent parameters` containing an **AddTorrentParamsWidget** (3.2) initialized from the folder's stored params (empty/defaults for a new folder).
- Add flow (Options): first `Select folder to monitor` directory picker (starting at last scan dir's parent), then this dialog; on OK the folder is added to the model; errors shown as critical `Adding entry failed` with: `Watched folder path cannot be empty.`, `Watched folder path cannot be relative.`, `Folder '%1' is already in watch list.`, `Folder '%1' doesn't exist.`, `Folder '%1' isn't readable.`
- Result: `WatchedFolderOptions { recursive, addTorrentParams }` (persisted by TorrentFilesWatcher to `watched_folders.json`).

### 3.21 Category properties dialog (`torrentcategorydialog.ui/.cpp`)

Title `Torrent Category Properties`.

| Label | Widget | Notes |
|---|---|---|
| `Name:` | line edit | New category: prefilled `New Category`, or `<parent>/New Category` for "Add subcategory..." with only the last segment selected. Editing an existing category → name field disabled. OK enabled only when non-empty. |
| `Save path:` | path combo (DirectorySave, caption `Choose save path`) | Empty = implicit; placeholder shows the effective path `categorySavePath(name, options)`. |
| group `Save path for incomplete torrents:` → `Use another path for incomplete torrents:` | combo `Default` (0) / `Yes` (1) / `No` (2) | `Default` → inherit from parent category (root: global "use download path" setting). |
| `Path:` | path combo (DirectorySave, caption `Choose download path`) | Enabled only for `Yes`; placeholder = effective download path when the effective option is enabled. The last typed path is remembered while toggling. |
| group `Torrent share limits` | TorrentShareLimitsWidget | Defaults shown are the **parent category's** effective limits (or global for top-level); "Default" text becomes `From category` when there is a parent. Stored as −2/Default when "Default". |

**Validation (create):** loop until valid or cancel:
- `Session::isValidCategoryName(name)` regex `^([^\\\/]|[^\\\/]([^\\\/]|\/(?=[^\/]))*[^\\\/])$` (empty name is "valid" but OK is disabled). Failure → critical `Invalid category name` / `Category name cannot contain '\'.\nCategory name cannot start/end with '/'.\nCategory name cannot contain '//' sequence.`
- Already exists → critical `Category creation error` / `Category with the given name already exists.\nPlease choose a different name and try again.`
- Success → `addCategory(name, options)`; missing parent categories in the `a/b/c` path are auto-created with default options.

**Effective path rules (core):**
- `categorySavePath(name)`: no category → global default save path. Category with explicit save path: absolute → used as is; relative → `<parent category's effective save path>/<path>`. No explicit path → `<parent category's effective save path>/<last segment made filesystem-valid>`.
- `categoryDownloadPath(name)`: resolve option (explicit, else inherit up the chain; root = global enable flag). Disabled → empty. Enabled: explicit path (absolute or relative to parent's effective download path, computed as if enabled) or implicit `<parent download path or global download path>/<last segment>`.

Edit (`Edit category...`): opened window-modal (async `open()`); OK → `setCategoryOptions(name, options)` (torrents using ATM are relocated by core).

### 3.22 Tags dialogs

**Torrent Tags dialog** (`torrenttagsdialog.ui/.cpp`) — from the Add New Torrent dialog / AddTorrentParamsWidget `...` button:
- Title `Torrent Tags`. Scroll area with a flow layout of check boxes, one per tag in (initial tags ∪ all session tags), checked if in the initial set; `&` in tag names is escaped as `&&` for display. Final item: `+` push button.
- `+` → loop AutoExpandableDialog titled `Add tag`, label `Tag:` (prefilled with the previously rejected text): empty/cancel stops; `Tag::isValid()` false → `Invalid tag name` / `Tag name '%1' is invalid.`; already exists in session → `Tag exists` / `Tag name already exists.`; else a new **checked** check box is appended before `+`. (Tags are not created in the session until used.)
- OK returns the set of checked tags.

**Add tags (transfer list › Tags › `Add...`)**: AutoExpandableDialog titled `Add tags`, label `Comma-separated tags:`; input trimmed, split on `,` (empty parts skipped), each part trimmed into a Tag; any invalid → warning `Invalid tag` / `Tag name: '%1' is invalid` and re-prompt. Tags are created if needed and added to all selected torrents.

**Remove all tags**: question `Remove All Tags` / `Remove all tags from selected torrents?` (Yes/No).

**Tag validity** (`Tag::isValid`): after trimming whitespace, non-empty and contains no `,`.

### 3.23 Search plugins dialog (`search/pluginselectdialog.ui/.cpp`, `pluginsourcedialog.ui/.cpp`) — Search tab `Search plugins...`

- Title `Search plugins`. Label `Installed search plugins:`. Tree (no root decoration, sortable, first section movable, sorted by Name ascending) with columns `Name` (plugin full name + favicon), `Version`, `Url`, `Enabled` (`Yes` / `No`); hidden ID column (plugin short name). Enabled rows colored **green**, disabled rows **red**.
- Warning label: `Warning: Be sure to comply with your country's copyright laws when downloading torrents from any of these search engines.`
- Link label: `You can get new search engine plugins here: <a href="https://plugins.qbittorrent.org">https://plugins.qbittorrent.org</a>`
- Buttons: `Install a new one`, `Check for updates`, `Close`.
- Context menu (on selection): checkable `Enabled` (reflects first selected; toggling applies to all selected), separator, `Uninstall`. Double-click toggles enabled state.
- Uninstall: built-in plugins cannot be removed — they are disabled instead and warning `Uninstall warning` / `Some plugins could not be uninstalled because they are included in qBittorrent. Only the ones you added yourself can be uninstalled.\nThose plugins were disabled.`; otherwise info `Uninstall success` / `All selected plugins were uninstalled successfully`.
- Install: `Install a new one` opens **Plugin source** dialog (title `Plugin source`, label `Search plugin source:`, buttons `Local file`, `Web link`).
  - Local file: `QFileDialog::getOpenFileNames` titled `Select search plugins`, filter `qBittorrent search plugin (*.py)`, multi-select.
  - Web link: AutoExpandableDialog `New search engine plugin URL` / `URL:`, default text = clipboard if it has a supported scheme and ends with `.py`, else `http://`; repeat with warning `Invalid link` / `The link doesn't seem to point to a search engine plugin.` while the URL doesn't end with `.py`.
  - Drag-and-drop of files/URLs (or plain-text list) onto the dialog installs each.
  - Results: success → plugin row added; aggregated info `Search plugin update` / `Plugins installed or updated: %1` (sorted, comma-joined) when all pending ops finish; failure → `Search plugin install` / `Couldn't install "%1" search engine plugin. %2`.
- Check for updates: wait cursor; no updates → `Search plugin update` / `All your plugins are already up to date.`; failure → `Search plugin update` / `Sorry, couldn't check for plugin updates. %1`; else each outdated plugin is updated; failure → `Search plugin update` / `Couldn't update "%1" search engine plugin. %2`.
- Favicons: if `<plugin>.png/.ico` is missing, `<plugin url>/favicon.ico` is downloaded, validated as decodable image, saved as `<pluginsLocation>/<id>.ico|.png`.

### 3.24 RSS Feed Options dialog (`rss/rssfeeddialog.ui/.cpp`) and RSS reader prompts

**RSS Feed Options** — used for `New subscription...` and `Feed options...`:
- Title `RSS Feed Options`. `URL:` line edit; `Refresh interval:` spin box (seconds, suffix ` sec`, max INT_MAX, adaptive step; value `0` shown as special text `Default` = use global RSS refresh interval).
- OK disabled while URL is empty.
- New subscription: URL prefilled from clipboard if the clipboard text has a supported network scheme, else `https://`. The feed is created inside the currently selected folder (or the parent folder of a selected feed; root if nothing/"Unread" selected) with path `<folder>\<url>` (feed display name initially = URL). On error (e.g. duplicate URL / path), warning box titled `qBittorrent` with the core error and the dialog re-opens with the same values. The new feed is selected and its folder expanded.
- Feed options (single feed selected): prefilled URL + refresh interval; OK → `setRefreshInterval(sec)` then `Session::setFeedURL(feed, url)`; error → warning `qBittorrent` / core message.

**Other RSS reader prompts:**
- New folder: AutoExpandableDialog `Please choose a folder name` / `Folder name:`, default `New folder`; created under the selected folder (same rules as above); error → warning `qBittorrent` / core error.
- Rename (feed or folder; F2 or double-click on feed list): AutoExpandableDialog `Please choose a new name for this RSS feed` / `New feed name:` prefilled with current name; loops on error with warning `Rename failed` / core message. The sticky "Unread" item cannot be renamed.
- Delete (Delete key or context menu): question `Deletion confirmation` / `Are you sure you want to delete the selected RSS feeds?` (default No); the "Unread" item is never deleted.

### 3.25 RSS Downloader dialog (`rss/automatedrssdownloader.ui/.cpp`, `base/rss/rss_autodownloadrule.cpp`) — RSS tab `RSS Downloader...`

Title `RSS Downloader`. Opened window-modal. Three-pane splitter (rule list | rule definition + feeds | matching articles preview; only the preview pane is collapsible). Warning banner (hidden when auto-downloading is enabled; tracks live state): `Auto downloading of RSS torrents is currently disabled. You can enable it in application settings.`

#### 3.25.1 Rule list (left)
- Header label `Download Rules`; tool buttons: rename (tooltip `Rename selected rule. You can also use the F2 hotkey to rename.`, enabled only with exactly one selection), remove, add.
- List of rule names with a **check box = rule enabled**; sortable; multi-select. Double-click or F2 → rename; Delete key → remove.
- Context menu: `Add new rule...`; if one selected: `Delete rule`, separator, `Rename rule...`; if several: `Delete selected rules`; then (any selection) separator, `Clear downloaded episodes...`.
- Add: AutoExpandableDialog `New rule name` / `Please type the name of the new download rule.`; empty → nothing; existing name → warning `Rule name conflict` / `A rule with this name already exists, please choose another name.`; creates a default rule (enabled).
- Remove: question `Rule deletion confirmation` with `Are you sure you want to remove the download rule named '%1'?` (one) or `Are you sure you want to remove the selected download rules?` (many).
- Rename: loop AutoExpandableDialog `Rule renaming` / `Please type the new rule name` (trimmed; empty cancels); conflict → `Rule name conflict` warning and re-prompt.
- Clear downloaded episodes: question `Clear downloaded episodes` / `Are you sure you want to clear the list of downloaded episodes for the selected rule?` → clears `previouslyMatchedEpisodes` of the current rule.

#### 3.25.2 Rule definition (enabled only when exactly one rule is selected; edits auto-saved when selection changes or the dialog closes)

| Label | Widget | Rule field (JSON key) | Notes |
|---|---|---|---|
| `Priority:` | int spin, full int range | `priority` (int, default 0) | Rules are evaluated in **ascending** priority order; the first enabled rule that matches an article (and whose feed list contains the article's feed) downloads it — later rules are not consulted for that article. |
| `Use Regular Expressions` | check box | `useRegex` (false) | Switches tooltips and validation of the two text filters. |
| `Must Contain:` | line edit + status icon | `mustContain` (string) | Wildcard mode: split on `\|` into alternatives (OR); within an alternative, whitespace-separated tokens are ANDed; `?` = one char, `*` = any chars; matching is case-insensitive. Regex mode: one Perl-compatible regex (case-insensitive). Empty = no condition. Invalid → text turns red (`#ff0000`) + warning icon; regex mode tooltip `Position %1: %2`. |
| `Must Not Contain:` | line edit + status icon | `mustNotContain` | Same syntax; article rejected if **any** alternative matches. |
| `Episode Filter:` | line edit + status icons | `episodeFilter` | Valid if empty or matches `^(^\d{1,4}x(\d{1,4}(-(\d{1,4})?)?;){1,}){1,1}` (case-insensitive). Invalid → red text + warning icon. Tooltip (see below). |
| `Use Smart Episode Filter` | check box | `smartFilter` (false) | Tooltip: `Smart Episode Filter will check the episode number to prevent downloading of duplicates.\nSupports the formats: S01E01, 1x1, 2017.12.31 and 31.12.2017 (Date formats also support - as a separator)` |
| `Ignore Subsequent Matches for (0 to Disable)` | spin 0..365, suffix ` days`, 0 shows `Disabled` | `ignoreDays` | After a match, further matches are ignored until `lastMatch + ignoreDays` days. |
| (right-aligned label) | | `lastMatch` (RFC 2822 date string in JSON) | `Last Match: %1 days ago` or `Last Match: Unknown`. |
| group `Torrent parameters` | **AddTorrentParamsWidget** (3.2) | `torrentParams` (object) | Category, save path, TMM, download path, tags, content layout, skip check, start torrent, stop condition, queue top, share limits. |
| `Apply Rule to Feeds:` | list of all feeds with check boxes (auto-tristate) | `affectedFeeds` (array of feed URLs) | With several rules selected, a feed shows Checked if in all, Partial if in some; toggling applies to all selected rules. Hidden/disabled when no rule selected. Sorted by name. |

Must Contain / Must Not Contain tooltips (verbatim, HTML):
- Regex mode: `Regex mode: use Perl-compatible regular expressions`
- Wildcard mode: `Wildcard mode: you can use` • `? to match any single character` • `* to match zero or more of any characters` • `Whitespaces count as AND operators (all words, any order)` • `| is used as OR operator` / `If word order is important use * instead of whitespace.`
- Both then append: `An expression with an empty | clause (e.g. expr|)` + ` will match all articles.` (Must Contain) / ` will exclude all articles.` (Must Not Contain).

Episode Filter tooltip (verbatim pieces, HTML list):
`Matches articles based on episode filter.` / `Example: ` **`1x2;8-15;5;30-;`** ` will match 2, 5, 8 through 15, 30 and onward episodes of season one` / `Episode filter rules: ` • `Season number is a mandatory non-zero value` • `Episode number is a mandatory positive value` • `Filter must end with semicolon` • `Three range types for episodes are supported: ` ◦ `Single number: <b>1x25;</b> matches episode 25 of season one` ◦ `Normal range: <b>1x25-40;</b> matches episodes 25 through 40 of season one` ◦ `Infinite range: <b>1x25-;</b> matches episodes 25 and upward of season one, and all episodes of later seasons`.

(Core matching semantics for the episode filter and smart filter regexes are specified in the Core behaviours section; relevant patterns from `rss_autodownloadrule.cpp`: per-episode `\bs0?(\d{1,4})[ -_\.]?e(0?\d{1,4})(?:\D|\b)` and `\b(\d{1,4})x(0?\d{1,4})(?:\D|\b)`; single-episode `\b(?:s0?%1[ -_\.]?e0?%2|%1x0?%2)(?:\D|\b)`.)

#### 3.25.3 Matching RSS Articles (right)
Header label `Matching RSS Articles`. Tree: one bold top-level item per feed (folder icon, tooltip = feed name) with children = titles of that feed's **currently stored** articles that the rule(s) match (`rule.matches(articleData)`, which applies must/must-not/episode/smart filters but not "already downloaded" side effects). Recomputed live on every edit; covers all selected rules (deduplicated by (feed, title)); sorted ascending.

#### 3.25.4 Import / Export (`&Import...`, `&Export...` buttons)
- File filters: `Rules (*.json)` and `Rules (legacy) (*.rssrules)`; extension appended according to the chosen filter.
- Export with no rules → warning `Invalid action` / `The list is empty, there is nothing to export.` Save dialog `Export RSS rules` (home dir). Write error → critical `I/O Error` / `Failed to create the destination file. Reason: %1`.
- Import: open dialog `Import RSS rules`; max file size 10 MiB; read error → critical `Import error` / `Failed to read the file. %1`; parse error → `Import error` / `Failed to import the selected rules file. Reason: %1` (legacy parse error message `Invalid data format`). Imported rules overwrite existing rules with the same name.
- **JSON format**: top-level object `{ "<rule name>": { ... } }` with keys `enabled` (bool), `priority` (int), `useRegex`, `mustContain`, `mustNotContain`, `episodeFilter`, `affectedFeeds` (array of URLs; a single string also accepted on import), `lastMatch` (RFC 2822 string), `ignoreDays` (int), `smartFilter` (bool), `previouslyMatchedEpisodes` (array of strings), deprecated duplicates `addPaused` (bool|null), `torrentContentLayout` (`"Original"`/`"Subfolder"`/`"NoSubfolder"`|null), `savePath`, `assignedCategory`, and `torrentParams` (object with keys `category`, `tags` (array), `save_path`, `download_path`, `use_download_path`, `operating_mode` (`"AutoManaged"`/`"Forced"`), `add_to_top_of_queue`, `stopped`, `stop_condition` (`"None"`/`"MetadataReceived"`/`"FilesChecked"`), `skip_checking`, `content_layout`, `use_auto_tmm`, `upload_limit`, `download_limit`, `seeding_time_limit`, `inactive_seeding_time_limit`, `share_limit_action` (enum name), `ratio_limit`, `ssl_certificate`, `ssl_private_key`, `ssl_dh_params`; optional keys omitted when unset). Import defaults: `enabled` true, `priority` 0; if `torrentParams` missing, legacy keys are used (non-empty `savePath` implies `use_auto_tmm=false`; old `createSubfolder` true→Original, false→NoSubfolder).
- **Legacy format** (`.rssrules`): `QDataStream` (version Qt_4_5) serialized `QVariantHash` `{ruleName: {name, must_contain, must_not_contain, save_path, affected_feeds, enabled, category_assigned, use_regex, add_paused (int: 0 = default, 1 = always, 2 = never), episode_filter, last_match, ignore_days}}`.

#### 3.25.5 Persistence
Dialog size `RssFeedDownloader/geometrySize`; splitters `GUI/Qt6/RSSFeedDownloader/HSplitterSizes`, `GUI/Qt6/RSSFeedDownloader/RuleDefSplitterState`. Rules themselves are stored by the core in `rss/download_rules.json` (same JSON format as export).

### 3.26 Exit / shutdown confirmation (`shutdownconfirmdialog.ui/.cpp`)

Used when "auto shutdown/suspend/hibernate/reboot/exit when downloads complete" fires (unless `dontConfirmAutoExit`). Always-on-top, centered, warning icon, **15-second countdown** (1 s timer); reaching 0 auto-accepts. Cancel is the default/focused button.

| Action (`ShutdownDialogAction`) | int | Message | OK button | Title |
|---|---|---|---|---|
| Exit | 0 | `qBittorrent will now exit.` | `E&xit Now` | `Exit confirmation` |
| Shutdown | 1 | `The computer is going to shutdown.` | `&Shutdown Now` | `Shutdown confirmation` |
| Suspend | 2 | `The computer is going to enter suspend mode.` | `&Suspend Now` | `Suspend confirmation` |
| Hibernate | 3 | `The computer is going to enter hibernation mode.` | `&Hibernate Now` | `Hibernate confirmation` |
| Reboot | 4 | `The computer is going to reboot.` | `&Reboot Now` | `Reboot confirmation` |

Text = message + `\n` + `You can cancel the action within %1 seconds.` (updated each second). Check box `Don't show again` visible **only for Exit**; on accept its state is saved to `dontConfirmAutoExit`.

### 3.27 Preview selection dialog (`previewselectdialog.ui/.cpp`) — context menu `Pre&view file...` / transfer-list double-click action "Preview file" (falls back to opening the destination folder when nothing is previewable)

- If the torrent has no previewable files → critical `Unable to preview` / `The selected torrent "%1" does not contain previewable files`.
- Previewable = MIME type (by extension) starts with `audio` or `video`, or extension (upper-cased) in: `.3GP .AAC .AC3 .AIF .AIFC .AIFF .ASF .AU .AVI .FLAC .FLV .M3U .M4A .M4P .M4V .MID .MKV .MOV .MP2 .MP3 .MP4 .MPC .MPE .MPEG .MPG .MPP .OGG .OGM .OGV .QT .RA .RAM .RM .RMV .RMVB .SWA .SWF .TS .VOB .WAV .WMA .WMV`.
- Title `Preview selection`; label `The following files from torrent "%1" support previewing, please select one of them:`; tree columns `Name` (file name), `Size` (friendly; sorts by bytes), `Progress` (progress-bar delegate; sorts numerically); hidden file-index column; sorted by Name; first row selected; Name column initially 60 % of width; header context menu with column visibility + `Resize columns` (tooltip `Resize all non-hidden columns to the size of their contents`).
- OK relabeled `Preview`; double-click = Preview. **If only one previewable file, it is previewed immediately without showing a choice.**
- Preview: flush the torrent's disk cache, compute `actualStorageLocation / actualFilePath(index)` (includes `.!qB` suffix if present); missing → critical `Preview impossible` / `Sorry, we can't preview this file: "%1".`; else open with the system default application.

### 3.28 Remove torrent(s) confirmation (`deletionconfirmationdialog.ui/.cpp`)

Shown when Options › Behavior `Shows a confirmation dialog upon torrent deletion` (`confirmTorrentDeletion`) is on; otherwise removal happens immediately with the requested mode.
- Title `Remove torrent(s)`; large warning icon; text `Are you sure you want to remove '%1' from the transfer list?` (one) / `Are you sure you want to remove these %1 torrents from the transfer list?` (many).
- Check box `Also remove the content files` — initially checked if the action was "remove with files" (e.g. Shift+Delete) **or** preference `removeTorrentContent` is true.
- Lock button (tooltip `Remember choice`) — enabled only when the check box state differs from the stored preference; click saves it as the new default (`setRemoveTorrentContent`) and disables itself.
- Buttons: OK relabeled `Remove`, Cancel (focused by default).

### 3.29 UI Theme Configuration (`uithemedialog.ui/.cpp`) — Options › Behavior `Customize UI Theme...`

- Enabled only when "Use custom UI Theme" is unchecked (customizes the built-in default theme). Title `UI Theme Configuration`.
- Tab `Colors`: note `Note that some custom colors may not be applied in some desktop environments.`; grid columns `Color ID`, `Light Mode`, `Dark Mode`. Each cell is a color swatch button with menu `Edit...` (color picker) / `Reset`; empty default shows `System`. Color IDs (sorted) with defaults (light / dark): `Log.TimeStamp` (#6e7781 / #6e7681), `Log.Normal` (system), `Log.Info` (#0969da / #58a6ff), `Log.Warning` (#bc4c00 / #db6d28), `Log.Critical` (#cf222e / #f85149), `Log.BannedPeer` (#cf222e / #f85149), `RSS.ReadArticle` (system), `RSS.UnreadArticle` (system), `TransferList.Downloading` (#1a7f37 / #3fb950), `TransferList.StalledDownloading` (#2da44e / #238636), `TransferList.DownloadingMetadata` (#1a7f37 / #3fb950), `TransferList.ForcedDownloadingMetadata` (#1a7f37 / #3fb950), `TransferList.ForcedDownloading` (#1a7f37 / #3fb950), `TransferList.Uploading` (#0969da / #58a6ff), `TransferList.StalledUploading` (#0969da / #1f6feb), `TransferList.ForcedUploading` (#0969da / #58a6ff), `TransferList.QueuedDownloading` (#7d4e00 / #845306), `TransferList.QueuedUploading` (#7d4e00 / #845306), `TransferList.CheckingDownloading` (#1a7f37 / #3fb950), `TransferList.CheckingUploading` (#1a7f37 / #3fb950), `TransferList.CheckingResumeData` (#1a7f37 / #3fb950), `TransferList.StoppedDownloading` (#57606a / #8b949e), `TransferList.StoppedUploading` (#8250df / #a371f7), `TransferList.Moving` (#1a7f37 / #3fb950), `TransferList.MissingFiles` (#cf222e / #f85149), `TransferList.Error` (#cf222e / #f85149), `PiecesBar.Border`, `PiecesBar.Piece`, `PiecesBar.PartialPiece`, `PiecesBar.MissingPiece` (system), and palette roles `Palette.Window`, `Palette.WindowText`, `Palette.Base`, `Palette.AlternateBase`, `Palette.Text`, `Palette.ToolTipBase`, `Palette.ToolTipText`, `Palette.BrightText`, `Palette.Highlight`, `Palette.HighlightedText`, `Palette.Button`, `Palette.ButtonText`, `Palette.Link`, `Palette.LinkVisited`, `Palette.Light`, `Palette.Midlight`, `Palette.Mid`, `Palette.Dark`, `Palette.Shadow`, `Palette.WindowTextDisabled`, `Palette.TextDisabled`, `Palette.ToolTipTextDisabled`, `Palette.BrightTextDisabled`, `Palette.HighlightedTextDisabled`, `Palette.ButtonTextDisabled` (all system default).
- Tab `Icons`: columns `Icon ID`, `Light Mode`, `Dark Mode`; each cell menu `Browse...` (file dialog `Select icon`, filter `Supported image files (*.svg *.png)`) / `Reset`.
- OK writes only overridden values to `<config dir>/themes/default/config.json` (`{"colors.light": {id: "#rrggbb"}, "colors.dark": {...}}`) and copies icons under `themes/default/icons/{light,dark}/<id>.svg|png`. Errors logged (`Couldn't save UI Theme configuration. Reason: %1`, `Couldn't remove icon file. File: %1.`, `Couldn't copy icon file. Source: %1. Destination: %2.`) and summarized in critical box `UI Theme Configuration.` / `The UI Theme changes could not be fully applied. The details can be found in the Log.` Changes apply after restart.

### 3.30 Crash info (`stacktracedialog.ui/.cpp`)

Title `Crash info`. Shown by the signal handler on fatal signals (if enabled at build). HTML: big red `qBittorrent has crashed`, `Please file a bug report at https://bugs.qbittorrent.org and provide the following information:`, then `qBittorrent version: v5.2.4 (64-bit)`, `Libtorrent version:`, `Qt version:`, `Boost version:`, `OpenSSL version:`, `zlib version:`, `OS version:`, `Caught signal: <name>`, and the stack trace in a fenced code block (selectable for copy).

### 3.31 Legal notice (`app/legalnotice.cpp`, `app/main.cpp`)

- Shown on first start unless `LegalNotice/Accepted` (default false) is true or the command line contains `--confirm-legal-notice` (which also sets it true).
- GUI: modal message box (no icon), centered, title `Legal Notice`, text `qBittorrent is a file sharing program. When you run a torrent, its data will be made available to others by means of upload. Any content you share is your sole responsibility.` + blank line + `No further notices will be issued.`, single OK. After showing (interactive) the flag is set true.
- Non-interactive (headless, no TTY): prints notice plus `If you have read the legal notice, you can use command line option `--confirm-legal-notice` to suppress this message.` and does **not** set the flag. Interactive console: prints and waits for `Press 'Enter' key to continue...`.

### 3.32 Program updater (Windows/macOS only; `programupdater.cpp`, `mainwindow.cpp`)

- Menu action `&Check for Updates` (tooltip `Check for program updates`); while running: text `Checking for Updates...`, tooltip `Already checking for program updates in the background`, disabled.
- Automatic: if Options › Behavior "Check for program updates" (`isUpdateCheckEnabled`) is on, one check at startup and then a single-shot timer every **24 h** (restarted after each check).
- Sources queried in parallel (proxy per "general purposes"): FossHub RSS `https://www.fosshub.com/feed/5b8793a7f9ee5a5c3e97a3b2.xml` (user agent `qBittorrent/5.2.4 ProgramUpdater (www.qbittorrent.org)`; picks `<item>` whose `<type>` equals the build variant, e.g. `Mac OS X` or `Windows x64`, or `Mac OS X (qt6 lt20)`-style when not Qt6+lt1; reads `<version>` and `<link>`), `https://www.qbittorrent.org/versions.json` and backup `https://qbittorrent.github.io/qBittorrent-website/versions.json` (JSON `{ "macos": {"version": "x.y.z"}, "win": {...} }`).
- A remote version counts if it is valid and > current (or == current while running an alpha/beta/rc build). The max of the three wins; failures logged `Failed to download the program update info. URL: "%1". Error: "%2"`.
- New version: non-modal question box `qBittorrent Update Available`: `A new version is available.<br/>Do you want to download %1?<br/><br/><a href="https://www.qbittorrent.org/news">Open changelog...</a>` (Yes default). Yes opens the FossHub link or `https://www.qbittorrent.org/download` / `https://qbittorrent.github.io/qBittorrent-website/download` in the browser (no self-update).
- None and user-invoked: info `qBittorrent` / `No updates available.\nYou are already using the latest version.`

### 3.33 Startup progress dialog (`app/application.cpp`)

Shown at startup while the session restores torrents (unless starting hidden to tray, in which case it appears only if the user activates the app before loading finishes; 1 s minimum duration to avoid flashing; minimized if starting minimized): `QProgressDialog` with label `Loading torrents...`, cancel button `Exit` (quits the app), range 0..100 fed by `Session::startupProgressUpdated`, shown immediately, minimizable. Closed when the main window is created.

### 3.34 Other main-window message boxes & prompts

| Trigger | Dialog | Strings / behaviour |
|---|---|---|
| Close/quit with active torrents and Options › Behavior `Shows a confirmation dialog when exiting with active torrents` (`confirmOnExit`) | custom message box | Title `Exiting qBittorrent`; text `Some files are currently transferring.\nAre you sure you want to quit qBittorrent?`; buttons `&No` (default), `&Yes`, `&Always Yes` (sets `confirmOnExit=false`). Window is shown first if hidden. |
| Lock › `&Set Password` | AutoExpandableDialog (password echo) | `UI lock password` / `Please type the UI lock password:`; length < 3 → warning `Invalid password` / `The password must be at least 3 characters long`; stored as PBKDF2 hash. |
| Lock › `&Clear Password` | question | `Clear the password` / `Are you sure you want to clear the password?` (default No). |
| Unlock (restore from tray when UI locked) | AutoExpandableDialog (password) | `UI lock password` / `Please type the UI lock password:`; wrong → `Invalid password` / `The password is invalid`. |
| View › Search engine enabled without Python | info (macOS/Linux) / question (Windows offers install) | `Missing Python Runtime` / `Python is required to use the search engine but it does not seem to be installed.` (+ Windows: `\nDo you want to install it now?`) |
| Python older than **3.9.0** | info / question | `Old Python Runtime` / `Your Python version (%1) is outdated. Please upgrade to latest version for search engines to work.\nMinimum requirement: %2.` (Windows: `Your Python version (%1) is outdated. Minimum requirement: %2.\nDo you want to install a newer version now?`) |
| Force recheck with `confirmTorrentRecheck` on | question | `Recheck confirmation` / `Are you sure you want to recheck the selected torrent(s)?` (default Yes). |
| Enable ATM from context menu | question | `Enable automatic torrent management` / `Are you sure you want to enable Automatic Torrent Management for the selected torrent(s)? They may be relocated.` (default Yes). Disabling has no prompt. |
| Export .torrent | directory dialog `Choose folder to save exported .torrent files` | Each torrent exported as `<valid file name>.torrent`, or `<name> (N).torrent` if taken. Failures logged `Export .torrent file failed. Torrent: "%1". Save path: "%2". Reason: "%3"` and summarized: warning `Export .torrent file error` / `Errors occurred when exporting .torrent files. Check execution log for details.` Tooltip of the action: `Exported torrent is not necessarily the same as the imported`. |
| New category from context menu (`&New...`) | Category dialog (3.21) | new category assigned to selection on success. |
| Search tab: empty pattern | critical | `Empty search pattern` / `Please type a search pattern first`. |
| Search with Python missing | desktop notification | `Search Engine` / `Please install Python to use the Search Engine.` |
| Windows Python installer download failure | warning | `Download error` / `Python installer could not be downloaded. Error: %1.\nPlease install it manually.` |

Desktop notifications (not dialogs, listed for completeness): `Torrent added` / `'%1' was added.` (if enabled), `Download completed` / `'%1' has finished downloading.`, `I/O Error` / `An I/O error occurred for torrent '%1'.\n Reason: %2`, `Add torrent failed` / `Couldn't add torrent '%1', reason: %2.`

### 3.35 RSS reader tab (`rss/rsswidget.ui/.cpp`, `feedlistwidget.cpp`, `articlelistwidget.cpp`)

(Main-window tab `RSS (%1)` where %1 = total unread count; toggled by View › `&RSS Reader`.)
- Warning banner when RSS processing is disabled: `Fetching of RSS feeds is disabled now! You can enable it in application settings.`
- Toolbar: `New subscription`, `Mark items read`, `Update all` (tooltip `Refresh RSS streams`), filter line edit (max width 200, placeholder `Filter feed items...`; case-insensitive substring on article title), `RSS Downloader...`.
- Left: feed tree, header `RSS feeds`; first sticky item `Unread  (%1)` (inbox icon; shows all unread articles across feeds); folders and feeds shown as `<name>  (<unread count>)`. Feed icon: loading spinner while fetching, `task-reject` on error, else downloaded favicon (fallback `application-rss`). Folder open/closed state persisted (`Preferences::RssOpenFolders`, `\`-separated paths). Splitter states persisted (`RssSideSplitterState`, `RssMainSplitterState`).
- Feed tree context menu: with selection → `Update`, `Mark items read`, separator, [single non-Unread: `Rename...`, (feed) `Feed options...`, `Delete`, separator, (folder) `New folder...`] or [multiple: `Delete`, separator], `New subscription...`, [if first selected is a feed: separator, `Copy feed URL`]. Without selection → `New subscription...`, `New folder...`, separator, `Update all feeds`. F2 renames; Delete deletes; double-click renames.
- Middle: label `Torrents: (double-click to download)`; article list (multi-select). Unread items in `RSS.UnreadArticle` color (default link color) with "unread" icon; read in `RSS.ReadArticle` (default inactive text) with "read" icon. Newest articles inserted at top. An article is marked read when it stops being the current item, when downloaded, or when its link is opened.
- Article context menu: `Download torrent` (if any selected article has a torrent URL), `Open news URL` (if any has a link). Double-click = download. Download only allows HTTP(S) URLs, magnet URIs and info-hashes; others are blocked: log `Blocked adding torrent from RSS article. Unsupported torrent URL. Only HTTP(S) URLs, magnet URIs and info hashes are supported. Article: "%1". URL: "%2".` + warning box `Blocked adding torrent from RSS article. The following article has an unsupported torrent URL and it may be malicious behaviour:\n%1` (+ `There are %1 more articles with the same issue.`). Open URL refuses `file:` URLs (`Blocked opening RSS article URL. ...`) and warns on empty links (`The following article has no news URL provided:\n%1`).
- Right: article viewer (HTML): header box (red 2px border) with title (highlight colors), `Date: ` (local short format), `Feed: ` (only in the Unread view), `Author: `, `Open link`; then description — rich HTML as-is, else plain text with BBCode converted (`[img]`, `[url=]`, `[b|i|u|s]`, `[color=]`, `[size=]`) inside `<pre>`. Relative URLs are made absolute using the article link's scheme+host.

### 3.36 Search tab (`search/searchwidget.ui/.cpp`, `searchjobwidget.ui/.cpp`)

(Main-window tab `Search`; toggled by View › `Search &Engine`; requires Python ≥ 3.9.0.)
- Top row: pattern line edit with completer from search history (tooltip: `A phrase to search for.` / `Spaces in a search term may be protected by double quotes.` / `Example:` / `<b>foo bar</b>: search for <b>foo</b> and <b>bar</b>` / `<b>&quot;foo bar&quot;</b>: search for <b>foo bar</b>`), category combo (`All categories`, separator, then categories supported by the selected plugins, localized: `Anime`, `Books`, `Games`, `Movies`, `Music`, `Pictures`, `Software`, `TV shows`), plugin combo (`Only enabled`, `All plugins`, `Select...` (opens Search plugins dialog), separator, each enabled plugin by full name), `Search` / `Stop` button (toggle). Shortcuts: `Ctrl+F` / `Ctrl+E` toggle focus between pattern and results filter; `Ctrl+W` (Close) closes current tab.
- Empty state: `There aren't any search plugins installed.\nClick the "Search plugins..." button at the bottom right of the window to install some.`; bottom-right button `Search plugins...`.
- Each search opens a tab (icon by status; tooltip = status text: `Searching...`, `Search has finished`, `Search aborted`, `An error occurred during search...`, `Search returned no results`). Tab context menu: `Refresh tab` (when not running) or `Stop search`, separator, `Close tab`, `Close all tabs`.
- Result tab: label `Results (showing <i>%1</i> out of <i>%2</i>):`; filter line edit `Filter search results...` (wildcards, or regex if context-menu `Use regular expressions` is checked); `Search in:` combo `Torrent names only` (default) / `Everywhere` (tooltip explains description matches); `Seeds:` min spin `to` max spin (max `∞`); `Size:` min (default 0 MiB) `to` max (default −1 = `∞`, GiB) with unit combos `B, KiB, MiB, GiB, TiB, PiB, EiB`.
- Result columns: `Name`, `Size`, `Seeders`, `Leechers`, `Engine`, `Engine URL`, `Published On`; header menu with `Column visibility` and `Resize columns`. Visited (downloaded) rows recolored.
- Result context menu: `Open download window` (add with dialog), `Download` (skip dialog), separator, `Open description page`, `Copy` › `Name`, `Download link`, `Description page URL`. Double-click = download (respecting the Add dialog preference). Non-magnet links are fetched through the plugin's `nova2dl.py`. Description pages pointing to local files are blocked (`Blocked opening search result description page URL...`); missing ones warn `Entry "%1" has no description page URL provided.`
- Persistence (`<data>/SearchUI/`): `History.txt` (search history, length = Options › Search "search history length" spin box), `Session.json` (`{"Tabs":[{"ID","SearchPattern"}], "CurrentTab"}`) when Options › Search "store opened tabs" group is checked, and per-tab result files with keys `FileName`, `FileURL`, `FileSize`, `SeedersCount`, `LeechersCount`, `EngineName`, `SiteURL`, `DescrLink`, `PubDate` when "store opened tabs results" is on.

---

### 3.37 Import from qBittorrent or Transmission (qBit NeoRS extension — not in qBittorrent)

File menu, after "Add Torrent Link…": "Import from qBittorrent…" and "Import from Transmission…" (also in the Command Palette; hidden in demo and snapshot runs). On the first launch with a profile that has no torrents, the same sheet is offered once (defaults key `qBitNeoRS/ImportOfferShown`) when qBittorrent 5.x or the Transmission app (macOS) has torrents on this Mac; with both, a segmented "Import from:" picker switches between them.

* The sheet runs a dry run of the migration (`docs/MIGRATION.md`) and shows counts: torrents, those whose progress carries over without a recheck, those checked when they start, magnets waiting for metadata, torrents already in the profile (merge), categories, tags, watched folders, RSS feeds and search plugins (qBittorrent), and whether the settings are taken over or kept. "Keep my current qBit NeoRS settings" switches to merge mode; it is on and fixed when the profile already has torrents. "Not Carried Over" lists what never transfers (localized) plus Transmission's remote access when it is on (its password is never copied). "Full Report" shows `migration/report.txt` as it would be written.
* The source application must be quit (checked by bundle id `org.qbittorrent.qBittorrent` / `org.m0k.transmission` before the dry run, before confirming and again at the restart). The app never quits it: the sheet shows "Quit <app> First" with "Check Again".
* "Import and Restart" records `<profile>/migration/pending-import.json` (valid for 15 minutes) and quits through the normal termination path (no exit confirmation; resume data saved, core stopped). Once the core is down, a detached `/bin/sh` waits for the process to exit and runs `open -n` on the bundle with the same launch arguments. The new instance applies the source's GUI settings to its defaults store before they load (full import only; previous values saved to `migration/gui-defaults-before-import.plist`), shows "Importing from <app>…", backs the profile up to `<profile>.before-migration-<time>`, migrates, and only then creates its core. A result sheet shows the counts, the backup folder ("Show Backup in Finder") and "Open Report". If the source application was opened meanwhile, the import is dropped with an alert.
* Why a restart and not an in-place session restart: the import rewrites `config/` and `data/BT_backup` of the running profile; with the old core gone, nothing can save resume data or statistics over the imported files afterwards, and the WebUI port, log file and engine files are free.

## 4. Core behaviors (engine)

This section specifies the behaviour of the qBittorrent 5.2.4 core (`src/base`, `src/app`) that is independent of any particular UI. File references are relative to `src/`. "lt" = libtorrent (qBittorrent 5.2.4 targets libtorrent 2.0.x; `QBT_USES_LIBTORRENT2` code paths are the ones to replicate). Settings are stored in the main INI file (see 4.17) under the key shown in `monospace`; enum-typed settings are stored as the enum **name string** (e.g. `Stop`, `Subfolder`), not the integer (`settingsstorage.h` uses `Utils::String::fromEnum`). The WebAPI exposes the same enums as integers (see `preferences.md`).

Identity strings (`base/bittorrent/sessionimpl.cpp`):
- Peer ID fingerprint: `lt::generate_fingerprint("qB", 5, 2, 4, 0)` → `-qB5240-` prefix.
- HTTP User-Agent for trackers / web seeds: `qBittorrent/5.2.4` (`USER_AGENT = "qBittorrent/" QBT_VERSION_2`).
- Default DHT bootstrap nodes: `dht.libtorrent.org:25401, dht.transmissionbt.com:6881, router.bittorrent.com:6881`.
- Startup log lines (Log::INFO): `Peer ID: "%1"`, `HTTP User-Agent: "%1"`, `Distributed Hash Table (DHT) support: ON|OFF`, `Local Peer Discovery support: ON|OFF`, `Peer Exchange (PeX) support: ON|OFF`, `Anonymous mode: ON|OFF`, `Encryption support: ON|FORCED|OFF`.
- Fixed lt settings: `listen_system_port_fallback=false`, `use_dht_as_fallback=false`, `auto_scrape_interval=1200` (20 min), `auto_scrape_min_interval=900` (15 min), `enable_set_file_valid_data=true`, UPnP/NAT-PMP disabled until PortForwarder enables them, `active_tracker_limit=-1`, `active_dht_limit=-1`, `active_lsd_limit=-1`, `alert_queue_size=INT_MAX/2`, `allowed_enc_level=pe_rc4`, `prefer_rc4=true`.
- lt plugins always loaded: smart_ban, ut_metadata; ut_pex only if PeX enabled **at startup** (toggling PeX logs `Restart is required to toggle Peer Exchange (PeX) support`, Log::WARNING).
- Alert mask: error, file_progress, ip_block, peer, port_mapping, status, storage, tracker, + performance_warning only if "PerformanceWarning" (advanced) is on.

### 4.1 Core enums (canonical integer values)

| Enum (source) | Values |
|---|---|
| `TorrentState` (`bittorrent/torrent.h`) | Unknown=-1, ForcedDownloading=0, Downloading=1, ForcedDownloadingMetadata=2, DownloadingMetadata=3, StalledDownloading=4, ForcedUploading=5, Uploading=6, StalledUploading=7, CheckingResumeData=8, QueuedDownloading=9, QueuedUploading=10, CheckingUploading=11, CheckingDownloading=12, StoppedDownloading=13, StoppedUploading=14, Moving=15, MissingFiles=16, Error=17 |
| `TorrentOperatingMode` (`torrent.h`) | AutoManaged=0, Forced=1 |
| `Torrent::StopCondition` (`torrent.h`) | None=0, MetadataReceived=1, FilesChecked=2 |
| `TorrentContentLayout` (`torrentcontentlayout.h`) | Original=0, Subfolder=1, NoSubfolder=2 |
| `ShareLimitAction` (`sharelimits.h`) | Default=-1 (special: inherit), Stop=0, Remove=1, EnableSuperSeeding=2, RemoveWithContent=3 |
| Share limit sentinels (`sharelimits.h`) | `DEFAULT_RATIO_LIMIT=-2`, `NO_RATIO_LIMIT=-1`, `DEFAULT_SEEDING_TIME_LIMIT=-2`, `NO_SEEDING_TIME_LIMIT=-1` (applies to both seeding-time and inactive-seeding-time) |
| `TorrentRemoveOption` (`session.h`) | KeepContent=0, RemoveContent=1 |
| `TorrentContentRemoveOption` (`torrentcontentremoveoption.h`) | Delete=0 ("Delete files permanently"), MoveToTrash=1 ("Move files to trash (if possible)") |
| `MoveStorageMode` (`torrentimpl.h`) | FailIfExist=0 (lt fail_if_exist), KeepExistingFiles=1 (lt dont_replace), Overwrite=2 (lt always_replace_files) |
| `MoveStorageContext` (`torrentimpl.h`) | AdjustCurrentLocation=0, ChangeSavePath=1, ChangeDownloadPath=2 |
| `MaintenanceJob` (`torrentimpl.h`) | None=0, HandleMetadata=1 |
| `DownloadPriority` (`downloadpriority.h`) | Ignored=0 ("Do not download"), Normal=1, High=6, Maximum=7, Mixed=-1 (UI-only aggregate) |
| `TrackerEndpointState` (`trackerentrystatus.h`) | NotContacted=1, Working=2, NotWorking=4, TrackerError=5, Unreachable=6 |
| `TorrentAnnounceStatusFlag` (`torrentannouncestatus.h`, bit flags) | HasNoProblem=0, HasWarning=1, HasTrackerError=2, HasOtherError=4 |
| `BTProtocol` (`session.h`) | Both=0 ("TCP and μTP"), TCP=1, UTP=2 |
| `ChokingAlgorithm` | FixedSlots=0, RateBased=1 |
| `SeedChokingAlgorithm` | RoundRobin=0, FastestUpload=1, AntiLeech=2 |
| `MixedModeAlgorithm` | TCP=0 ("Prefer TCP"), Proportional=1 ("Peer proportional (throttles TCP)") |
| `DiskIOType` | Default=0, MMap=1 ("Memory mapped files"), Posix=2 ("POSIX-compliant"), SimplePreadPwrite=3 ("Simple pread/pwrite") |
| `DiskIOReadMode` | DisableOSCache=0, EnableOSCache=1 |
| `DiskIOWriteMode` | DisableOSCache=0, EnableOSCache=1, WriteThrough=2 (lt2 only) |
| `ResumeDataStorageType` | Legacy=0 ("Fastresume files"), SQLite=1 ("SQLite database (experimental)") |
| `Net::ProxyType` (`net/proxyconfigurationmanager.h`) | None=0, HTTP=1, SOCKS5=2, SOCKS4=5 (3 and 4 were legacy HTTP_PW / SOCKS5_PW, migrated away) |
| Encryption (int setting `BitTorrent/Session/Encryption`) | 0 = "Allow encryption" (lt pe_enabled in+out), 1 = "Require encryption" (pe_forced), 2 = "Disable encryption" (pe_disabled; any value other than 0/1 is treated as disabled) |
| `Scheduler::Days` (`preferences.h`) | EveryDay=0, Weekday=1, Weekend=2, Monday=3, Tuesday=4, Wednesday=5, Thursday=6, Friday=7, Saturday=8, Sunday=9 |
| `TorrentFileGuard::AutoDeleteMode` (`torrentfileguard.h`) | Never=0, IfAdded=1, Always=2 |
| `ShutdownDialogAction` (`base/types.h`) | Exit=0, Shutdown=1, Suspend=2, Hibernate=3, Reboot=4 |
| `DNS::Service` (`preferences.h`) | DynDNS=0, NoIP=1, None=-1 |
| `Log::MsgType` (`logger.h`, flags) | ALL=-1, NORMAL=1, INFO=2, WARNING=4, CRITICAL=8 |
| `FileLogger::FileLogAgeType` (`app/filelogger.h`) | DAYS=0, MONTHS=1, YEARS=2 |
| `AddTorrentError::Kind` (`addtorrenterror.h`) | DuplicateTorrent=0, Other=1 |
| `TorrentFilter::Status` (`base/torrentfilter.h`) | All=0, Downloading=1, Seeding=2, Completed=3, Running=4, Stopped=5, Active=6, Inactive=7, Stalled=8, StalledUploading=9, StalledDownloading=10, Checking=11, Moving=12, Errored=13 |

Other constants: `MAX_ETA = 8640000` s (100 days, "∞"), `Torrent::MAX_RATIO = +infinity` (UI shows ∞ for ratio ≥ MAX_RATIO or -1), `QB_EXT = ".!qB"`, `UNWANTED_FOLDER_NAME = ".unwanted"` (`bittorrent/common.h`), `MAX_LOG_MESSAGES = 20000`.

### 4.2 Torrent identity

- `InfoHash` holds v1 (SHA-1, 20 bytes) and/or v2 (SHA-256, 32 bytes). A torrent is "hybrid" if it has both.
- `TorrentID` (40 hex chars) = `info_hashes.get_best()`: the SHA-1 for v1-only torrents, otherwise the **first 20 bytes of the SHA-256** (truncated v2) for v2-only and hybrid torrents. Used for resume file names, WebAPI `hash`, `%K`.
- Hybrid torrents are additionally indexed by their v1 hash as an "alternative ID" so that a hybrid torrent added by v1 magnet is found as the same torrent (`findTorrent`, `m_hybridTorrentsByAltID`). When a v1-only magnet later receives hybrid metadata, the torrent ID changes (`handleTorrentInfoHashChanged`) and the old resume record is deleted after the new one is written.
- Startup migration: resume data stored under a hybrid torrent's v1 ID is re-stored under the v2-based ID and the v1 record removed; if both exist, the v2 record wins (metadata merged from v1 record if missing). Inconsistent IDs → `Failed to resume torrent: inconsistent torrent ID is detected. Torrent: "%1"` (WARNING).
- On `lt::torrent_conflict_alert` (v1 and v2 halves of the same hybrid exist as separate torrents): the v1 torrent / metadata download is removed, metadata is set on the v2 torrent.

### 4.3 Adding torrents

#### 4.3.1 Sources (`base/addtorrentmanager.cpp`, `gui/guiaddtorrentmanager.cpp`, `bittorrent/torrentdescriptor.cpp`)
A "source" string is classified in this order:
1. **URL** with a scheme supported by the HTTP client (`http`, `https`, `ftp`, `data`, `file` …): downloaded via `Net::DownloadManager` with size limit = `.torrent file size limit` (`BitTorrent/TorrentFileSizeLimit`, default 100 MiB), proxied iff "Use proxy for general purposes". Log: `Downloading torrent... Source: "%1"`. Duplicate in-flight downloads of the same URL are ignored. Result: `.torrent` bytes, or HTTP redirect to `magnet:` (status `RedirectedToMagnet`) which is then parsed as a magnet.
2. **Magnet URI / bare hash**: `TorrentDescriptor::parse()`:
   - 64 hex chars → `magnet:?xt=urn:btmh:1220<hex>` (v2).
   - 40 hex chars or 32 Base32 chars (`[2-7A-Za-z]`) → `magnet:?xt=urn:btih:<hash>` (v1).
   - Anything else is passed to `lt::parse_magnet_uri`. Supported parameters (libtorrent semantics): `xt=urn:btih:<40hex|32base32>`, `xt=urn:btmh:1220<64hex>` (both may appear → hybrid), `dn` (display name → initial torrent name), `tr` (trackers; each successive `tr` gets the next tier), `ws` (web seeds), `x.pe` (peer `ip:port` to connect), `dht` (DHT node), `so` (select-only file indices, applied once metadata arrives). A string starting with `magnet:` that fails to parse is an error (`Failed to add torrent. Source: "%1". Reason: "%2"`).
3. **Local file** (optionally `file://` URL, percent-decoded): `TorrentDescriptor::loadFromFile()` with limits: max buffer = TorrentFileSizeLimit, bdecode depth = `BitTorrent/BdecodeDepthLimit` (default 100), bdecode tokens = `BitTorrent/BdecodeTokenLimit` (default 10,000,000). A `TorrentFileGuard` is attached (see 4.9.3).

Entry points: command line / second-instance messages (4.20), macOS `QFileOpenEvent` (file or URL, incl. `magnet:`), GUI dialogs, drag & drop, WebAPI `torrents/add`, watched folders (4.9, bypass AddTorrentManager and call `Session::addTorrent` directly), RSS auto-downloader, recursive download (4.10.4), search engine downloads.

If the GUI "Add new torrent" dialog is enabled (`AddNewTorrentDialog/Enabled`, default true, or forced by `--skip-dialog=false`), the GUI manager shows the dialog instead (one dialog per infohash; re-adding the same hash re-focuses it and, if metadata now known, updates it). For magnets without metadata the dialog calls `Session::downloadMetadata()` (4.3.5).

#### 4.3.2 `AddTorrentParams` (`bittorrent/addtorrentparams.h`) — per-add overrides
| Field | Type / default | JSON key (watched folders / API) | Meaning |
|---|---|---|---|
| name | string, "" | — | Explicit torrent name (overrides metadata/`dn`). |
| category | string, "" | `category` | Created on the fly if missing (`addCategory`); if invalid → cleared. |
| tags | set, {} | `tags` (array) | Missing tags are created; invalid tags dropped. |
| savePath | path, "" | `save_path` | Relative → resolved against the suggested save path (4.6). |
| useDownloadPath | optional bool | `use_download_path` | Unset → `true` if `downloadPath` given, else global "Keep incomplete torrents in" toggle. |
| downloadPath | path, "" | `download_path` | Relative → resolved against suggested download path (or global download path). |
| sequential | bool, false | — | lt `sequential_download` flag. |
| firstLastPiecePriority | bool, false | — | 4.13.3. |
| addForced | bool, false | `operating_mode` = `"Forced"`/`"AutoManaged"` | Add in Forced mode (bypass queue). |
| addToQueueTop | optional bool | `add_to_top_of_queue` | Unset → global `AddTorrentToTopOfQueue` (default false). |
| addStopped | optional bool | `stopped` | Unset → global `AddTorrentStopped` (default false; label "Do not start the download automatically"). |
| stopCondition | optional StopCondition | `stop_condition` (name string) | Unset → global `TorrentStopCondition` (default None). |
| filePaths | list | — | Per-file renamed paths (only with metadata). |
| filePriorities | list | — | Per-file priorities (only with metadata). Empty → Normal for all, then excluded-file-name filter (4.8.1). |
| skipChecking | bool, false | `skip_checking` | lt `seed_mode`; torrent is assumed complete ("hasFinishedStatus" pre-set). |
| contentLayout | optional enum | `content_layout` (name) | Unset → global `TorrentContentLayout` (default Original). |
| useAutoTMM | optional bool | `use_auto_tmm` | Unset → `true` iff savePath **and** downloadPath both empty **and** "Default Torrent Management Mode" is Automatic (`DisableAutoTMMByDefault` = false; default is Manual). |
| uploadLimit / downloadLimit | int B/s, -1 | `upload_limit` / `download_limit` | Per-torrent rate limit; ≤0 = unlimited. |
| ratioLimit | real, -2 | `ratio_limit` | -2 = use category/global, -1 = no limit. |
| seedingTimeLimit | int minutes, -2 | `seeding_time_limit` | same sentinel semantics. |
| inactiveSeedingTimeLimit | int minutes, -2 | `inactive_seeding_time_limit` | same. |
| shareLimitAction | enum, Default | `share_limit_action` (name) | Default = inherit. |
| sslParameters | cert/key/dh | `ssl_certificate`, `ssl_private_key`, `ssl_dh_params` (PEM strings) | For SSL torrents (lt `torrent_need_cert_alert`); missing → `Torrent is missing SSL parameters. Torrent: "%1". Message: "%2"`. |

Serialization (`serializeAddTorrentParams`) always writes `category, tags, save_path, download_path, operating_mode, skip_checking, upload_limit, download_limit, seeding_time_limit, inactive_seeding_time_limit, share_limit_action, ratio_limit, ssl_*`; optional keys only when set.

#### 4.3.3 Add algorithm (`SessionImpl::addTorrent_impl`)
Adding is refused until the session has finished restoring (`isRestored()`). Steps:
1. **Duplicate check** (`findTorrent`, incl. hybrid alt-ID). If duplicate:
   - If the new source has metadata and the existing torrent has none → set metadata on the existing torrent.
   - If "Merge trackers to existing torrent" (`BitTorrent/MergeTrackersEnabled`, default false) is off → message `Merging of trackers is disabled`.
   - Else if either is private → `Trackers cannot be merged because it is a private torrent`.
   - Else add new trackers + web seeds → `Trackers are merged from new source`.
   - Log `Detected an attempt to add a duplicate torrent. Existing torrent: "%1". Torrent infohash: %2. Result: %3` and emit `addTorrentFailed(DuplicateTorrent, message)`. (GUI: with "Ask to merge trackers for manually added torrent" (`GUI/ConfirmActions/MergeTrackers`, default true) a confirmation box "Torrent is already present" is shown instead.)
2. Cancel any in-progress hidden metadata download for this hash (and alt-ID).
3. Build `LoadTorrentParams` (`initLoadTorrentParams`): resolve category (auto-create), save/download paths (4.6.3), tags, defaults for stopped/stopCondition/queueTop/contentLayout.
4. With metadata:
   - If stopCondition = MetadataReceived → torrent is added **stopped** and stopCondition reset to None.
   - File paths = given `filePaths` or metadata paths adjusted for content layout (4.13.1).
   - If name not explicit and the (possibly renamed) root folder / single file name differs from the metadata name → that becomes the torrent name.
   - If no explicit priorities and "Excluded file names" enabled → apply filter (4.8.1).
   - All lt file priorities default to qBittorrent Normal (1) instead of lt default 4; pad files included.
   - If not skipChecking → `findIncompleteFiles` (4.8.3).
5. Without metadata: name = magnet `dn` if present.
6. Additional trackers: if "Automatically add these trackers to new downloads" enabled and torrent is not private → append `AdditionalTrackers` entries with tier = (max existing tier + 1) + entry tier. Same afterwards for trackers-from-URL list (4.11.2). For magnets the private flag is unknown so trackers are always added.
7. `upload_limit`/`download_limit`; `storage_mode` = allocate if "Pre-allocate disk space for all files" (`Preallocation`, default false) else sparse; sequential flag; `seed_mode` iff skipChecking.
8. Flags: `paused` iff (stopped OR AutoManaged); `auto_managed` iff (!stopped AND AutoManaged). `duplicate_is_error`. `added_time = now`. `max_connections` = per-torrent connection limit, `max_uploads` = per-torrent upload slots.
9. `async_add_torrent`; on `add_torrent_alert`:
   - error → `Failed to add torrent. Reason: "%1"` (WARNING), `addTorrentFailed` (DuplicateTorrent if lt duplicate error).
   - success → if addToQueueTop: `queue_position_top()`; create TorrentImpl; request resume data with `save_info_dict`; log `Added new torrent. Torrent: "%1"`; emit `torrentAdded`; if it has metadata and "Copy .torrent files to:" (`TorrentExportDirectory`) set → export (4.8.5).
10. After lt checks/pauses (4.4.3) the torrent is resumed per its mode.

#### 4.3.4 Stop condition semantics
- Set per torrent at add time (or later via `setStopCondition`, only accepted if torrent is running, and: MetadataReceived only while metadata is missing; FilesChecked only while metadata is missing or the torrent is currently checking).
- **MetadataReceived**: when metadata arrives (4.3.5) the torrent is stopped (`handleTorrentStopped`) and the condition cleared. If metadata already present at add → added stopped.
- **FilesChecked**: when lt finishes checking (`torrent_checked_alert`) the torrent is stopped. Also set implicitly when "Force recheck" is applied to a stopped torrent (it is started for checking, then stops again).
- `stop()` always clears the stop condition. Persisted as `qBt-stopCondition` / column `stop_condition`. Legacy lt `stop_when_ready` flag in old resume data maps to FilesChecked.

#### 4.3.5 Magnet metadata retrieval
- Normal path (torrent added without metadata): state `DownloadingMetadata` / `ForcedDownloadingMetadata` / `StoppedDownloading` / `QueuedDownloading`. On `metadata_received_alert` → `MaintenanceJob::HandleMetadata` → request resume data → in `handleSaveResumeData` the info dict is taken, content layout applied, renamed files honoured, `findIncompleteFiles` run → `endReceivedMetadataHandling`: file priorities (excluded-name filter applied), stop condition applied, torrent **reloaded** in lt (remove + re-add with metadata, same queue position, flags reflecting stopped/forced/automanaged), first/last piece priority applied, `torrentMetadataReceived` emitted, `.torrent` exported to TorrentExportDirectory if set.
- Preview path (`Session::downloadMetadata`, used by the Add dialog for magnets): adds a hidden lt torrent (not in torrent list) with save path `<system temp>/.qBittorrent/<torrentID>`, flags: not paused, not auto-managed, **upload_mode** (never writes files), additional trackers appended; on metadata → removed with `delete_files`, `metadataDownloaded(TorrentInfo)` emitted. `cancelDownloadMetadata` removes it. Queue up/bottom operations push these hidden torrents to the queue bottom. The temp dir is deleted recursively on exit.
- Log on failure: `Failed to download torrent metadata. Reason: "%1"`.

### 4.4 Torrent lifecycle operations

#### 4.4.1 State computation (`TorrentImpl::updateState`, evaluated on every lt state update)
Priority order:
1. lt `checking_resume_data` → **CheckingResumeData**
2. storage move in progress → **Moving**
3. `m_hasMissingFiles` (set by `fastresume_rejected_alert`) → **MissingFiles**
4. lt error code set OR lt `upload_mode` flag (file write error) → **Error**
5. no metadata → Stopped → **StoppedDownloading**; queued → **QueuedDownloading**; else Forced → **ForcedDownloadingMetadata**, else **DownloadingMetadata**
6. lt `checking_files` and not stopped → **CheckingUploading** if "has finished status" else **CheckingDownloading**
7. finished (lt `finished` or `seeding`) → Stopped → **StoppedUploading**; queued → **QueuedUploading**; Forced → **ForcedUploading**; upload payload rate > 0 → **Uploading**; else **StalledUploading**
8. otherwise → Stopped → **StoppedDownloading**; queued → **QueuedDownloading**; Forced → **ForcedDownloading**; download payload rate > 0 → **Downloading**; else **StalledDownloading**

Helpers: `isQueued` = queueing enabled AND not stopped AND lt flags `auto_managed` AND `paused`. `isForced` = !stopped AND mode Forced. `isFinished` = lt state finished/seeding. `isActive` = up or down payload rate > 0. `isErrored` = MissingFiles or Error. `isDownloading` = {Downloading, DownloadingMetadata, ForcedDownloadingMetadata, StalledDownloading, CheckingDownloading, StoppedDownloading, QueuedDownloading, ForcedDownloading}. `isUploading` = {Uploading, StalledUploading, CheckingUploading, QueuedUploading, ForcedUploading}. `isCompleted` = isUploading set + StoppedUploading. Status filter mapping uses these (`base/torrentfilter.cpp`): Downloading→isDownloading, Seeding→isUploading, Completed→isCompleted, Running→!stopped, Stopped→stopped, Active→isActive, Inactive→!isActive, Stalled→StalledUploading|StalledDownloading, Checking→CheckingUploading|CheckingDownloading|CheckingResumeData, Moving, Errored.

Error text (`error()`): lt error message, or for upload_mode: `Couldn't write to file. Reason: "%1". Torrent is now in "upload only" mode.` Logged on add if present: `Torrent errored. Torrent: "%1". Error: "%2"`. `upload_mode` is never persisted (stripped before saving resume data) so the torrent operates normally after restart.

File errors (`file_error_alert`): remembered as last file error; per torrent at most once per 1 s burst: log `File error alert. Torrent: "%1". File: "%2". Reason: "%3"` (WARNING) and emit `fullDiskError` (GUI notification title "I/O Error", body `An I/O error occurred for torrent '%1'.\n Reason: %2`).

#### 4.4.2 Start / Stop / Force start (`TorrentImpl::start/stop`)
- **Stop**: clears stop condition, `m_isStopped=true`, saves resume data, logs `Torrent stopped. Torrent: "%1"`, resets tracker statuses; unless a maintenance job is running: unset `auto_managed`, `pause()`, reset speed sampler. Persisted as lt flags `paused=1, auto_managed=0`.
- **Start** (mode AutoManaged = "Start", Forced = "Force Start"): if errored → `clear_error()` + unset `upload_mode`. Sets operating mode. If MissingFiles → clear flag, unstop, **reload** (re-add from resume data → re-check). Else if was stopped → unstop, log `Torrent resumed. Torrent: "%1"`. Unless maintenance job: set `auto_managed` = (mode==AutoManaged); for Forced also `resume()`.
- **Forced** torrents ignore queue limits (not auto-managed) and are exempt from share-limit processing.
- **Session pause/resume** (`SessionImpl::pause/resume`, GUI actions "Pause session"/"Resume session"): pauses the whole lt session without changing per-torrent state; tracker statuses reset. "Start BitTorrent session in paused state" (`BitTorrent/Session/StartPaused`, default false) starts paused.

#### 4.4.3 Internal pause after checking (`nativetorrentextension.cpp`)
Every lt torrent gets a plugin: whenever its state **leaves** `downloading_metadata` or `checking_files`, the plugin unsets `auto_managed` and pauses it. qBittorrent then (in `handleTorrentChecked`) resumes it according to its operating mode unless it is stopped (or the FilesChecked stop condition fired). On `fastresume_rejected_alert` the session plugin also unsets auto_managed + pauses (→ MissingFiles).

#### 4.4.4 Remove (`SessionImpl::removeTorrent`)
- Options: KeepContent ("Remove torrent") or RemoveContent ("Also remove the content files"/"Remove torrent and its files").
- KeepContent: if a storage-move job for the torrent is pending, the torrent is pushed to queue bottom, auto-managed off, paused, and actually removed from lt after the move job finishes; otherwise `remove_torrent(delete_partfile)`.
- RemoveContent: queued (non-active) move jobs for it are dropped; `remove_torrent(delete_partfile)`; after lt confirms (`torrent_deleted_alert` / `torrent_delete_failed_alert`), the content is removed by `TorrentContentRemover` on the IO thread: each file `<actualStorageLocation>/<actual file path>` is deleted (option Delete) or moved to the OS trash (MoveToTrash; global `BitTorrent/Session/TorrentContentRemoveOption`, default Delete); then for each top-level folder `Utils::Fs::smartRemoveEmptyFolderTree` deletes empty folders bottom-up, also deleting junk files `Thumbs.db`, `desktop.ini`, `.directory`, `.DS_Store` (case-insensitive) and files ending with `~`.
- Resume record deleted immediately. Logs: `Torrent removed. Torrent: "%1"`, `Torrent content removed. Torrent: "%1"`, `Failed to remove torrent content. Torrent: "%1". Error: "%2"` (WARNING), `Failed to remove partfile. Torrent: "%1". Reason: "%2".`
- GUI confirmation ("Delete torrent confirmation") and default checkbox state (`Preferences/General/DeleteTorrentsFilesAsDefault`) are UI concerns.

#### 4.4.5 Move storage (`addMoveTorrentStorageJob`)
- Single global FIFO queue; only the head job runs (`move_storage`). Log `Enqueued torrent move. Torrent: "%1". Source: "%2". Destination: "%3"`, `Start moving torrent. Torrent: "%1". Destination: "%2"`, `Moved torrent successfully. Torrent: "%1". Destination: "%2"`, `Failed to move torrent. Torrent: "%1". Source: "%2". Destination: "%3". Reason: "%4"` (WARNING).
- A new job for a torrent cancels its pending (non-active) job (`Torrent move canceled...`). Rejected if the active job already targets the same path, or if no active job and destination == current location (`Failed to enqueue torrent move ... Reason: both paths point to the same location`).
- Mode: user-initiated save-path / download-path changes use **KeepExistingFiles** (lt `dont_replace`: existing files at destination are kept and used); automatic relocation (`AdjustCurrentLocation`, e.g. ATM/category changes, completion move from download path to save path) uses **Overwrite**.
- Torrent shows state Moving while any job for it is queued/running. After completion: stored save/download path updated, if the torrent had MissingFiles it is reloaded (allows "Set location" to fix missing files), deferred "finished" handling is executed.
- Without metadata a path change only updates the stored path (no IO).
- On exit, queued moves except the running one are discarded; shutdown waits for the running move.

#### 4.4.6 Set save path / download path (manual mode only) (`TorrentImpl::setSavePath/setDownloadPath`)
- Relative input is resolved against the default save path (or the category path if "Use Category paths in Manual Mode", `UseCategoryPathsInManualMode`, default false).
- setSavePath: if torrent is complete (finished or has finished status) or has no download path → move content now (ChangeSavePath); else just record it (content moves there on completion).
- setDownloadPath: if incomplete → move content to new download path (or to save path if cleared); if complete → just record.

#### 4.4.7 Recheck (`forceRecheck`)
Only with metadata. Calls lt `force_recheck`, immediately marks state as checking, clears progress/pieces/completed-file cache. If MissingFiles: clears it and restores automanaged/forced running state. If stopped: starts it and sets stop condition FilesChecked (so it stops again after checking). GUI asks confirmation (`Preferences/Advanced/confirmTorrentRecheck`, default true).

"Recheck torrents on completion" (`Preferences/Advanced/RecheckOnCompletion`, default false): when a torrent finishes and it has been "unchecked" (it downloaded data since its last check), it is force-rechecked instead of being marked finished.

#### 4.4.8 Reannounce
- Per torrent `forceReannounce(index=-1)` → lt `force_reannounce(0, index)`; DHT: `force_dht_announce`.
- Session-wide `reannounceToAllTrackers` → `force_reannounce(0, -1, ignore_min_interval)` for every torrent; triggered: (a) external IP change (`external_ip_alert`, separate tracking for IPv4/IPv6; not on the first detection) or listen-interface reconfiguration, only if "Reannounce to all trackers when IP or port changed" (`ReannounceWhenAddressChanged`, default false); (b) system wake-up detection: a 30 s timer; if >100 s elapsed between ticks → log `System wake-up event detected. Re-announcing to all the trackers...` and reannounce (always).

#### 4.4.9 Rename torrent / files
- Torrent display name: `setName` (persisted `qBt-name`). Effective name = explicit name, else metadata name, else lt status name (from `dn`), else ID.
- File rename (`renameFile(index, path)`): target actual path computed with `.!qB` / `.unwanted` decoration (4.8); lt `rename_file`. Folder rename = rename every file under the folder. Validation (`abstractfilestorage.cpp`): `The old path is invalid: '%1'.`, `The new path is invalid: '%1'.`, `Absolute path isn't allowed: '%1'.`, `The file already exists: '%1'.`, `No such file: '%1'.`, `The folder already exists: '%1'.` (case-sensitive comparison), `No such folder: '%1'.`. Path validity: non-empty, and on macOS must not contain NUL or `:`; on Windows not `\0-\37 : ? " * < > |`.
- After rename, empty leftover parent folders between old path and common ancestor are removed. Renames that only add/remove `.!qB`/`.unwanted` are recognised (not user renames). Failure: `File rename failed. Torrent: "%1", file: "%2", reason: "%3"`.

#### 4.4.10 File priorities (`prioritizeFiles`)
- Values Ignored/Normal/High/Maximum (0/1/6/7). If a previously Ignored, not-yet-complete file is re-enabled, the "finished" status is reset (torrent will go back to downloading and fire "finished" again).
- Re-applies first/last piece priority, then re-evaluates `.unwanted`/`.!qB` decorations (`manageActualFilePaths`).

#### 4.4.11 Category / tags / ATM on an existing torrent
- `setCategory`: category must exist (or empty). If "When Torrent Category changed" = "Switch torrent to Manual Mode" (`DisableAutoTMMTriggers/CategoryChanged`, default false = "Relocate torrent") → torrent switched to manual first. ATM torrents are then relocated to the new category paths (AdjustCurrentLocation).
- `belongsToCategory(c)`: equal, or category starts with `c + "/"` (subcategory membership, used for filtering and removal).
- `setAutoTMMEnabled(false)`: freezes current category-derived save/download paths as manual paths. `true`: relocate to category paths.
- Tags: `addTag` creates global tag if missing; `removeTag`; global tag removal removes from all torrents.

#### 4.4.12 Per-torrent switches
- `setSequentialDownload` (lt flag), `setFirstLastPiecePriority` (log `Download first and last piece first: On|Off, torrent: '%1'`), `setSuperSeeding` (lt `super_seeding`), `setDHTDisabled` / `setPEXDisabled` / `setLSDDisabled` (lt `disable_dht/pex/lsd` flags, persisted in lt resume data), `setUploadLimit`/`setDownloadLimit` (B/s; <0 or INT_MAX normalised to 0 = unlimited).
- `setRatioLimit` (<-2 → -1), `setSeedingTimeLimit`, `setInactiveSeedingTimeLimit` (<-2 → -1), `setShareLimitAction`.
- Trackers: `addTrackers` (dedupe by URL, sorted by tier), `removeTrackers`, `replaceTrackers` (dedupe; for private torrents also **clears peer list**); URL seeds add/remove (async). Session logs `Added URL seed to torrent...`, `Removed URL seed from torrent. Torrent: "%1". URL: "%2"`, tracker added/removed logs.
- `createMagnetURI()`: `magnet:?xt=urn:btih:<v1>` [`&xt=urn:btmh:1220<v2>`] `&dn=<pct-enc name>` (only if name != ID) `&xl=<total size>` (if metadata) `&tr=<pct-enc>`… `&ws=<pct-enc>`….
- `exportToFile` / `exportToBuffer`: writes a complete .torrent (lt `write_torrent_file` with current trackers/web seeds); error `Missing metadata` without metadata.

#### 4.4.13 Derived values
- **Progress**: checking → lt progress; wanted bytes 0 → 0 (or 1 if complete); else `total_wanted_done/total_wanted` clamped.
- **Ratio** (`realRatio`): upload = all-time upload; download = all-time download, but if all-time download < 1% of `total_done` (seeder that lost stats) use `total_done`. download 0 → ratio 0 if upload 0 else ∞.
- **Popularity**: ratio / active months (`active_duration` in fractional months); 0 if no active time.
- **ETA** (`eta()`): stopped → MAX_ETA. Downloading: `(wanted − completed) / avg download payload rate` (MAX_ETA if 0). Finished: min over the applicable share limits — ratio ETA `((realDL or wantedSize) × ratioLimit − totalUpload) / avg upload rate`, seeding-time ETA `limit×60 − finishedTime` (≥0), inactive ETA `limit×60 − timeSinceActivity` (≥0); MAX_ETA if no limits. Average rate = `SpeedMonitor` sliding average of samples (one per state update).
- **Seeding time** = lt `finished_duration`; **active time** = lt `active_duration`; **time since activity** = if exactly one of (time since last upload, time since last download) is unknown (-1) → the known one, else the min of both.
- **Next announce**, **current tracker** = lt `current_tracker` or first tracker URL.
- Files progress uses per-piece accounting; availability = fraction of a file's pieces with availability > 0 (−1 per file if lt returns empty availability, i.e. seeding).

### 4.5 Queueing system

Settings (`BitTorrent/Session/…`): `QueueingSystemEnabled` (default **false** for fresh profiles; profiles created by older versions keep legacy default true via `handleChangedDefaults`), `MaxActiveDownloads` (3), `MaxActiveUploads` (3), `MaxActiveTorrents` (5) — values < -1 clamped to -1, -1 = unlimited; `IgnoreSlowTorrentsForQueueing` (false, "Do not count slow torrents in these limits"), `SlowTorrentsDownloadRate` (2 KiB/s, "Download rate threshold"), `SlowTorrentsUploadRate` (2 KiB/s, "Upload rate threshold"), `SlowTorrentsInactivityTimer` (60 s, "Torrent inactivity timer"), `MaxActiveCheckingTorrents` (1).

Mapping to lt: enabled → `active_downloads`, `active_limit` (max active torrents), `active_seeds` (max uploads), `dont_count_slow_torrents`, `inactive_down_rate`/`inactive_up_rate` (KiB→B), `auto_manage_startup` = inactivity timer. Disabled → all three `active_*` = -1 (no queuing). `active_checking` = max checking torrents (regardless of queueing).

Semantics (lt auto-manager): auto-managed (non-forced, non-stopped) torrents beyond limits are paused internally → shown as Queued; downloading torrents compete for `active_downloads` in queue order; seeds for `active_seeds`; total ≤ `active_limit`. With "do not count slow", torrents below both rate thresholds for longer than the inactivity timer don't count against limits. Forced torrents are not auto-managed and never queued.

Queue positions: only downloading (unfinished) torrents have a queue position (0-based in lt; UI shows 1-based); finished/seeding torrents and when queueing disabled → -1 (UI "*"). Operations (`increase/decrease/top/bottomTorrentsQueuePos`): selected torrents with position ≥ 0 sorted by position; up and bottom iterate from highest position, down and top iterate from lowest, so relative order is preserved; hidden metadata-download torrents are kept at the bottom. Queue changes mark the queue dirty; it is persisted after the next state update (4.16.3). Disabling queueing deletes the persisted queue; enabling marks it dirty.

### 4.6 Save paths, categories and Automatic Torrent Management (ATM)

#### 4.6.1 Global paths
- Default save path `BitTorrent/Session/DefaultSavePath`, default = OS Downloads folder (`QStandardPaths::DownloadLocation`); a relative value is resolved against the Downloads folder.
- Download (incomplete) path `BitTorrent/Session/TempPath`, default `<save path>/temp`; relative value resolved against `<save path>/temp/`. Enabled by `TempPathEnabled` (default false; label "Keep incomplete torrents in:").
- Changing the default save path re-points the free-disk-space checker (4.18).

#### 4.6.2 Categories (`categories.json` in the Config dir)
- Names: hierarchical with `/` separators. Valid iff empty or matches `^([^\\\/]|[^\\\/]([^\\\/]|\/(?=[^\/]))*[^\\\/])$` (no backslash, no leading/trailing `/`, no `//`). Adding `a/b/c` auto-creates `a` and `a/b` with default options. Removing a category removes all its subcategories and moves member torrents to the **parent** category (or uncategorized).
- `CategoryOptions` JSON per category:
  ```json
  {
    "Movies":    {"save_path": "", "download_path": null, "ratio_limit": -2, "seeding_time_limit": -2,
                  "inactive_seeding_time_limit": -2, "share_limit_action": "Default"},
    "Movies/HD": {"save_path": "/abs/or/relative", "download_path": false, ...}
  }
  ```
  - `save_path`: "" = implicit.
  - `download_path`: `null` = inherit (parent category, ultimately the global "Keep incomplete torrents in" toggle); `false` = explicitly disabled; string = enabled ("" = implicit path).
  - Share limit fields: -2/"Default" = inherit from parent category, ultimately global.
  - Written pretty-printed by `QJsonDocument::toJson`; max read size 1 MiB. Errors: `Failed to load Categories. %1`, `Failed to parse Categories configuration. File: "%1". Error: "%2"`, `Failed to save Categories configuration. File: "%1". Error: "%2"`. Legacy migration from `BitTorrent/Session/Categories` (map name→save path).
- **Category save path** (`categorySavePath`): uncategorized → default save path. Explicit absolute → as is. Explicit relative → `<default save path>/<path>`. Implicit → `<parent category save path>/<subcategory name made valid>` (invalid chars `: ? " * < > |` replaced via `toValidPath`).
- **Category download path** (`categoryDownloadPath`): resolve option (own → parent chain → global enabled flag). Disabled → none. Uncategorized → global download path. Path = explicit or implicit (subcategory name); absolute → as is; relative → `<parent's download path computed as if enabled>/<path>`, or `<global download path>/<path>` if parent yields none.
- Category share limits: `categoryRatioLimit` etc. walk own → parent → … → global.
- Recovery at startup: if a torrent references a missing category it is recreated (`Detected inconsistent data: category is missing from the configuration file. Category will be recovered but its settings will be reset to default. Torrent: "%1". Category: "%2"`); invalid names cleared (`Detected inconsistent data: invalid category...`); if a recovered category's path doesn't match an ATM torrent's actual location, the torrent switches to manual (`Detected mismatch between the save paths of the recovered category and the current save path of the torrent. Torrent is now switched to Manual mode...`). Missing tags are likewise recovered (`Detected inconsistent data: tag is missing from the configuration file. Tag will be recovered...`).

#### 4.6.3 Path resolution when adding
- `suggestedSavePath(category, useAutoTMM)` = category save path if (ATM resolved true, defaulting to !DisableAutoTMMByDefault) OR "Use Category paths in Manual Mode"; else default save path.
- `suggestedDownloadPath` = category download path under the same condition (if non-empty) else global download path.
- ATM torrents store **no** paths (resume `qBt-savePath` absent/empty ⇒ ATM on reload); effective save/download path is always recomputed from the category.
- Manual torrents: savePath = absolute given path, or `suggestedSavePath / given`; downloadPath only if useDownloadPath (see table 4.3.2).

#### 4.6.4 Relocation triggers ("relocate vs. switch to manual")
| Event | Setting (default) | Behaviour |
|---|---|---|
| Torrent's category changed | `DisableAutoTMMTriggers/CategoryChanged` (false → "Relocate torrent") | true → torrent switched to Manual before the change (stays in place). |
| Default save path or default download path changed | `DisableAutoTMMTriggers/DefaultSavePathChanged` (true → "Switch affected torrents to Manual Mode") | Affected = ATM torrents that are uncategorized or whose category save path (resp. effective download path) is relative. If true they are switched to manual before the change; otherwise all ATM torrents are re-evaluated and moved. |
| A category's save/download path changed | `DisableAutoTMMTriggers/CategorySavePathChanged` (true → "Switch affected torrents to Manual Mode") | Torrents of that exact category switched to manual if true; else relocated. |
| Global "Keep incomplete" toggle changed | — | All ATM torrents re-evaluated. |

Re-evaluation (`adjustStorageLocation`): target = save path if (finished OR has finished status OR no download path) else download path; move (AdjustCurrentLocation/Overwrite) if target differs from actual location.

On completion an incomplete torrent in its download path is moved to its save path (via `handleTorrentFinished` → `adjustStorageLocation`), and the "finished" event is delivered only after the move and pending renames complete.

### 4.7 Share limits (`SessionImpl::processTorrentShareLimits`)

- Global settings: `GlobalMaxRatio` (default -1; negative stored as -1, "When ratio reaches"), `GlobalMaxSeedingMinutes` (default -1, "When total seeding time reaches"), `GlobalMaxInactiveSeedingMinutes` (default -1, "When inactive seeding time reaches"), `ShareLimitAction` (default Stop; "then": "Stop torrent" / "Remove torrent" / "Remove torrent and its files" / "Enable super seeding for torrent"; Default is not allowed globally).
- Effective limits per torrent: own value unless -2 → category chain → global.
- Evaluation runs (a) every **10 s** on a timer that is active only while a global limit is set or any torrent has an explicit (≥0) own limit (note: category-only limits don't start the timer — they are still evaluated on the finish event; a reimplementation should include category limits in the timer condition), and (b) immediately when a torrent finishes.
- Skipped if torrent is not finished or is Forced.
- Checks in order, first hit wins: ratio ≥ limit (`Torrent reached the share ratio limit.`), seeding minutes (`finishedTime/60`) ≥ limit (`Torrent reached the seeding time limit.`), inactive minutes (`timeSinceActivity/60`) ≥ limit (`Torrent reached the inactive seeding time limit.`).
- Actions (log `"<description> <action text> Torrent: "<name>"."`):
  - Remove → remove torrent keeping files (`Removing torrent.`).
  - RemoveWithContent → remove with content (`Removing torrent and deleting its content.`).
  - Stop → if not already stopped: stop (`Torrent stopped.`).
  - EnableSuperSeeding → if running and not already super-seeding: enable (`Super seeding enabled.`).
- Remove actions also apply to stopped (finished) torrents.
- Category option dialogs and per-torrent dialogs expose the same three limits + action.

### 4.8 Files on disk

#### 4.8.1 Excluded file names (`ExcludedFileNamesEnabled` default false, `ExcludedFileNames` list)
- Each line is a wildcard pattern (`QRegularExpression::wildcardToRegularExpression`: `*`, `?`, `[...]`, fully anchored), **case-insensitive**.
- A file is excluded if the pattern matches its file name **or the name of any ancestor folder** within the torrent.
- Excluded files get priority Ignored (already-Ignored files untouched). Applied at add time only when no explicit priorities were given (e.g. the Add dialog sets its own), and when metadata of a magnet arrives. Also applied by the RSS/watched-folder/API paths since they don't pass priorities.

#### 4.8.2 `.!qB` extension (`AddExtensionToIncompleteFiles`, default false, "Append .!qB extension to incomplete files")
- Actual on-disk name = user path + `.!qB` while the file is incomplete and size > 0. On `file_completed_alert` the file is renamed to its final name. Toggling the option renames all files of all torrents accordingly.
- User-visible paths never include `.!qB` (stripped by `makeUserPath`).
- Adding with existing data: `FileSearcher` (4.8.3) recognises `name.!qB` files.
- Custom disk-IO wrapper (`customstorage.cpp`): before checking and after a move, for each wanted non-pad file whose lt path ends with `.!qB`, if the **complete** name exists, the `.!qB` file is deleted and the complete file renamed to the `.!qB` name so checking verifies it (then completion renames it back).

#### 4.8.3 Finding existing data (`FileSearcher::search`)
On add (unless skipChecking) and when magnet metadata arrives: first look in the save path — for each file, if `path` exists use it, else if `path.!qB` exists use that, else (if appending is enabled and no download path) expect `path.!qB`. If **no** file was found in the save path and a download path is set, repeat in the download path (appending `.!qB` names when enabled). The directory where files were found becomes the initial lt save path.

#### 4.8.4 `.unwanted` folder (`UseUnwantedFolder`, default false, "Keep unselected files in ".unwanted" folder")
- Files with priority Ignored are stored at `<parent dir>/.unwanted/<file name>` (lt rename). Re-enabling moves them back. Toggling the option re-evaluates all torrents. Empty `.unwanted` folders are removed after files leave them; on Windows the folder is marked hidden. User-visible paths never include `.unwanted`.
- Combines with `.!qB` (extension applied first, then folder).

#### 4.8.5 Copy .torrent files
- "Copy .torrent files to:" (`TorrentExportDirectory`, default empty) — on add (if metadata present) or when magnet metadata arrives.
- "Copy .torrent files for finished downloads to:" (`FinishedTorrentExportDirectory`) — on each "finished" event.
- File name: `<torrent name made valid>.torrent` (chars `\ / : ? " * < > |` removed via `toValidFileName`, trimmed); if it exists → `<name> (1).torrent`, `(2)`… Directory created if missing. Error: `Failed to export torrent. Torrent: "%1". Destination: "%2". Reason: "%3"`.

#### 4.8.6 Pre-allocation, Mark-of-the-Web
- Pre-allocate → lt `storage_mode_allocate`, else sparse.
- macOS/Windows: "Mark-of-the-Web" (`Preferences/Advanced/markOfTheWeb`, default true): when a file completes while the torrent is in lt `downloading` state, set the quarantine attribute (`kLSQuarantineTypeOtherDownload`) on it.

### 4.9 Watched folders & .torrent auto-deletion

#### 4.9.1 Configuration (`watched_folders.json` in Config dir, `base/torrentfileswatcher.cpp`)
```json
{
  "/absolute/watched/path": {
    "add_torrent_params": { ...AddTorrentParams JSON (4.3.2)... },
    "recursive": false
  }
}
```
Path must be non-empty and absolute (`Watched folder Path cannot be empty.`, `Watched folder Path cannot be relative.`). Max file size 10 MiB. Logs: `Watching folder: "%1"`, `Failed to load Watched Folders configuration. %1`, `Failed to parse Watched Folders configuration from %1. Error: "%2"`, `Couldn't store Watched Folders configuration to %1. Error: %2`. Legacy `Preferences/Downloads/ScanDirsV2` (path → 0 "save to watched folder" | 1 "default" | string custom path) is migrated and deleted.

#### 4.9.2 Processing
- Watch strategy: local, non-recursive folders use OS file-system notifications (`QFileSystemWatcher`), processed 2 s after a change notification and once at registration. Folders on network file systems (macOS: `cifs`, `nfs`, `smbfs`; Linux: SMB/NFS/CEPH/… magic numbers) **or** with recursive mode are polled every **10 s**.
- Matched files: `*.torrent` and `*.magnet` (non-recursive per directory; recursive mode descends into subdirectories except those that are themselves watched folders).
- Subfolder mapping (recursive mode): for a file in subdir `a/b` relative to the watched folder: if the effective ATM is on → category = `a/b` (or `<configured category>/a/b`); else save path = `<configured save path>/a/b`.
- `.magnet` files: text, ≤100 MiB, each line trimmed and parsed as magnet/hash; invalid lines logged `Invalid Magnet URI. URI: %1. Reason: %2`; file deleted after reading.
- `.torrent` files: loaded; on success added and **file deleted** (always, independent of the auto-delete setting). On load failure (e.g. partially written) the file is retried every 10 s up to 5 retries, then renamed to `<file>.qbt_rejected` (`Rejecting failed torrent file: %1`).
- Torrents are added directly to the session (no Add dialog) with the folder's AddTorrentParams.

#### 4.9.3 Auto-delete of added .torrent files (`Core/AutoDeleteAddedTorrentFile`, default Never)
`TorrentFileGuard` guards a local .torrent source:
- **Never** (UI unchecked "Delete .torrent files afterwards"): never deleted.
- **IfAdded** (checked): deleted only if the torrent was actually added to the session.
- **Always** (also "Also delete .torrent files whose addition was cancelled"): deleted even if the add was cancelled/failed (e.g. Add dialog cancelled); but if the session reports add failure for a tracked source the guard is disarmed.

### 4.10 Completion actions

#### 4.10.1 "Finished" event
Emitted once per transition to finished (`torrentFinished`), after storage moves/renames complete; not emitted for torrents added with skipChecking. Logged `Torrent download finished. Torrent: "%1"`. Then: export to finished-.torrent dir, share-limit processing, GUI notification `Download completed` / `'%1' has finished downloading.` After processing a batch, if no torrent remains that is unfinished-and-running-and-not-errored → `allTorrentsFinished` (4.10.3).

#### 4.10.2 Run external program (`app/application.cpp::runExternalProgram`)
- Two hooks: on torrent **added** (`AutoRun/OnTorrentAdded/Enabled` false, `…/Program`) and on torrent **finished** (`AutoRun/enabled` false, `AutoRun/program`). Template trimmed before use.
- Placeholders (case-sensitive; scanned right-to-left so `%%N` keeps a literal `%` + value semantics: after a replacement the scanner skips one char):
  | Token | Value |
  |---|---|
  | `%N` | Torrent name |
  | `%L` | Category |
  | `%G` | Tags (separated by comma) |
  | `%F` | Content path (same as root path for multifile torrent; for single-file torrents the file path) |
  | `%R` | Root path (first torrent subdirectory path; empty if no root folder) |
  | `%D` | Save path |
  | `%C` | Number of files |
  | `%Z` | Torrent size (bytes) (total size) |
  | `%T` | Current tracker |
  | `%I` | Info hash v1 (or `-` if unavailable) |
  | `%J` | Info hash v2 (or `-` if unavailable) |
  | `%K` | Torrent ID (either sha-1 info hash for v1 torrent or truncated sha-256 info hash for v2/hybrid torrent) |
  | `%M` | Comment |
  UI tip: `Tip: Encapsulate parameter with quotation marks to avoid text being cut off at whitespace (e.g., "%N")`.
- No shell is used (command-injection safety). macOS/Linux: the template is split into arguments with shell-like quoting rules (`Utils::String::splitCommand`), surrounding `"` stripped per argument, placeholders substituted **per argument**, first argument = program, started detached with all inherited file descriptors closed. Windows: placeholders substituted in the whole string, split with `CommandLineToArgvW`; "Show console window" (`AutoRun/ConsoleEnabled`, default false) chooses `CREATE_NEW_CONSOLE` vs `CREATE_NO_WINDOW`.
- Logs: ``Running external program. Torrent: "%1". Command: `%2` `` / ``Failed to run external program. Torrent: "%1". Command: `%2` ``.

#### 4.10.3 Email notification on completion
- Enabled by `Preferences/MailNotification/enabled` (false). Fields: sender `…/sender` (default `qBittorrent_notification@example.com`), recipient `…/email`, server `…/smtp_server` (default `smtp.changeme.com`, format `host[:port]`), `…/req_ssl` (false), `…/req_auth` (false), `…/username`, `…/password`.
- Log `Torrent: %1, sending mail notification`.
- Message: headers `Date`, `From: qBittorrent <sender>`, `Subject: Torrent "<name>" has finished downloading`, `To`, `MIME-Version: 1.0`, `Content-Type: text/plain; charset=UTF-8`, `Content-Transfer-Encoding: base64`; body (LF→CRLF, base64 in 78-char lines):
  ```
  Torrent name: <name>
  Torrent size: <friendly wanted size>
  Save path: <save path>

  The torrent was downloaded in <friendly duration of active time>.


  Thank you for using qBittorrent.
  ```
- Test mail (button "Send test email"): subject `Test email`, body `This is a test email.\nThank you for using qBittorrent.\n`.
- SMTP client (`net/smtp.cpp`): SSL option → implicit TLS (`connectToHostEncrypted`, default port 465) else plain TCP (default port 25). `EHLO <local hostname or "localhost">`, falls back to `HELO`. If SSL option set and server advertises STARTTLS → `STARTTLS`. Auth only if username and password set and server advertises AUTH: prefers CRAM-MD5, then PLAIN, then LOGIN. Errors logged (e.g. `Both EHLO and HELO failed, msg: %1`).

#### 4.10.4 Recursive download (GUI)
If "Enable recursive download dialog" (`Preferences/Advanced/DisableRecursiveDownload` = false, default enabled) and a finished torrent contains `.torrent` files: ask `The torrent '%1' contains .torrent files, do you want to proceed with their downloads?` (Yes / No / Never; Never disables the option). Yes → each `.torrent` inside is added (skip dialog) with save path = parent's save path; log `Recursive download .torrent file within torrent. Source torrent: "%1". File: "%2"`.

#### 4.10.5 Auto-exit / shutdown when all downloads complete
- Flags (`Preferences/Downloads/…`, all default false): `AutoShutDownqBTOnCompletion` (Exit), `AutoShutDownOnCompletion` (Shutdown), `AutoSuspendOnCompletion` (Suspend), `AutoHibernateOnCompletion` (Hibernate), `AutoRebootOnCompletion` (Reboot). GUI menu "Tools → On Downloads Done".
- On `allTorrentsFinished`: priority Suspend > Hibernate > Shutdown > Reboot > Exit. Unless action is Exit and `ShutdownConfirmDlg/DontConfirmAutoExit` is true, a confirmation dialog with a **15 s** countdown is shown (`ShutdownConfirmDialog`; titles "Exit confirmation" / "Shutdown confirmation" / "Suspend confirmation" / "Hibernate confirmation" / "Reboot confirmation"; messages "qBittorrent will now exit." / "The computer is going to shutdown." / "…enter suspend mode." / "…enter hibernation mode." / "…reboot." + "You can cancel the action within %1 seconds."; buttons "E&xit Now"/"&Shutdown Now"/"&Suspend Now"/"&Hibernate Now"/"&Reboot Now"; for Exit a "never show again" checkbox sets `DontConfirmAutoExit`); cancel aborts, timeout = accept. For non-Exit actions the system-action flags are reset to false (one-shot), then the app exits and, after full cleanup, the OS action is performed (macOS: Apple Events `kAESleep` (Suspend), `kAERestart`, `kAEShutDown` to `loginwindow`; Hibernate not supported on macOS).

#### 4.10.6 Power management (`gui/powermanagement/*`, `MainWindow::updatePowerManagementState`)
- "Inhibit system sleep when torrents are downloading" (`Preferences/General/PreventFromSuspendWhenDownloading`, false) and "…are seeding" (`…PreventFromSuspendWhenSeeding`, false).
- Evaluated every 60 s and on preference change: busy if any torrent (a) downloading pref and torrent unfinished, not stopped, not errored, has metadata; or (b) seeding pref and finished & not stopped; or (c) is Moving (always). macOS: `IOPMAssertionCreateWithName(kIOPMAssertionTypeNoIdleSleep, "qBittorrent is active")` / release.

### 4.11 Trackers

#### 4.11.1 Tracker list text format (`trackerentry.cpp::parseTrackerEntries`)
One URL per line, trimmed. Tier starts at 0; **every empty line increments the tier** (multiple blank lines increment multiple times; a leading blank line puts the first tracker in tier 1). Equality of tracker entries is by URL only.

#### 4.11.2 Additional trackers
- "Automatically add these trackers to new downloads" (`AddTrackersEnabled` false, `AdditionalTrackers` text). Appended to every new non-private torrent and to metadata-preview downloads; tiers offset after the torrent's highest tier.
- "Automatically append trackers from URL to new downloads:" (`AddTrackersFromURLEnabled` false, `AdditionalTrackersURL`). The list is downloaded (proxy = general purposes) to `<Data>/additional_trackers_from_url.txt` at startup (cached file loaded first), when enabled/URL changed, and every **24 h**. Logs `Tracker list updated` / `Failed to update tracker list. Reason: "%1"`. Disabling clears the list and deletes the file. Appended like the above (not for metadata preview).

#### 4.11.3 Tracker status aggregation (`torrentimpl.cpp::updateTrackerEntryStatus`)
- Each tracker has endpoints = (local listen endpoint × BT protocol version v1/v2). Endpoint state: updating → isUpdating; else fails>0 with lt `tracker_failure` → TrackerError; with `announce_skipped` → Unreachable; other errors → NotWorking; else if tracker `verified` → Working; else NotContacted. Message = tracker message or error text.
- Tracker state: any updating → isUpdating; else Working if any working; else TrackerError if any; else Unreachable if all unreachable; else NotWorking if all unreachable/not working. Peers/seeds/leeches/downloaded = max over endpoints; next/min announce = earliest among endpoints with the tracker's state; message from that endpoint (for Working only if non-empty).
- Torrent announce status flags: any Working tracker with an endpoint message → HasWarning; NotWorking/Unreachable → HasOtherError; TrackerError → HasTrackerError. Used by tracker-status filters ("Tracker error", "Other error", "Warning").
- Tracker statuses are reset to NotContacted when the torrent stops or the session pauses. Status updates are only applied to running torrents.

#### 4.11.4 Tracker favicons (`gui/transferlistfilters/trackersfilterwidget.cpp`)
- Enabled by "Download tracker's favicon" (advanced; key `GUI/DownloadTrackerFavicon`, `SettingValue<bool>` without explicit default → **default false**).
- Tracker host = URL host (or the raw string if unparsable). Favicon host = host if it is an IP literal, else the **last two labels** (`tracker.example.org` → `example.org`). URL: `<scheme>://<faviconHost>/favicon.ico`, scheme = tracker scheme if it starts with `http`, else `http`.
- Downloaded to a temp file (proxy = general purposes); if it fails or can't be decoded as an image → retry once with `.png` instead of `.ico`. Downloads are deduplicated per URL; not persisted across restarts.

#### 4.11.5 Embedded tracker (advanced)
"Enable embedded tracker" (`BitTorrent/TrackerEnabled`, false), port `Preferences/Advanced/trackerPort` (9000), "Enable port forwarding for embedded tracker" (`…/trackerPortForwarding`, false). HTTP tracker at `/announce` on all interfaces; max 10000 torrents, 200 peers/torrent, interval 1800 s, default numwant 50, compact by default, supports `peers6`. Log `Embedded Tracker: Now listening on IP: %1, port: %2`.

### 4.12 Network

#### 4.12.1 Listening, interfaces, port
- Port `BitTorrent/Session/Port`: on first run (value < 0) a random port in [1024, 65535]; "Random" button generates a new one; 0 = let OS choose each start (WebAPI `random_port`). `--torrenting-port` overrides at launch. SSL listen port (`SSL/Enabled` false, `SSL/Port` random ≠ port) adds `:<port>s` endpoints for SSL torrents.
- Network interface (`Interface` name/id, `InterfaceName` display, `InterfaceAddress` "Optional IP address to bind to"): 
  - none + no address → listen on `0.0.0.0` and `::` (all).
  - address `0.0.0.0` = all IPv4; `::` = all IPv6.
  - specific address without interface → all local addresses equal to it.
  - interface without address → interface name passed to lt.
  - interface + address → matching addresses of that interface.
  - invalid address/interface → still passed to lt (no IP leak; recovers when interface returns) and logged CRITICAL (`The configured network address is invalid. Address: "%1"`, `The configured network interface is invalid. Interface: "%1"`, `Failed to find the configured network address to listen on. Address: "%1"`).
  - The same IPs/interfaces are used as lt `outgoing_interfaces` (except the wildcards). If a port is set, `max_retry_port_bind = 0` (no fallback to other ports).
  - Logs: `Trying to listen on the following list of IP addresses: "%1"`, `Successfully listening on IP. IP: "%1". Port: "%2/%3"` (proto TCP/UTP/TCP_SSL/UTP_SSL/I2P/SOCKS5…), `Failed to listen on IP. IP: "%1". Port: "%2/%3". Reason: "%4"` (CRITICAL), `Detected external IP. IP: "%1"`.
- Port forwarding (`Network/PortForwardingEnabled`, default true, "Use UPnP / NAT-PMP port forwarding from my router"): toggles lt `enable_upnp` + `enable_natpmp` (lt maps listen ports itself). Extra ports via profiles: WebUI port (if its own UPnP option) and embedded tracker port → lt `add_port_mapping(tcp)`. Logs `UPnP/NAT-PMP support: ON|OFF`, `UPnP/NAT-PMP port mapping succeeded. Message: "%1"`, `UPnP/NAT-PMP port mapping failed. Message: "%1"`. UPnP lease duration (advanced, `UPnPLeaseDuration` 0 = permanent).

#### 4.12.2 Protocol options
- Peer connection protocol (`BTProtocol`): Both → TCP+uTP in/out; TCP only; uTP only.
- uTP-TCP mixed mode (`uTPMixedMode`): Prefer TCP (`prefer_tcp`) / Peer proportional (`peer_proportional`).
- DHT (`DHTEnabled` true) `enable_dht` + bootstrap nodes (`DHTBootstrapNodes`, empty → default list); LSD (`LSDEnabled` true) `enable_lsd`; PeX (`PeXEnabled` true) plugin at startup only.
- Encryption: see 4.1 (always RC4-only allowed level, prefer RC4).
- Anonymous mode (`AnonymousModeEnabled` false): lt `anonymous_mode` (no client id/version leaks, no listen port in announces etc.).
- Other lt knobs (advanced, see preferences.md): connection speed, socket buffers, backlog, outgoing port range (`outgoing_port`, `num_outgoing_ports = max-min`), DSCP (`PeerToS` default 0x01), announce IP/port, max concurrent HTTP announces (50), stop tracker timeout (2 s), peer turnover (4 % / 90 % / 300 s), request queue size (500), choking algorithms, hostname cache TTL (1200 s), IDN support, multiple connections per IP, HTTPS tracker certificate validation (true), SSRF mitigation (true), block peers on privileged ports (false), announce to all trackers (false) / all tiers (true), max connections (500 global, 100 per torrent), max upload slots (20 global, 4 per torrent) — values ≤0 mean unlimited (-1). Per-torrent connection/upload limits are pushed to all existing torrents when changed.

#### 4.12.3 Rate limiting
- Global limits `GlobalDLSpeedLimit` / `GlobalUPSpeedLimit` and alternative `AlternativeGlobalDLSpeedLimit` / `AlternativeGlobalUPSpeedLimit` (defaults 0/0 and 10/10 KiB/s) are **stored in KiB/s** but exposed in **bytes/s**: setting ≤0 → 0 (unlimited), 1…1024 B/s → 1 KiB/s, else `bytes/1024` (floor). Active limit = alternative if `UseAlternativeGlobalSpeedLimit` else normal → lt `download_rate_limit` / `upload_rate_limit`.
- Peer classes (`configurePeerClasses`): all IPv4/IPv6 → global class. If "Apply rate limit to peers on LAN" is **unchecked** (`IgnoreLimitsOnLAN` = true) the ranges 10/8, 172.16/12, 192.168/16, 169.254/16, 127/8, fe80::/10, fc00::/7, ::1 go to lt's local (unlimited) class. TCP, SSL-TCP and I2P sockets → tcp class. If "Apply rate limit to µTP protocol" is unchecked (`uTPRateLimited` = false) uTP/SSL-uTP sockets are excluded from the global class.
- "Apply rate limit to transport overhead" → lt `rate_limit_ip_overhead` (`IncludeOverheadInLimits` default false).
- Per-torrent limits via lt `set_upload_limit/set_download_limit` (B/s, 0 unlimited).

#### 4.12.4 Alternative speed limits scheduler (`bittorrent/bandwidthscheduler.cpp`)
- Enabled by `BandwidthSchedulerEnabled` (false) with `Preferences/Scheduler/start_time` (08:00), `end_time` (20:00), `days` (EveryDay).
- On enable it immediately applies the computed mode, then re-evaluates every **30 s** and only toggles on change (manual toggles in between persist until the next computed change).
- Algorithm (replicate exactly): `alt = false`; if start > end: swap them and set `alt = true` (window wraps midnight). If `start ≤ now ≤ end` (inclusive) and today matches the day selector, `alt = !alt`. Day selector: EveryDay always; Weekday = Mon–Fri; Weekend = Sat–Sun; a specific day = that ISO weekday (Mon=1 … Sun=7, i.e. `days − 2`). Consequence: with a wrapped window and a day restriction, on non-matching days alternative limits stay on all day.
- The scheduler drives `setAltGlobalSpeedLimitEnabled`, which persists the state and emits `speedLimitModeChanged`.

#### 4.12.5 Proxy (`net/proxyconfigurationmanager.cpp`, `Network/Proxy/*`)
- Config: `Type` (None default), `IP` (default "" for None else "0.0.0.0"), `Port` (8080), `AuthEnabled`, `Username`, `Password`, `HostnameLookupEnabled` (true, "Perform hostname lookup via proxy").
- Usage profiles: `Network/Proxy/Profiles/BitTorrent` ("Use proxy for BitTorrent purposes"), `…/RSS` ("Use proxy for RSS purposes"), `…/Misc` ("Use proxy for general purposes" — search engine, software updates, GeoIP, favicons, .torrent URL downloads, tracker list URL). All default false on new profiles (migration sets BitTorrent=true).
- BitTorrent (lt): only if type ≠ None and BitTorrent profile on → lt proxy type SOCKS4 → `socks4`; HTTP → `http`/`http_pw`; SOCKS5 → `socks5`/`socks5_pw`; hostname/port/credentials; `proxy_peer_connections` = "Use proxy for peer connections" (`BitTorrent/Session/ProxyPeerConnections`, false); `proxy_hostnames` = hostname lookup option. Tracker/web-seed/DHT traffic always goes through the lt proxy when set; peer connections only with the peer option. Log on SOCKS5 failure: `SOCKS5 proxy error. Address: %1. Message: "%2".`
- HTTP client (Qt): HTTP and SOCKS5 proxies supported (with optional auth and hostname-lookup capability); **SOCKS4 is ignored** (direct connection) for non-BitTorrent traffic.
- Proxy changes trigger deferred lt reconfiguration.

#### 4.12.6 I2P (lt 2.x built with I2P)
`I2P/Enabled` (false), `I2P/Address` (127.0.0.1), `I2P/Port` (7656, SAM bridge), `I2P/MixedMode` (false, allow I2P + regular peers), inbound/outbound quantity (3/3), inbound/outbound length (3/3). Disabled → empty hostname/port 0. Errors: `I2P error. Message: "%1".` Peers blocked due to mixed-mode restrictions are logged in the peer log.

#### 4.12.7 IP filtering & bans (`bittorrent/filterparserthread.cpp`)
- Settings: `IPFilteringEnabled` (false), `IPFilter` (file path), `TrackerFilteringEnabled` (false, "Apply to trackers" → lt `apply_ip_filter_to_trackers`). Manual bans `State/BannedIPs` (sorted, unique, normalised via `QHostAddress::toString`, RFC 5952 for IPv6; invalid entries rejected with `Rejected invalid IP address while applying the list of banned IP addresses. IP: "%1"`).
- Format chosen by extension (case per `Path::hasExtension`): `.p2p` → PeerGuardian text, `.p2b` → PeerGuardian binary, `.dat` → eMule DAT; other extensions → 0 rules.
- Parsing runs on a background thread (2 MiB buffer); re-parsing aborts a running parse. Lines starting with `#` or `//` are comments. At most 5 errors are logged individually, then `%1 extra IP filter parsing errors occurred.`.
  - **DAT**: `start - end , access , description`. Fields split on the first two commas; if an access value is present and **> 127** the line is ignored (allowed range); IP range split on `-`; both ends trimmed.
  - **P2P**: `description:start-end`; split on the **last** `:` (IPv6-safe), range on `-`.
  - **P2B**: header `\xFF\xFF\xFF\xFFP2B` + version byte. v1/v2: repeated (NUL-terminated name, uint32 BE start, uint32 BE end). v3: uint32 BE name count, names (NUL-terminated), uint32 BE range count, then (uint32 name index, start, end). Invalid → `Parsing Error: The filter file is not a valid PeerGuardian P2B file.`
  - IPv4 parsing accepts leading zeros (`001.002.003.004`), each octet 0–255; otherwise generic IPv4/IPv6 parse. Errors: start/end malformed, mixed v4/v6 (`One IP is IPv4 and the other is IPv6!`), end < start (`End IP is lower than Start IP!`).
- Result: lt `ip_filter` with blocked ranges + manual bans (each a single-address blocked rule) applied atomically. Disabled filter → filter containing only manual bans. Logs `Successfully parsed the IP filter file. Number of rules applied: %1`, `Failed to parse the IP filter file` (WARNING). Signal `IPFilterParsed(error, ruleCount)` drives the GUI message.
- `banIP(ip)` (peer list "Ban peer permanently", WebAPI `transfer/banPeers`): adds a blocked rule to the live filter asynchronously and appends to the ban list.
- Peer log entries (Logger peer channel) on `peer_blocked_alert` with reason: `IP filter`, `filtered port (%1)`, `I2P mixed mode restrictions`, `privileged port (%1)`, `μTP is disabled`, `TCP is disabled`; and on `peer_ban_alert` (smart-ban; blocked=false).

#### 4.12.8 HTTP downloads (`net/downloadmanager.cpp`, `net/downloadhandlerimpl.cpp`)
- User-Agent spoofs Firefox: `Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:<V>.0) Gecko/20100101 Firefox/<V>.0` with V = 125 + whole months since 2024-04-16 (computed once). Requests may override the UA (e.g. DynDNS check uses `qBittorrent/5.2.4`).
- `Referer` header = the request URL itself. Manual redirect handling (max 20, `Exceeded max redirections (%1)`), redirect to `magnet:` returns `RedirectedToMagnet` + URI. Size limit → `The file size (%1) exceeds the download limit (%2)`. Gzip handling, Qt transfer timeout default.
- Cookies: persistent cookie jar loaded from / saved to `Network/Cookies` (session and expired cookies dropped); manageable in the GUI Cookies dialog.
- SSL errors are logged `SSL error, URL: "%1", errors: "%2"` and fail the request, unless "Ignore SSL errors" (`Preferences/Advanced/IgnoreSSLErrors`, false) → `Ignoring SSL error, ...`.
- Optional per-service sequential queues with delay (used e.g. by RSS for rate limiting).

#### 4.12.9 Dynamic DNS (`net/dnsupdater.cpp`; WebUI options page)
`Preferences/DynDNS/Enabled` (false), `Service` (DynDNS=0 / NoIP=1), `DomainName` (`changeme.dyndns.org`), `Username`, `Password`. External IP checked via `http://checkip.dyndns.org` every 30 min; on change GET `https://members.dyndns.org/nic/update?hostname=<domain>&myip=<ip>` (No-IP: `dynupdate.no-ip.com`) with basic auth. Response = first word of body: `good`/`nochg` → `Your dynamic DNS was successfully updated.`; `911`/`dnserr` → `Dynamic DNS error: The service is temporarily unavailable, it will be retried in 30 minutes.`; any other code stops the IP-check timer: `nohost` (`...hostname supplied does not exist under specified account.`) and `badauth` (`...Invalid username/password.`) → state INVALID_CREDS (resumes when the user changes domain/username/password); `badagent`, `!donator`, `abuse` → state FATAL (never retried this session). Local validation errors: `supplied domain name is invalid.`, `supplied username is too short.`, `supplied password is too short.` (all Log::CRITICAL). Registration links: `https://account.dyn.com/entrance/`, `https://www.noip.com/remote-access`.

### 4.13 Content layout, pieces, seeding modes

#### 4.13.1 Content layout
- Original (0): keep metadata layout. Subfolder (1, "Create subfolder"): if the torrent has no root folder (single-file torrent), wrap files in a folder named after the **first file's name without extension**. NoSubfolder (2, "Don't create subfolder"): strip the common root folder.
- Applied at add (with metadata) or when magnet metadata arrives (only if the user did not rename files). The original layout is detected via `Path::findRootFolder` (common first component).
- Content path = single file → `<location>/<file>`; multi-file with root → `<location>/<root>`; without root → `<location>`. Root path = `<location>/<root>` or empty.

#### 4.13.2 Sequential download / super seeding / DHT-PeX-LSD per torrent
lt flags, persisted in lt resume data; see 4.4.12. Super seeding can be enabled by share-limit action.

#### 4.13.3 First and last piece priority (`applyFirstLastPiecePriority`)
For every file with priority > Ignored: N = ceil(fileSize × 1% / pieceLength) pieces at the beginning and N at the end of the file's piece range get Maximum (7) priority; pieces in between get the file's priority; pieces of ignored files stay 0. Disabling restores file priorities. Re-applied after priority changes and after metadata arrives.

### 4.14 GeoIP and reverse DNS
- Enabled by "Resolve peer countries" (`Preferences/Connection/ResolvePeerCountries`, default true); disabling unloads the DB.
- Database: DB-IP "IP to Country Lite" MaxMind-DB format, stored decompressed at `<Data>/GeoDB/dbip-country-lite.mmdb`. Download URL `https://download.db-ip.com/free/dbip-country-lite-<yyyy-MM>.mmdb.gz` (UTC year-month), proxy = general purposes, gunzipped, validated by loading, replaced only if its build epoch is newer.
- Update check only when the DB is (re)loaded (startup / enabling): download if missing, or if (build year < current year OR build month < current month) and today's day-of-month > 1 (expired).
- Logs: `IP geolocation database loaded. Type: %1. Build time: %2.`, `Couldn't load IP geolocation database. Reason: %1`, `Couldn't download IP geolocation database file. Reason: %1`, `Could not decompress IP geolocation database file.`, `Successfully updated IP geolocation database.`, `Couldn't save downloaded IP geolocation database file. Reason: %1`.
- Lookup returns ISO 3166-1 alpha-2 code; `CountryName()` maps codes to localized names ("N/A" for unknown). Flags come from bundled icon set (UI).
- Reverse DNS of peers ("Resolve peer host names", `…/ResolvePeerHostNames`, default false): async `QHostInfo` lookup per peer IP, LRU cache of 2048 entries; a result equal to the IP string is treated as no hostname.

### 4.15 Logging
- In-memory `Logger` (`base/logger.cpp`): two ring buffers of 20000 entries — messages `{id, type, timestamp(s), message}` and peer events `{id, blocked, timestamp, ip, reason}`. IDs are monotonically increasing per process; `getMessages(lastKnownId)` / `getPeers(lastKnownId)` return entries after that ID (all if -1 or if gap exceeds buffer). Used by GUI Execution Log and WebAPI `log/main`, `log/peers`.
- File logger (`app/filelogger.cpp`), settings `Application/FileLogger/…`: `Enabled` (true), `Path` (default `<Data>/logs`), `Backup` (true, "Backup the log file after:"), `MaxSizeBytes` (default 65 KiB = 66560, clamped 1 KiB–1000 MiB; UI in KiB), `DeleteOld` (true, "Delete backup logs older than:"), `Age` (1, clamped 1–365), `AgeType` (1 = months; 0 days, 1 months, 2 years).
  - File `qbittorrent.log`; line format `(N|I|W|C) <ISO-8601 local datetime> - <message>\n`.
  - When Backup is on and size ≥ max → close, rename to `qbittorrent.log.bak`, or `.bak1`, `.bak2`… (first free), reopen.
  - Delete-old at startup / when enabled: delete `qbittorrent.log.bak*` whose mtime + age < now.
  - Flushed 2 s after the last write.
- Startup log lines: `qBittorrent %1 started. Process ID: %2`, `Running in portable mode. Auto detected profile folder at: %1` or `Using config directory: %1`, `Redundant command line flag detected: "%1". Portable mode implies relative fastresume.`

### 4.16 Persistence

#### 4.16.1 Resume data (default: "Fastresume files", `ResumeDataStorageType` = Legacy)
Directory `<Data>/BT_backup/`:
- `<TorrentID>.fastresume` — bencoded lt resume data (`lt::write_resume_data`) **minus** `info`, `creation date`, `created by`, `comment` (which go to the .torrent), with save path converted to profile-relative form when portable/relative mode is on, plus qBittorrent extension keys:
  | Key | Type | Meaning |
  |---|---|---|
  | `qBt-savePath` | string | Manual save path (written only when ATM off; absence/empty ⇒ ATM on). |
  | `qBt-downloadPath` | string | Manual download path (only when ATM off; "" = none). |
  | `qBt-category` | string | Category. |
  | `qBt-tags` | list of strings | Tags. |
  | `qBt-name` | string | Explicit name ("" = none). |
  | `qBt-comment` | string | Comment override. |
  | `qBt-ratioLimit` | int | ratio × 1000 (legacy: string with a real number). -2000 default, -1000 none. |
  | `qBt-seedingTimeLimit` | int | minutes; -2 default, -1 none. |
  | `qBt-inactiveSeedingTimeLimit` | int | minutes; -2 / -1. |
  | `qBt-shareLimitAction` | string | `Default` / `Stop` / `Remove` / `RemoveWithContent` / `EnableSuperSeeding`. |
  | `qBt-seedStatus` | int(bool) | "has finished status" (download previously completed). |
  | `qBt-contentLayout` | string | `Original` / `Subfolder` / `NoSubfolder` (legacy `qBt-hasRootFolder` int: 1→Original, 0→NoSubfolder). |
  | `qBt-firstLastPiecePriority` | int(bool) | First/last piece priority. |
  | `qBt-stopCondition` | string | `None` / `MetadataReceived` / `FilesChecked`. |
  | `qBt-sslCertificate`, `qBt-sslPrivateKey`, `qBt-sslDhParams` | string (PEM) | Only written when set. |
  Stopped/forced state is encoded in lt flags: stopped ⇒ `paused=1, auto_managed=0`; running AutoManaged ⇒ `auto_managed=1` (paused as lt had it); Forced ⇒ `paused=0, auto_managed=0`. On load: `stopped = paused && !auto_managed`; mode = AutoManaged if `paused || auto_managed` else Forced. lt `stop_when_ready` ⇒ stop condition FilesChecked.
- `<TorrentID>.torrent` — the metadata (`info` dict + creation date, created by, comment), when known.
- `queue` — text file, one 40-hex TorrentID per line in queue order (only torrents with queue position ≥ 0; lines >48 chars ignored). On load, registered torrents are reordered so queued torrents are restored in this order.
- Loading validates: bdecode limits, info-hash consistency between resume data and .torrent (`Mismatching info-hash detected in resume data`), non-empty save path (`Corrupted resume data: save_path is invalid`), metadata or `info-hash` present (`Resume data is invalid: neither metadata nor info-hash was found`). Failures: `Failed to resume torrent. Torrent: "%1". Reason: "%2"` (CRITICAL). Success: `Restored torrent. Torrent: "%1"`; lt load failure `Failed to load torrent. Reason: "%1"`.
- Writes are asynchronous on a worker thread, written atomically (`Utils::IO::saveToFile`). Errors: `Couldn't save torrent metadata to '%1'. Error: %2.`, `Couldn't save torrent resume data to '%1'. Error: %2.`, `Couldn't save data to '%1'. Error: %2`, `Couldn't load torrents queue: %1`.

#### 4.16.2 SQLite resume storage (`ResumeDataStorageType` = SQLite, "SQLite database (experimental)")
File `<Data>/torrents.db`, WAL journal mode (`PRAGMA journal_mode = WAL`; failure logged `Couldn't enable Write-Ahead Logging (WAL) journaling mode. Error: %1.`). DB_VERSION = 9 (with migrations for older schemas).
```sql
CREATE TABLE "meta" (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, value BLOB);   -- ('version', 9)
CREATE TABLE "torrents" (
  id INTEGER PRIMARY KEY,
  torrent_id BLOB NOT NULL UNIQUE,               -- 40-hex TorrentID text
  queue_position INTEGER NOT NULL DEFAULT -1,
  name TEXT, category TEXT, tags TEXT,            -- tags comma-joined
  comment TEXT,
  target_save_path TEXT, download_path TEXT,     -- NULL/empty ⇒ ATM
  content_layout TEXT NOT NULL,
  ratio_limit INTEGER NOT NULL,                  -- ×1000
  seeding_time_limit INTEGER NOT NULL,
  inactive_seeding_time_limit INTEGER NOT NULL,
  share_limit_action TEXT NOT NULL DEFAULT `Default`,
  has_outer_pieces_priority INTEGER NOT NULL,    -- first/last piece priority
  has_seed_status INTEGER NOT NULL,
  operating_mode TEXT NOT NULL,                  -- AutoManaged | Forced
  stopped INTEGER NOT NULL,
  stop_condition TEXT NOT NULL DEFAULT `None`,
  ssl_certificate TEXT, ssl_private_key TEXT, ssl_dh_params TEXT,
  libtorrent_resume_data BLOB NOT NULL,          -- bencoded, without info dict
  metadata BLOB                                  -- bencoded info dict + creation date/created by/comment
);
```
Rows are upserted (`ON CONFLICT(torrent_id) DO UPDATE`), loaded `ORDER BY queue_position`. Queue is stored by updating `queue_position`.
- Switching storage type takes effect at restart: on startup, if the selected storage is empty/missing but the other one exists, everything is loaded from the other one and re-stored into the selected one; when migrating SQLite→Legacy the DB file is deleted afterwards; the queue is re-saved.

#### 4.16.3 Saving cadence
- Every torrent property change calls `deferredRequestResumeData()` (coalesced to the next event-loop iteration).
- Periodic: every `SaveResumeDataInterval` minutes (default 60; 0 disables) for torrents with lt `need_save_resume` or pending changes.
- Queue saved after the next state update when changed.
- Statistics: see 4.16.4.
- Shutdown: request resume data (`only_if_modified`) for all torrents, drop queued moves except active one, then process alerts until all resume data/move/queue writes done; abort if no relevant alert for 30 s and no move in progress (`Aborted saving resume data. Number of outstanding torrents: %1`, CRITICAL). Then save statistics, delete lt session on a thread and wait up to `ShutdownTimeout` seconds (`BitTorrent session shutdown timeout [-1: unlimited]`, default -1): `BitTorrent session successfully finished.` / `Session shutdown timed out.`. Logs `qBittorrent termination initiated`, `Saving resume data completed.`, `qBittorrent is now ready to exit`.
- Startup: resume records processed in batches of ≤50 concurrent lt adds; startup progress (0–100%) emitted (startup progress dialog "Loading torrents..." with Exit button); session resumed (unless StartPaused) after all loaded; `restored()` emitted; only then new torrents can be added and command-line/queued params processed.

#### 4.16.4 Statistics persistence
- All-time totals stored in a separate settings file `qBittorrent-data.ini` (`.conf` on Linux) in the same directory as the main config, key `Stats/AllStats` = map `{AlltimeDL: <bytes>, AlltimeUL: <bytes>}`.
- Session totals = lt `net.recv_bytes + net.recv_ip_overhead_bytes` (and sent counterpart) — i.e. **including protocol overhead**; all-time = previously stored + session totals.
- Saved when dirty every `SaveStatisticsInterval` minutes (default 15; 0 disables periodic save) and on exit.
- Session status refresh: every `RefreshInterval` ms (default 1500) the session posts torrent updates and session stats; rates computed as byte deltas / elapsed time for payload, total (incl. overhead), IP overhead, DHT, tracker; wasted = redundant + failed bytes; DHT nodes; peers connected; disk queues; cache stats.

#### 4.16.5 Profile / configuration locations (`base/profile_p.cpp`)
Directories (Config / Data / Cache), default profile:
| OS | Config dir | Data dir | Main settings file |
|---|---|---|---|
| macOS | `~/Library/Preferences/qBittorrent` | `~/Library/Application Support/qBittorrent` | `~/.config/qBittorrent/qBittorrent.ini` (Qt INI, user scope) |
| Linux | `~/.config/qBittorrent` | `~/.local/share/qBittorrent` (legacy `~/.local/share/data/qBittorrent` used if it exists and the new one doesn't) | `~/.config/qBittorrent/qBittorrent.conf` |
| Windows | `%APPDATA%\qBittorrent` | `%LOCALAPPDATA%\qBittorrent` | `%APPDATA%\qBittorrent\qBittorrent.ini` |
Cache = OS cache location (`~/Library/Caches/qBittorrent` on macOS). `--configuration=<name>` appends `_<name>` to every directory/profile name.
Custom profile (`--profile=<dir>` or portable mode): base `<dir>/qBittorrent[_<name>]/` with `config/`, `data/`, `cache/`, `downloads/` (default save path); settings file `config/qBittorrent.ini` (`.conf` on Linux). **Portable mode**: auto-enabled when no `--profile` given and a folder named `profile` exists next to the executable; implies relative fastresume paths.
Relative paths mode (`--relative-fastresume` or portable): save paths stored in resume data relative to the profile base path (Windows: only if on the same drive).

Files and folders:
| Location | File | Content |
|---|---|---|
| main settings | `qBittorrent.ini/.conf` | All `SettingValue`s. Saved 5 s after a change (debounced) by writing `qBittorrent_new.ini` then renaming over the original (crash-safe). Key `Meta/MigrationVersion` = 8. |
| main settings dir | `qBittorrent-data.ini/.conf` | Statistics. |
| Config | `categories.json` | Categories (4.6.2). |
| Config | `watched_folders.json` | Watched folders (4.9.1). |
| Config | `rss/feeds.json`, `rss/download_rules.json` | RSS config (see RSS section). |
| Config | `themes/default/` | UI theme overrides. |
| Data | `BT_backup/` or `torrents.db` | Resume data. |
| Data | `logs/qbittorrent.log*` | Default file-log location. |
| Data | `GeoDB/dbip-country-lite.mmdb` | GeoIP DB. |
| Data | `rss/articles/` | RSS article storage. |
| Data | `nova3/` | Search engine (plugins in `nova3/engines/`). |
| Data | `SearchUI/History.txt`, `SearchUI/Session.json` | Search UI history / open tabs. |
| Data | `additional_trackers_from_url.txt` | Cached tracker list from URL. |
| Data | `web_clientdata.json` | WebUI client-side data. |
| Cache | `rss/` | RSS HTML browser cache. |
| Temp | `<tmp>/.qBittorrent/` | Metadata-preview downloads; removed at exit. |
| Config dir | single-instance lock/socket | `ApplicationInstanceManager` (QtLocalPeer) keyed by the config path. |

#### 4.16.6 Settings migration (`app/upgrade.cpp`, MIGRATION_VERSION = 8)
Applied sequentially when `Meta/MigrationVersion` < N: v1 export WebUI HTTPS cert/key from settings to files, content-layout / listen-port / scheduler-days / DynDNS-service / tray-style enum upgrades; v2 key renames; v3 proxy type ints → names (0 None, 1 HTTP, 2 SOCKS5, 3 HTTP_PW, 4 SOCKS5_PW, 5 SOCKS4); v4 (Windows) memory priority key move; v5 `StartMinimized`+`MinimizeToTray` → `GUI/StartUpWindowState` (Normal/Minimized/Hidden), locale `zh` → `zh_CN`; v6 proxy profiles from `OnlyForTorrents`, `HTTP_PW`/`SOCKS5_PW` → type + AuthEnabled, `ProxyHostnameLookup` → `Network/Proxy/HostnameLookupEnabled`; v7 `MaxRatioAction` int (0 Stop, 1 Remove, 2 EnableSuperSeeding, 3 RemoveWithContent) → `ShareLimitAction`; v8 `AddTorrentPaused` → `AddTorrentStopped`. Changed defaults: `QueueingSystemEnabled` legacy true / current false (written only if absent). Fresh profiles are stamped with the current version.

### 4.17 Disk I/O behaviour (summary; keys in preferences.md)
- Disk IO type (requires restart): Default → lt default disk IO (mmap on 64-bit); MMap; Posix; SimplePreadPwrite = mmap backend with `mmap_file_size_cutoff = INT_MAX` and `disk_write_mode = always_pwrite`. All wrapped by qBittorrent's `CustomDiskIOThread` (`.!qB` handling 4.8.2).
- Async IO threads (10), hashing threads (1), file pool size (100), outstanding memory when checking (`CheckingMemUsageSize` 32 MiB → ×64 blocks), disk queue size (1 MiB), OS cache modes for read/write (EnableOSCache), piece extent affinity (false), suggest mode (false), send buffer watermark (500 KiB / low 10 KiB / factor 50 %), coalesce reads/writes (lt1 only), disk cache size/TTL (lt1 only).
- Performance warnings (if enabled) logged `Performance alert: %1. More info: https://libtorrent.org/reference-Alerts.html#enum-performance-warning-t`. Alert queue overflow: `Error: Internal alert queue is full and alerts are dropped, you might see degraded performance. Dropped alert type: "%1". Message: "%2"` (CRITICAL). Session error: `BitTorrent session encountered a serious error. Reason: "%1"`.

### 4.18 Disk space, missing files, error recovery
- Free disk space of the **default save path** is checked on the IO thread at start and then every 30 s (and immediately when the default save path changes); exposed as `free_space_on_disk` (status bar / WebAPI). No automatic pausing on low space.
- **MissingFiles**: lt rejected the fastresume (files moved/missing or storage inaccessible) → torrent paused, state MissingFiles, log `Failed to restore torrent. Files were probably moved or storage isn't accessible. Torrent: "%1". Reason: "%2"`. Progress from the old resume data is preserved. Recovery: Start (reloads/rechecks), Force recheck, or Set location (moves/points to new location and reloads).
- **Error** (write failure → lt upload_mode, or lt error): Start clears error flags. 
- Resume data generation failures: `Generate resume data failed. Torrent: "%1". Reason: "%2"` (CRITICAL; "not modified" ignored).
- URL seed failures: `URL seed connection failed. Torrent: "%1". URL: "%2". Error: "%3"` / `Received error message from URL seed...`.

### 4.19 Single instance & parameter passing (`app/applicationinstancemanager.cpp`, `app/application.cpp`)
- One instance per configuration (profile + configuration name): a local socket server (QtLocalPeer) named from the config dir. A second launch sends its parameters to the first and exits (`callMainInstance`, default timeout). An **empty** message means "activate": show/raise the main window (or the startup progress dialog while loading).
- Message format: items joined by `|`; option items first, then sources: `@savePath=<path>`, `@addStopped=1|0`, `@skipChecking`, `@category=<name>`, `@sequential`, `@firstLastPiecePriority`, `@skipDialog=1|0`, then each torrent source (absolute file path if it exists, else verbatim URL/magnet).
- Received params are queued until the session is restored and UI/WebUI created, then processed: each source is added with the given AddTorrentParams; dialog shown per `@skipDialog` (true → skip, false → force show, absent → preference).

### 4.20 Command-line options (`app/cmdoptions.cpp`)
Usage: `qbittorrent [options] [(<filename> | <url>)...]`. An argument is treated as an option if it starts with `--` and does not end with `.torrent`, or is `-x` (2 chars). Unknown option → error. Every option can be given as environment variable `QBT_<NAME>` (upper-case, `-`→`_`); flags set via `1`/`TRUE`; tri-state via `true|false|1|0` (empty = default value); **command line overrides env**.
| Option | Env | Meaning |
|---|---|---|
| `-h`, `--help` | `QBT_HELP` | "Display this help message and exit" |
| `-v`, `--version` | `QBT_VERSION` | "Display program version and exit" (not on Windows GUI build) |
| `--confirm-legal-notice` | `QBT_CONFIRM_LEGAL_NOTICE` | "Confirm the legal notice" (skip first-run legal notice) |
| `--webui-port=<port>` | `QBT_WEBUI_PORT` | "Change the WebUI port" (1–65535; persisted to settings) |
| `--torrenting-port=<port>` | `QBT_TORRENTING_PORT` | "Change the torrenting port" (1–65535; persisted to `BitTorrent/Session/Port`) |
| `--no-splash` | `QBT_NO_SPLASH` | "Disable splash screen" (GUI build) |
| `-d`, `--daemon` | `QBT_DAEMON` | "Run in daemon-mode (background)" (nox build, non-Windows) |
| `--profile=<dir>` | `QBT_PROFILE` | "Store configuration files in <dir>" (made absolute) |
| `--configuration=<name>` | `QBT_CONFIGURATION` | "Store configuration files in directories qBittorrent_<name>" |
| `--relative-fastresume` | `QBT_RELATIVE_FASTRESUME` | "Hack into libtorrent fastresume files and make file paths relative to the profile directory" |
| `files or URLs` | — | "Download the torrents passed by the user" |
| `--save-path=<path>` | `QBT_SAVE_PATH` | "Torrent save path" |
| `--add-stopped[=<true|false>]` | `QBT_ADD_STOPPED` | "Add torrents as running or stopped" (bare flag = true) |
| `--skip-hash-check` | `QBT_SKIP_HASH_CHECK` | "Skip hash check" |
| `--category=<name>` | `QBT_CATEGORY` | "Assign torrents to category. If the category doesn't exist, it will be created." |
| `--sequential` | `QBT_SEQUENTIAL` | "Download files in sequential order" |
| `--first-and-last` | `QBT_FIRST_AND_LAST` | "Download first and last pieces first" |
| `--skip-dialog[=<true|false>]` | `QBT_SKIP_DIALOG` | "Specify whether the "Add New Torrent" dialog opens when adding a torrent." (bare = true) |
Errors: `%1 must specify a valid port (1 to 65535).`, `Parameter '%1' must follow syntax '%1=%2'`. Help footer explains env vars with example `QBT_NO_SPLASH=1 qbittorrent`.

### 4.21 Startup / shutdown sequence (application level)
1. Parse command line; init Logger; determine profile (portable detection) → Profile; single-instance manager; SettingsStorage + Preferences; first-run: stamp migration version + current defaults, else run migrations; load translation; open file logger; apply `--webui-port` / `--torrenting-port`.
2. `exec()`: init ProxyConfigurationManager, DownloadManager, BitTorrent Session (constructor: lt session created **paused**, categories/tags loaded, share-limit timer, additional trackers, excluded-name regexes, IO thread (free-space checker, file searcher, content remover), statistics loaded, PortForwarder, embedded tracker, resume data loading started, trackers-from-URL timer), desktop integration, startup progress dialog (shown after 1 s if still loading; hidden if starting minimized to tray).
3. On `restored`: connect added/finished/allFinished handlers, AddTorrentManager, GeoIPManager, ReverseResolution, TorrentFilesWatcher, RSS Session + AutoDownloader, notifications, MainWindow, WebUI; then process queued parameters.
4. Exit (`cleanup()`, once): tooltip `qBittorrent is shutting down...`, close Add dialogs, main window cleanup, WebUI, RSS, watcher, **Session destruction (4.16.3)**, other singletons, remove temp dir, file logger closed, then OS shutdown action if requested (4.10.5).

---

## 5. Search engine & RSS

Sources: `src/base/search/{searchpluginmanager,searchhandler,searchdownloadhandler}.cpp`, `src/searchengine/nova3/*`, `src/base/utils/foreignapps.cpp`, `src/gui/search/*`, `src/gui/mainwindow.cpp` (Python bootstrap), `src/base/rss/*`, `src/gui/rss/*`, `src/base/net/downloadmanager.cpp` (per-host request serialization).

Profile folders referenced below (macOS default profile, Qt `QStandardPaths`):
- **Data** = `AppLocalDataLocation` → `~/Library/Application Support/qBittorrent`
- **Config** = `AppConfigLocation` → `~/Library/Preferences/qBittorrent`
- **Cache** = `CacheLocation` → `~/Library/Caches/qBittorrent`
(With `--profile`/`--configuration` these become `<root>/qBittorrent[_<conf>]/{data,config,cache}`.)

---

### 5.1 Search engine — architecture overview

- The search engine is an external **Python 3** program ("nova3") that qBittorrent runs as a child process for each search. Search plugins are third-party `.py` files. qBittorrent itself contains **no** site-specific search code.
- qBittorrent ships five helper scripts as Qt resources under `:/searchengine/nova3/` (`searchengine.qrc`): `helpers.py` (VERSION 1.55), `nova2.py` (1.53), `nova2dl.py` (1.26), `novaprinter.py` (1.53), `socks.py` (1.00, a vendored PySocks). **No plugins are bundled.** Official plugins come from the update server (§5.4).
- There is **no `version.txt`** in 5.2.4. Each script/plugin carries its own version in a header comment `# VERSION: x.y`.
- Components:
  - `SearchPluginManager` (singleton): engine dir layout, plugin install/update/uninstall/enable, capabilities discovery, proxy env, category names.
  - `SearchHandler`: one running search (one `nova2.py` process), streaming result parsing, 3‑minute timeout, cancellation.
  - `SearchDownloadHandler`: one `nova2dl.py` process that downloads a `.torrent` through the plugin.
  - GUI: `SearchWidget` (the tab), `SearchJobWidget` (one result tab), `SearchSortModel` (filter/sort), `PluginSelectDialog`, `PluginSourceDialog`.
  - WebAPI `search/*` uses the same manager/handlers (see WebAPI section).

### 5.2 Locating Python (`Utils::ForeignApps::pythonInfo()`)

- Constants: `PYTHON_ISOLATE_MODE_FLAG = "-I"`, `PYTHON_UTF8_MODE_FLAG = "-Xutf8=1"`, `PythonInfo::MINIMUM_SUPPORTED_VERSION = 3.9.0` (`Utils::Version<3,1>`: 1–3 numeric components).
- Result is cached in a static; it is re-detected only when the cached one is invalid or the preferred path changed.
- **Preferred path**: setting `Preferences/Search/pythonExecutablePath` (Advanced settings row "Python executable path (may require restart)", placeholder "(Auto detect if empty)"). If non-empty, ONLY that path is tested (no auto-detect):
  - test OK but version < 3.9 → log WARNING `Python failed to meet minimum version requirement. Path: "%1". Found version: "%2". Minimum supported version: "%3".` (pyInfo still returned → `isValid()` true but `isSupportedVersion()` false).
  - test failed → log WARNING `Failed to find Python executable. Path: "%1".`
- **Auto-detect** (preferred path empty, and cache invalid):
  1. Try `python3`, then `python` (resolved through `PATH`). For each failure log INFO `Failed to find \`%1\` executable in PATH environment variable. PATH: "%2"`; for too-old versions log the min-version message at INFO and continue.
  2. Windows only: registry `HKCU`, then `HKLM` 64-bit view, then `HKLM` 32-bit view, key `SOFTWARE\Python\PythonCore\<ver>\InstallPath` (default value), versions sorted by natural compare, highest first; candidates `<InstallPath>\python3.exe` then `python.exe`. Fallback: directories `C:/Python*` (name-sorted descending) with the same two exe names.
  3. Nothing usable → WARNING `Failed to find Python executable`.
- **Version test** (`testPythonInstallation`): run `<exe> --version` (Unix: close inherited FDs), wait for finish (default 30 s Qt timeout), require normal exit. Read stdout; if empty, read stderr. `simplified()`, split on spaces, need ≥2 tokens; token[1] truncated at the first char matching `[^\.\d]` (handles `3.6.6+`, Anaconda `Python 3.4.3 :: Anaconda …`), parsed as version. On success log INFO `Found Python executable. Name: "%1". Version: "%2"`.

#### 5.2.1 Enabling the Search tab (View ▸ Search Engine) — `MainWindow::on_actionSearchWidget_triggered`
- Setting `Preferences/Search/SearchEnabled` (bool, default **false**) persists whether the tab is shown. Search tab is inserted at tab index 1, title "Search" (icon `edit-find` on non-macOS). Shortcut **Alt+2** switches to it (enabling it if needed).
- When the user checks the action:
  - Python not found → uncheck, `SearchEnabled=false`, dialog title "Missing Python Runtime":
    - Windows: question "Python is required to use the search engine but it does not seem to be installed.\nDo you want to install it now?" (Yes/No, default Yes) → Yes runs the installer flow.
    - Other OSes (macOS): information "Python is required to use the search engine but it does not seem to be installed."
  - Python too old → uncheck, dialog title "Old Python Runtime":
    - Windows: "Your Python version (%1) is outdated. Minimum requirement: %2.\nDo you want to install a newer version now?"
    - Other: "Your Python version (%1) is outdated. Please upgrade to latest version for search engines to work.\nMinimum requirement: %2."
- **Windows installer flow** (not needed on macOS, listed for completeness): downloads `https://www.python.org/ftp/python/3.14.2/python-3.14.2-amd64.exe` (general-purpose proxy setting), action disabled with tooltip "Python installation in progress...", wait cursor; verifies MD5 `c887e19e66e66e6961c444283dafaa33` AND SHA3-512 `b5d83ec9…e450`; renames temp file to `.exe`; runs it with `/passive`; on exit code 0 logs "Python installation success.", deletes installer, re-detects, and re-enables the Search tab if version supported; otherwise logs "Python installation failed." + "Exit code: %1." / "Reason: installer crashed.". Download failure → warning "Download error" / "Python installer could not be downloaded. Error: %1.\nPlease install it manually.".
- Main window notifications (only if notifications enabled and the Search tab is not current): title "Search Engine", text "Search has finished" or "Search has failed".

### 5.3 Engine directory layout & bootstrap (`SearchPluginManager`)

- `engineLocation()` = `<Data>/nova3` (created on first use).
- `pluginsLocation()` = `<Data>/nova3/engines`.
- Layout:
  ```
  <Data>/nova3/__init__.py          (empty, created/truncated at startup)
  <Data>/nova3/helpers.py           (copied from resources)
  <Data>/nova3/nova2.py
  <Data>/nova3/nova2dl.py
  <Data>/nova3/novaprinter.py
  <Data>/nova3/socks.py
  <Data>/nova3/engines/__init__.py  (empty, created/truncated at startup)
  <Data>/nova3/engines/<plugin>.py  (one module per plugin; module name == plugin id)
  <Data>/nova3/engines/<plugin>.png | <plugin>.ico   (optional icon; .png preferred over .ico)
  ```
- **`updateNova()`** (constructor): create the two `__init__.py` files, `mkdir engines`, then for each of the 5 bundled scripts: if `version(bundled) > version(on-disk)` (missing file ⇒ invalid version ⇒ always copy) → delete disk copy and copy bundled one.
- **Version parsing** (`getPluginVersion(path)`): open as text; read line by line with `readLine(16)` (max 15 chars per read), strip **all spaces**; first line starting (case-insensitive) with `#VERSION:` → remainder parsed as `PluginVersion = Utils::Version<2>` (exactly `major.minor`, both numeric, max 2 components). Invalid → WARNING `Search plugin '%1' contains invalid version string ('%2')` and returns invalid version. No such line → invalid version.
- **Capabilities discovery** (`update()`, called at construction, after every install/rollback): runs **synchronously** `python -I -Xutf8=1 <engineLocation>/nova2.py --capabilities` with the proxy environment (§5.5); waits for exit (Qt default 30 s). Non-empty stderr → WARNING `Error occurred when fetching search engine capabilities. Error: "%1".`. Stdout parsed as XML; root must be `<capabilities>`; each child element's **tag name** is the plugin id:
  ```xml
  <capabilities>
    <engine_module_name>
      <name>long name</name>
      <url>http://example.com</url>
      <categories>movies music games</categories>   <!-- space separated, "all" excluded, sorted -->
    </engine_module_name>
  </capabilities>
  ```
  For each: `PluginInfo{ name=<tag>, version=getPluginVersion(engines/<tag>.py), fullName=<name>, url=<url>, supportedCategories=<categories split on ' ', trimmed, non-empty>, enabled = !(tag ∈ SearchEngines/disabledEngines), iconPath }`. New id → emit `pluginInstalled`; existing id with different version → replace + emit `pluginUpdated`. (Plugins missing from output are NOT removed from the in-memory map here.)
- `PluginInfo` struct: `name` (id), `version`, `fullName`, `url` (site URL; also used to map results back to plugin), `supportedCategories`, `iconPath`, `enabled`.
- `clearPythonCache(engineLocation)`: before every install/uninstall, recursively delete every `__pycache__` directory and every `*.pyc` file under `nova3/`.

### 5.4 Plugin management

#### 5.4.1 Settings
- `SearchEngines/disabledEngines` — QStringList of disabled plugin ids (default empty ⇒ all enabled). `enablePlugin(name, enabled)` updates it and emits `pluginEnabled`.

#### 5.4.2 Update server
- Base URL (`m_updateUrl`): **`https://raw.githubusercontent.com/qbittorrent/search-plugins/refs/heads/master/nova3/engines/`**
- **Check for updates** downloads `<base>versions.txt` (uses general-purpose proxy setting `Network/Proxy/Profiles/Misc`).
  - Format: one `name: x.y` per line; blank lines and lines starting `#` ignored; split on `:` must yield exactly 2 parts and a valid 2-component version, else counted invalid.
  - For each valid line: `isUpdateNeeded` = **plugin not installed** OR `new > installed`. Needed ones are logged INFO `Plugin "%1" is outdated, updating to version %2` and collected. ⇒ "Check for updates" also **installs every official plugin not yet installed** — this is how a fresh install obtains plugins.
  - Any invalid lines → `checkForUpdatesFailed("Incorrect update info received for %1 out of %2 plugins.")`; else `checkForUpdatesFinished(map name→version)`.
  - Download failure → `checkForUpdatesFailed("Update server is temporarily unavailable. %1")`.
- `updatePlugin(name)` = `installPlugin("<base><name>.py")`.

#### 5.4.3 Install (`installPlugin(source)`)
1. `clearPythonCache`.
2. If `source` starts with `<scheme>:` for any scheme supported by `QNetworkAccessManager` (`DownloadManager::hasSupportedScheme`; Qt 6 typically reports `http`, `https`, `file`, `qrc`, `data` — so a `file:` URL actually goes through this download branch; a rewrite should simply treat `http(s)://` as remote and everything else as a local path) → download to temp file (general-purpose proxy). On success: plugin id = URL path's file name without extension; `installPlugin_impl(id, tempFile)`; delete temp. On failure: id = last path segment with `.py` removed (case-insensitive); emit `pluginUpdateFailed` if id installed else `pluginInstallationFailed`, reason `Failed to download the plugin file. %1`.
3. Else local path (a `file:` URL is converted to a local path). File name must end with `.py` (case-insensitive) → id = name without extension → `installPlugin_impl`; else `pluginInstallationFailed(name, "Unknown search engine plugin file format.")`.
- `installPlugin_impl(id, path)`:
  1. `newVersion = getPluginVersion(path)`. If id already installed and NOT (`installed < new`) → INFO `Plugin already at version %1, which is greater than %2`; `pluginUpdateFailed(id, "A more recent version of this plugin is already installed.")`; stop.
  2. Dest = `engines/<id>.py`. If it exists: copy to `<id>.py.bak`, delete dest, `updated=true`.
  3. Copy file to dest; run `update()` (capabilities).
  4. If id is not in the plugin map afterwards (import failed / not a valid engine): delete dest; INFO `Plugin %1 is not supported.`; if `updated` → restore `.bak`, delete `.bak`, `update()`, emit `pluginUpdateFailed(id, "Plugin is not supported.")`; else `pluginInstallationFailed(id, "Plugin is not supported.")`.
  5. Success: if `updated` → INFO `Plugin %1 has been successfully updated.` and delete `.bak`. (The `pluginInstalled`/`pluginUpdated` signals come from `update()`.)
#### 5.4.4 Uninstall
- `uninstallPlugin(id)`: `clearPythonCache`; delete every file in `engines/` matching `<id>.*` (script, icon, .bak); remove from map; emit `pluginUninstalled`; **always returns true** in 5.2.4 (no built-in plugins exist, so the GUI's "cannot uninstall bundled plugin" branch is dead but its strings still exist: "Uninstall warning" / "Some plugins could not be uninstalled because they are included in qBittorrent. Only the ones you added yourself can be uninstalled.\nThose plugins were disabled.").
- Success message: "Uninstall success" / "All selected plugins were uninstalled successfully".

#### 5.4.5 Categories
Fixed set (nova2 `Category` enum) with display names (`categoryFullName`):

| id | display name |
|---|---|
| `all` | All categories |
| `anime` | Anime |
| `books` | Books |
| `games` | Games |
| `movies` | Movies |
| `music` | Music |
| `pictures` | Pictures |
| `software` | Software |
| `tv` | TV shows |

- `supportedCategories()` = union over **enabled** plugins.
- `getPluginCategories(selector)`: selector `"all"` → all plugins; `"enabled"` or `"multi"` → enabled plugins; else the single plugin id. Returns the union of their categories.
- `pluginNameBySiteURL(url)` → id of the plugin whose `url` equals the result's `engine_url` exactly (used to fill the "Engine" column and to choose the plugin for `nova2dl.py`).

### 5.5 Proxy environment for Python processes (`applyProxySettings`)
- Base = the system environment. Recomputed on proxy-config change and on any preference change.
- Only applied when **"Use proxy for general purposes"** (`Network/Proxy/Profiles/Misc`) is on; otherwise `http_proxy`, `https_proxy`, `qbt_socks_proxy` are removed.
- Proxy type mapping:
  - None → remove all three.
  - HTTP → `http_proxy` = `https_proxy` = `http://[user:pass@]ip:port`; remove `qbt_socks_proxy`.
  - SOCKS5 → `qbt_socks_proxy` = `socks5h://` (if "Perform hostname lookup via proxy") else `socks5://`, plus `[user:pass@]ip:port`; remove HTTP vars.
  - SOCKS4 → `qbt_socks_proxy` = `socks4a://ip:port` (hostname lookup) else `socks4://ip:port` (no credentials); remove HTTP vars.
- In Python, `helpers.enable_socks_proxy(True)` (called at import by `nova2.py`/`nova2dl.py`) parses `qbt_socks_proxy`: schemes `socks4`/`socks4a` → `PROXY_TYPE_SOCKS4`, `socks5`/`socks5h` → `PROXY_TYPE_SOCKS5` (with username/password), remote DNS for `socks4a`/`socks5h`; monkey-patches `socket.socket = socks.socksocket`. Legacy fallback: env `sock_proxy` (`host:port` or `user:pass@host:port`) treated as `socks5h://…` (marked for removal in qBt ≥ 5.3). HTTP proxies are honoured by `urllib` from `http_proxy`/`https_proxy`.

### 5.6 Running a search (`SearchHandler`)

- Command line (program = detected Python):
  ```
  <python> -I -Xutf8=1 <Data>/nova3/nova2.py <plugin1,plugin2,...> <category> <word1> <word2> ...
  ```
  - plugins: `usedPlugins.join(",")` — list resolved by the GUI selector (§5.8): "Only enabled"/"Select..." → enabled ids, "All plugins" → all ids, or a single id.
  - category: category id (`all`, `movies`, …).
  - pattern: the user's trimmed query **split on single spaces** into separate argv entries (quotes are passed through literally; `nova2.py` re-joins with spaces).
  - Env = proxy environment (§5.5). Unix: inherited file descriptors closed.
- Process start is deferred (queued) so listeners can attach. A **single-shot 3‑minute timer** starts; on timeout → `cancelSearch()`.
- `cancelSearch()`: if running and not already cancelled → Windows `kill()`, others `terminate()` (SIGTERM); mark cancelled; stop timer.
- Process error (`errorOccurred`) when not cancelled → WARNING `Search process failed. Search query: "%1". Category: "%2". Engines: "%3". Error: "%4".` and `searchFailed(errMsg)` where errMsg ∈ "Process failed to start" / "Process crashed" / "Process timed out" / "Process write error" / "Process read error" / "Process unknown error".
- On finish: read all stderr; if non-empty → WARNING `Error occurred in search engine. Search query: "%1". Category: "%2". Engines: "%3". Error: "%4".`. Then: cancelled → `searchFinished(cancelled=true)`; normal exit with code 0 → `searchFinished(false)`; else `searchFailed(stderr)`.
- **Streaming output parsing** (`readSearchOutput`): buffer = leftover + new stdout; split on `\n` keeping empty parts; the last (possibly partial) element is kept as leftover (trimmed); every complete line is parsed; parsed results are appended to `m_results` and emitted in batches (`newSearchResults`). The leftover is never flushed at exit (nova always terminates lines).
- **Result line format** (UTF-8, `|`-separated, fields trimmed):

  | idx | field | parse rule |
  |---|---|---|
  | 0 | `link` (download URL or magnet) | string → `fileUrl` |
  | 1 | `name` | string → `fileName` (plugin side replaces `|` with space) |
  | 2 | `size` (bytes) | `toLongLong()`; parse failure ⇒ 0; `-1` = unknown |
  | 3 | `seeds` | int64; failure or negative ⇒ **-1** |
  | 4 | `leech` | int64; failure or negative ⇒ **-1** |
  | 5 | `engine_url` | string → `siteUrl`; `engineName = pluginNameBySiteURL(siteUrl)` |
  | 6 | `desc_link` (optional) | string → `descrLink` |
  | 7 | `pub_date` (optional, Unix seconds) | int64; only if parse OK and `> 0` → `pubDate` (local QDateTime); else invalid/empty |

  Lines with ≤ 5 fields (i.e. fewer than 6) are dropped. Extra fields beyond 8 ignored.
- `SearchResult` struct: `fileName`, `fileUrl`, `fileSize` (qlonglong, default 0), `nbSeeders`, `nbLeechers` (qlonglong), `engineName`, `siteUrl`, `descrLink`, `pubDate` (QDateTime).

### 5.7 nova3 Python side (behaviour a rewrite must preserve or replace)

#### 5.7.1 `nova2.py` (VERSION 1.53)
- Appends its own directory to `sys.path` (isolated mode `-I` excludes it), imports `helpers`, calls `helpers.enable_socks_proxy(True)`.
- `THREADED = True`; `MAX_THREADS = multiprocessing.cpu_count()` (fallback 1).
- `Category = Enum('Category', ['all','anime','books','games','movies','music','pictures','software','tv'])`.
- `list_engines()`: glob `engines/*.py`, module name = basename up to first `.`, stripped; skip empty and names starting `_`; sorted.
- `import_engine(name)`: `importlib.import_module("engines.<name>")`, `getattr(module, name)` (class must be named exactly like the module); any exception ⇒ `None` (broken engines silently ignored); memoized.
- `get_capabilities(engines)`: XML as in §5.3, `ET.indent`, categories = sorted keys of `supported_categories` excluding `"all"`; engines without the attribute get empty categories.
- CLI:
  - `--capabilities` anywhere in argv → print capabilities XML, exit 0. With `--names` too → print comma-joined names of importable engines.
  - fewer than 3 args → usage to stderr: `Usage: <prog> all|engine1[,engine2]* <category> <keywords>` / `To list available engines: <prog> --capabilities [--names]` / `Found engines: …`; exit **2**.
  - engines: `argv[1]` split on `,`, stripped, lower-cased, de-duplicated; `all` ⇒ every found engine; otherwise only found ones.
  - category: `argv[2].lower()` must be a `Category` name else stderr `Invalid category: <cat>`, exit 2.
  - query: `urllib.parse.quote(' '.join(argv[3:]))` (spaces become `%20`).
  - Runs `run_search((engine_class, what, category))` per engine in a `multiprocessing` **spawn** pool of `max(min(len(engines), MAX_THREADS), 1)` processes. `run_search`: instantiate; if engine has `supported_categories` → call `search(what, cat.name)` **only if** `cat.name` is a key (engines lacking that category are silently skipped); if attribute absent → `search(what)`. Exceptions → traceback to stderr, returns False.
  - Exit code 0 if all engines succeeded, else **1** (qBittorrent then marks the tab "An error occurred during search..." even if some results arrived).
- Plugin contract (for plugin authors; must remain compatible): module `engines/<id>.py` with `# VERSION: x.y` header and class `<id>` having class attributes `url: str`, `name: str`, `supported_categories: dict[str,str]` (category id → site-specific value; should include `all`), method `search(self, what: str, cat: str = 'all')` that calls `novaprinter.prettyPrinter(dict)`; optional `download_torrent(self, url)` that prints `"<local file path> <url>"`.

#### 5.7.2 `novaprinter.py` (VERSION 1.53)
- `prettyPrinter(d)` prints one line to fd 1 as UTF-8:
  `link|name.replace("|"," ")|anySizeToBytes(size)|seeds|leech|engine_url|desc_link (default "")|pub_date (default -1)`.
- `anySizeToBytes(x)`: int → as is; float → `round(x)`; str → regex `^(?P<size>\d*\.?\d+) *(?P<unit>[a-z]+)?` (IGNORECASE) on stripped string; no match → **-1**; unit's first letter upper-cased: `T`→2^40, `G`→2^30, `M`→2^20, `K`→2^10, anything else ×1; result `round(size)`.

#### 5.7.3 `nova2dl.py` (VERSION 1.26) — downloading a result's `.torrent`
- argv: `nova2dl.py <engine_name> <download_param>` (both stripped). <3 args → `SystemExit("Usage: <prog> engine_name download_parameter")`.
- Imports `engines.<engine_name>`, instantiates class; failure → prints `repr(e)` to stderr and exits with message `` <prog>: `engine_name` was not recognized: <name> ``.
- If engine has `download_torrent` → call it (it prints the result); else `print(helpers.download_file(param))`. Exit 0.
- Output contract: single stdout line `"<temp file path> <url>"`.
- qBittorrent side (`SearchDownloadHandler`): runs `<python> -I -Xutf8=1 <Data>/nova3/nova2dl.py <pluginName> <url>` with proxy env; on finish logs stderr (WARNING `Error occurred when downloading torrent via search engine. Engine: "%1". URL: "%2". Error: "%3".`); if exit code 0 and normal exit: stdout trimmed, split on `' '`; **exactly 2 parts** ⇒ path = part[0]; emits `downloadFinished(path, errMsg)` (path empty on failure). No timeout. (Note: a temp path containing spaces would break this.)

#### 5.7.4 `helpers.py` (VERSION 1.55)
- User-Agent spoof: `Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:{v}.0) Gecko/20100101 Firefox/{v}.0` with `v = 125 + (today − 2024‑04‑16).days // 30`.
- `retrieve_url(url, custom_headers={}, request_data=None, ssl_context=None, unescape_html_entities=True) -> str`: GET/POST via urllib with UA + custom headers; `URLError` → stderr `Connection error: <reason>` and return `""`; gunzip if body starts with `\x1f\x8b`; charset from `Content-Type` `charset=` else utf-8, decode with `'replace'`; optional `html.unescape`.
- `download_file(url, referer=None, ssl_context=None) -> str`: GET with UA (+`referer`), gunzip if needed, write to `tempfile.mkstemp()`, return `"<path> <url>"`.
- `htmlentitydecode = html.unescape` (back-compat alias).

### 5.8 Search UI (`SearchWidget`, `searchwidget.ui`)

Layout (top row): query line edit `lineEditSearchPattern` | category combo `comboCategory` | plugin combo `selectPlugin` | **"Search"** button / **"Stop"** button (mutually exclusive). Body: stacked pages — empty page or tab widget of result tabs. Bottom-right: **"Search plugins..."** button.

- **Empty page** (shown when `allPlugins()` is empty; query, combos and Search button disabled): "There aren't any search plugins installed.\nClick the "Search plugins..." button at the bottom right of the window to install some."
- **Query tooltip** (HTML): "A phrase to search for." / "Spaces in a search term may be protected by double quotes." / "Example:" / "**foo bar**: search for **foo** and **bar**" / "**"foo bar"**: search for **foo bar**".
- **Category combo**: item 0 "All categories" (data `all`), separator, then categories of the currently selected plugin selector (`getPluginCategories(selectorData)`), display names sorted by locale-aware compare. Rebuilt whenever the plugin selector changes or any plugin is installed/uninstalled/updated/enabled.
- **Plugin combo**: "Only enabled" (`enabled`), "All plugins" (`all`), "Select..." (`multi`), separator, then each **enabled** plugin by full name (data = id), locale-sorted. Choosing "Select..." immediately opens the Search plugins dialog; searching with `multi` uses the enabled set.
- **Search button / Enter in query box**:
  - Empty (trimmed) pattern → critical box "Empty search pattern" / "Please type a search pattern first".
  - Python invalid → desktop notification title "Search Engine", text "Please install Python to use the Search Engine."
  - Otherwise starts `SearchHandler(pattern, category, plugins)`, creates a new tab (ID = decimal `qHash(currentDateTimeUtc)` unique among tabs), makes it current, swaps Search→Stop, adds pattern to history, saves session.
- **Stop button**: `cancelSearch()` on the current tab. Button state: Stop shown only if the query text equals the current tab's pattern (`!m_isNewQueryString`) and that tab is `Ongoing`; editing the query text to something different shows Search again (allows new search while one runs).
- **Tabs**: closable; tab text = search pattern with `&` doubled; tab icon by status (Ongoing `queued`, Finished `task-complete`, Aborted `task-reject`, Error `error`, NoResults `dialog-warning`); tab tooltip = status text. Switching tabs puts that tab's pattern into the query box unless the user is typing a new query. **Double-click a tab** → copies its pattern into the query box. **Middle-click** closes tab. **Right-click tab** menu: "Refresh tab" (if not ongoing; re-runs the tab's pattern with the *currently selected* category/plugins in the same tab, clearing results) or "Stop search" (if ongoing), separator, "Close tab", "Close all tabs". Tab reorder/move saves session.
- **Shortcuts**: Ctrl+F (`QKeySequence::Find`) and Ctrl+E toggle focus between query box and current tab's result filter (select-all); Ctrl+W (`QKeySequence::Close`) closes current tab.
- **History** (completer on query box, case-insensitive, natural-sorted popup): setting `Search/HistoryLength` (int, default **50**, clamped 0–99; 0 disables and deletes file). New unique patterns appended; oldest removed beyond limit. Persisted to `<Data>/SearchUI/History.txt` (UTF‑8, `\n`-joined; max 10 MiB on read). Options ▸ Search page label "History length".
- **Session persistence** (Options ▸ Search ▸ "Search UI" group):
  - "Store opened tabs" → `Search/StoreOpenedSearchTabs` (bool, default false). File `<Data>/SearchUI/Session.json`: `{"Tabs":[{"ID":"<id>","SearchPattern":"<pattern>"},…],"CurrentTab":"<id>"}` (compact JSON; validation: unique IDs, string types). Turning it off deletes the file.
  - "Also store search results" → `Search/StoreOpenedSearchTabResults` (bool, default false). Per tab `<Data>/SearchUI/<ID>.json` = JSON array of `{"FileName","FileURL","FileSize","SeedersCount","LeechersCount","EngineName","SiteURL","DescrLink","PubDate"}` (`PubDate` = Unix seconds; sizes/counts numbers). Written when a search stops (any non-Ongoing status) and when the option is toggled on; deleted on tab close or when toggled off. Restored tabs show results with status Ready (no process).
  - All file I/O happens on a dedicated thread; errors logged: "Failed to load Search UI saved state data. File: "%1". Error: "%2"", "Failed to load saved search results. Tab: "%1". File: "%2". Error: "%3"", "Failed to save Search UI state. File: "%1". Error: "%2"", "Failed to save search results. Tab: "%1". File: "%2". Error: "%3"", "Failed to load Search UI history. File: "%1". Error: "%2"", "Failed to save search history. File: "%1". Error: "%2"", "Invalid data format.".

#### 5.8.1 Result tab (`SearchJobWidget`, `searchjobwidget.ui`)
- **Status** enum: `Ready`, `Ongoing`, `Finished`, `Error`, `Aborted`, `NoResults`. Status text (tab tooltip / statusTip): Ongoing "Searching...", Finished "Search has finished", Aborted "Search aborted", Error "An error occurred during search...", NoResults "Search returned no results", Ready "" . Transition on finish: cancelled → Aborted; else zero total results → NoResults; else Finished. Failure → Error.
- **Filter row** (left to right): free-text filter line edit (placeholder "Filter search results..."), results label `Results (showing <i>%1</i> out of <i>%2</i>):` (visible/total), "Search in:" combo, "Seeds:" min spin "to" max spin, "Size:" min spin + unit "to" max spin + unit.
  - **Free-text filter**: case-insensitive match on the Name column; wildcard by default (`wildcardToRegularExpression`, unanchored, non-path), raw regex if the context-menu toggle "Use regular expressions" is on (setting `SearchTab/UseRegexAsFilteringPattern`, bool, default false; the line edit's standard context menu + separator + this checkable action).
  - **"Search in:"** (tooltip explains): "Torrent names only" (`OnlyNames` = 1) / "Everywhere" (`Everywhere` = 0); setting `Search/FilteringMode` (enum name string, default `OnlyNames`). OnlyNames: if the tab's search pattern is longer than 2 chars and wrapped in `"…"`, the inner phrase must be a case-insensitive substring of the name; otherwise **every** space-separated word of the pattern must be a case-insensitive substring of the name.
  - **Seeds**: two `QSpinBox` 0–1000 (tooltips "Minimum number of seeds" / "Maximum number of seeds"); max shows "∞" at 0. Row rejected if `min>0 && seeds<min` or `max>0 && seeds>max`. (Tooltip on label: "Set minimum and maximum allowed number of seeders".)
  - **Size**: two `QDoubleSpinBox` 0–1000 (tooltips "Minimum torrent size" / "Maximum torrent size"; max shows "∞" at 0) + unit combos `B, KiB, MiB, GiB, TiB, PiB, EiB` (1024-based). Defaults: min 0 MiB, max ∞ GiB. Row rejected if `min>0 && size<min` or `max>0 && size>max`. (Label tooltip "Set minimum and maximum allowed size of a torrent".)
  - Filters apply live (valueChanged + editingFinished) and update the count label.
- **Columns** (`SearchSortModel::SearchColumn`, in order): `NAME` "Name", `SIZE` "Size" (right-aligned, `friendlyUnit` e.g. "1.5 GiB"), `SEEDS` "Seeders" (right-aligned, integer, `-1` shown literally when unknown), `LEECHES` "Leechers" (same), `ENGINE_NAME` "Engine" (plugin id), `ENGINE_URL` "Engine URL", `PUB_DATE` "Published On" (locale short date-time in local time; empty if no date), hidden `DL_LINK`, hidden `DESC_LINK`.
  - Sorting: Name and Engine URL use case-insensitive **natural** compare on display text; others compare underlying values (bytes, ints, datetime).
  - Extended selection, no root decoration, no editing, first section movable, columns user-resizable.
  - Header right-click → menu titled "Column visibility": checkable entry per visible-able column (Name…Published On); unchecking the last visible column is refused; checking a column of width ≤5 auto-resizes; separator; "Resize columns" (tooltip "Resize all non-hidden columns to the size of their contents"). Header state shared by all tabs: `GUI/Qt6/SearchTab/HeaderState`.
- **Row context menu**: "Open download window" (icon `download`; add with dialog forced), "Download" (icon `downloading`; add skipping dialog), separator, "Open description page" (icon `application-url`), "Copy" ▸ { "Name", "Download link", "Description page URL" } (copies selected rows' non-empty values joined with `\n`).
- **Double-click row** or **Enter/Return** → download selected rows with the default option (honours the "Show Add Torrent dialog" preference).
- **Download procedure** per row: if `fileUrl` starts with `magnet:` (case-insensitive) → `AddTorrentManager::addTorrent(url, {}, option)`; else run `nova2dl.py <engineName> <url>` and add the returned local path (empty path on failure → add fails). Row is then marked **visited**: foreground = palette Disabled WindowText (greyed) for all cells, re-applied on theme change; visited state is in-memory only.
- **Open description page**: for each selected row, empty `descrLink` → counted; `file://` (local file) URL → **blocked**, WARNING `Blocked opening search result description page URL. URL pointing to local file might be malicious behaviour. Name: "%1". URL: "%2".`; else open in default browser. Afterwards: any blocked → warning "Blocked opening search result description page URL. The following result URL is pointing to local file and it may be malicious behaviour:\n%1" (+ "\nThere are %1 more results with the same issue."); else any empty → warning "Entry "%1" has no description page URL provided." (+ "\nThere are %1 more entries with the same issue."). Message box title "qBittorrent".

#### 5.8.2 Search plugins dialog (`PluginSelectDialog`, title "Search plugins")
- Label "Installed search plugins:"; tree columns **Name** (full name + icon), **Version**, **Url**, **Enabled** ("Yes" green row / "No" red row), hidden id column. Sorted by Name ascending by default; first section movable; dialog size stored `SearchPluginSelectDialog/Size`.
- Warning label: "Warning: Be sure to comply with your country's copyright laws when downloading torrents from any of these search engines."
- Link label: "You can get new search engine plugins here: <a href="https://plugins.qbittorrent.org">https://plugins.qbittorrent.org</a>".
- Buttons: **"Install a new one"**, **"Check for updates"**, **"Close"**.
- Double-click a row toggles enabled. Right-click (with selection) → checkable "Enabled" (state of first selected; applies to all selected), separator, "Uninstall" (icon `list-remove`).
- Icons: if `iconPath` missing, downloads `<plugin url>/favicon.ico` (general-purpose proxy); validates it decodes as an image; saves as `engines/<id>.ico` (or `.png` if the URL doesn't end with `.ico`); re-validates after copy (deletes if undecodable); updates `iconPath`.
- **Install a new one** → "Plugin source" dialog (`PluginSourceDialog`, label "Search plugin source:", buttons **"Local file"** / **"Web link"**; size key `SearchPluginSourceDialog/Size`):
  - Local file → multi-select file dialog "Select search plugins", start dir home, filter "qBittorrent search plugin (*.py)"; each → `installPlugin(path)`.
  - Web link → text input "New search engine plugin URL" / "URL:"; default = clipboard text if it has a supported scheme and ends with `.py`, else `http://`; loop: if non-empty and not ending `.py` → warning "Invalid link" / "The link doesn't seem to point to a search engine plugin." and re-prompt.
- **Drag & drop** onto the dialog: accepts `text/plain` or `text/uri-list`; each URL (file URLs → local path) or each text line → `installPlugin`.
- Async-op counter shows a wait cursor while installs/updates run.
- Result messages: check-for-updates with nothing to do → "Search plugin update" / "All your plugins are already up to date."; failure → "Search plugin update" / "Sorry, couldn't check for plugin updates. %1"; after all pending installs/updates finish → "Search plugin update" / "Plugins installed or updated: %1" (case-insensitively sorted, comma-joined); install failure → "Search plugin install" / "Couldn't install "%1" search engine plugin. %2"; update failure → "Search plugin update" / "Couldn't update "%1" search engine plugin. %2".

---

### 5.9 RSS — data model & storage (`RSS::Session`, `Item`, `Folder`, `Feed`, `Article`)

#### 5.9.1 Session settings (all in main settings file)

| Key | Type | Default | Options-dialog label (RSS page) | Semantics |
|---|---|---|---|---|
| `RSS/Session/EnableProcessing` | bool | **false** | "Enable fetching RSS feeds" (group "RSS Reader") | Master switch for automatic feed refreshing (manual refresh via UI still works). Enabling triggers immediate refresh; disabling stops the timer. |
| `RSS/Session/RefreshInterval` | int (minutes) | **30** | "Feeds refresh interval:" (suffix " min", 1–999999) | Global refresh period for feeds whose own interval is 0. Changing it restarts the timer with the new full period. |
| `RSS/Session/FetchDelay` | int64 (seconds) | **2** | "Same host request delay:" (suffix " sec", 0–2147483646) | Minimum pause between consecutive HTTP requests to the same host:port (see 5.9.6). |
| `RSS/Session/MaxArticlesPerFeed` | int | **50** | "Maximum number of articles per feed:" (0–2147483646) | Cap per feed; excess oldest articles are dropped immediately when lowered. |
| `RSS/AutoDownloader/EnableProcessing` | bool | **false** | "Enable auto downloading of RSS torrents" (group "RSS Torrent Auto Downloader", button "Edit auto downloading rules...") | Master switch for the rule engine. |
| `RSS/AutoDownloader/DownloadRepacks` | bool | **true** | "Download REPACK/PROPER episodes" (group "RSS Smart Episode Filter") | See smart filter. |
| `RSS/AutoDownloader/SmartEpisodeFilter` | QStringList | null ⇒ 4 built-in patterns (5.10.4) | "Filters:" (plain-text edit, one regex per line, empty lines skipped) | Smart-episode regex alternatives. |
| `Network/Proxy/Profiles/RSS` | bool | false | Connection page "Use proxy for RSS purposes" (tooltip "RSS feeds will use proxy") | Feed + favicon downloads go through the proxy. |
| `GUI/RSSWidget/Enabled` | bool | false | View ▸ RSS Reader | Show the RSS tab. |
| `GUI/RSSWidget/OpenedFolders` | QStringList | [] | — | Paths of expanded folders in the feed tree. |
| `GUI/Qt6/RSSWidget/SideSplitterState`, `GUI/Qt6/RSSWidget/MainSplitterState` | bytes | — | — | Splitter geometry. |

Legacy keys deleted at startup: `Rss/streamList`, `Rss/streamAlias`, `Rss/open_folders`, `Rss/qt5/splitter_h`, `Rss/qt5/splitterMain`, `Rss/hosts_cookies`, the same six under `RSS/…`, `Rss/Session/EnableProcessing`, `Rss/Session/RefreshInterval`, `Rss/Session/MaxArticlesPerFeed`, `Rss/AutoDownloader/EnableProcessing`.

#### 5.9.2 Item tree & paths
- Items: `Folder` (root folder has path `""`) and `Feed`. Path separator is **backslash** `\` (`Item::PathSeparator`). Item name = last path segment. Valid path regex: `\A[^\\]+(\\[^\\]+)*\z` (non-empty segments, no leading/trailing/double separators).
- Session operations (all return error strings shown verbatim in the UI):
  - `addFolder(path)`; `addFeed(url, path, refreshInterval=0s)` — errors: "RSS feed with given URL already exists: %1.", "Incorrect RSS Item path: %1.", "RSS item with given path already exists: %1.", "Parent folder doesn't exist: %1.". Adding a feed stores and, if processing enabled, triggers `refresh()`.
  - `setFeedURL(feed, url)` — duplicate URL error as above; "Feed doesn't exist: %1."; stores; if processing enabled, refreshes that feed immediately.
  - `moveItem(item, destPath)` (also used for rename) — "Cannot move root folder.", "Item doesn't exist: %1.", "Can't move a folder into itself or its subfolders.".
  - `removeItem(path)` — "Cannot delete root folder.", "Item doesn't exist: %1."; deletes the feed's article file and icon (`cleanup()`).
- Feed URL uniqueness enforced; feed UID (`QUuid`) unique.
- **Auto naming**: a new feed is created with name = its URL. After the first parse yields a non-empty title, if the name still equals the URL the feed is renamed (moved) to the title. If the URL is changed while name == old URL, the feed is renamed to the new URL.

#### 5.9.3 `feeds.json` (Config/rss/feeds.json)
- Written (async, pretty JSON) on every structural change. Max 10 MiB on read. Missing file → import legacy `Rss/streamList` + `Rss/streamAlias` then write new format ("Corrupted RSS list, not loading it." if list sizes differ).
- Format: nested object = root folder; key = item name; value = folder object (recursively) **or** feed object (recognized by having a `url` key):
  ```json
  {
    "TV": {
      "ShowRSS": { "uid": "{6b8c…-…}", "url": "https://showrss.info/user/1.rss", "refreshInterval": 900 }
    },
    "Linux ISOs": { "uid": "{…}", "url": "https://example.org/feed.xml" }
  }
  ```
  - `uid` = `QUuid::toString()` (braced). `refreshInterval` = seconds, **written only if > 0** (0 ⇒ use global).
  - Legacy reduced form accepted on load: `"name": "url"` string (empty string ⇒ url = key); missing `uid` ⇒ generated; both trigger a re-save.
  - Load errors (logged, item skipped): "Couldn't load RSS feed. Feed: "%1". Reason: URL is required.", "…Reason: UID is invalid.", "Duplicate RSS feed found. UID: "%1". Error: Configuration seems to be corrupted.", "Couldn't load RSS item. Item: "%1". Invalid data format.", "Failed to read RSS session data. %1", "Failed to parse RSS session data. File: "%1". Error: "%2"", "Failed to load RSS session data. File: "%1". Error: "Invalid data format."". Save error: "Couldn't save RSS session configuration. File: "%1". Error: "%2"".

#### 5.9.4 Article storage (Data/rss/articles/)
- Per feed: `<Data>/rss/articles/<uidHex>.json` where `uidHex` = hex of the 16 RFC‑4122 bytes (32 lowercase hex chars, no braces/dashes). Feed favicon: `<Data>/rss/articles/<uidHex>.ico`. Legacy file name `toValidFileName(url, "_") + ".json"` is renamed on first load (pre‑4.1.2 migration).
- Content: compact JSON **array** of article objects; each object = the article's full data hash (all known keys plus any extra RSS/Atom child elements captured by the parser), with `date` serialized as **RFC 2822** string (`Qt::RFC2822Date`). On load: date parsed back from RFC 2822; sorted newest first; truncated to `maxArticlesPerFeed`; duplicate IDs skipped.
- Writes: `store()` when dirty — after each parse, on mark-all-read, on feed destruction; single-article read changes are saved **deferred 5 s**. Errors: "Failed to save RSS feed in '%1', Reason: %2", "Couldn't parse RSS Session data. Error: %1", "Couldn't load RSS Session data. Invalid data format.", "Couldn't load RSS article '%1#%2'. Invalid data format.".
- Article data keys (`Article::Key*`):

  | Constant | JSON key | Meaning |
  |---|---|---|
  | `KeyId` | `id` | unique id within feed (guid / Atom id / fallback) |
  | `KeyDate` | `date` | publication date (QDateTime; RFC 2822 string on disk) |
  | `KeyTitle` | `title` | title (trimmed) |
  | `KeyAuthor` | `author` | author |
  | `KeyDescription` | `description` | HTML/text body |
  | `KeyTorrentURL` | `torrentURL` | torrent/magnet URL |
  | `KeyLink` | `link` | news/web page URL |
  | `KeyIsRead` | `isRead` | bool, absent ⇒ false |

- `Article::torrentUrl()` returns `torrentURL` if non-empty else `link`.
- `Article::isSupportedTorrentURL(url)`: starts with `http:`/`https:`/`magnet:` (case-insensitive) OR parses as a bare info-hash/torrent descriptor.

#### 5.9.5 Refresh scheduling
- Each feed has a `nextRefresh` time point (initially epoch). `Session::refresh()`: for each feed whose time point ≤ now+1 s → `feed->refresh()` and set next = now + effective interval (feed interval if > 0 s else global minutes); single-shot timer set to the smallest remaining interval.
- At startup: if processing enabled → `refresh()` immediately (all feeds due). New feed added while enabled → `refresh()`.
- Per-feed interval change: stored; pending next time point shifted by (new − old); if processing enabled and new interval < old effective interval → `refresh()`.
- `Feed::refresh()`: if the article file hasn't finished loading yet → mark pending, refresh after load. Cancels any in-flight download, starts `DownloadManager::download(url, useProxyForRSS)`, downloads the favicon if `<uidHex>.ico` doesn't exist, sets `isLoading=true`.
- `Folder::refresh()` refreshes all children recursively; "Update all" = root folder refresh (ignores schedule, does not reset time points).
- Download failure → `isLoading=false`, `hasError=true`, WARNING `Failed to download RSS feed at '%1'. Reason: %2`. Success → INFO `RSS feed at '%1' is successfully downloaded. Starting to parse it.` and parse on the RSS worker thread.

#### 5.9.6 Same-host request delay
- Each feed registers its `ServiceID {host, port (default 80)}` as a *sequential service* with delay = `FetchDelay` (re-registered for all feeds when the setting changes). For a sequential service, `DownloadManager` runs one request at a time; queued requests to the same service start `delay` seconds after the previous reply finishes. (Favicons and other downloads to that host share the queue.)

#### 5.9.7 Favicon
- URL = `<scheme>://<host>/favicon.ico` derived from the feed URL (path discarded); saved to `<uidHex>.ico`; downloaded at feed construction if processing enabled (else upon first enabling), and on refresh if missing. Emits `iconLoaded`.

#### 5.9.8 Feed state flags exposed to UI/WebAPI
- `isLoading` = downloading/parsing OR article file not yet loaded; `hasError` = last download failed or parser reported an error; `title`; `lastBuildDate`; `unreadCount`; `refreshInterval`; `uid`; `url`. `Feed::toJsonValue(withData)` (used by WebAPI `rss/items`) emits `uid`, `url`, optional `refreshInterval`, and with data: `title`, `lastBuildDate`, `isLoading`, `hasError`, `articles` (each article's data with `date` as RFC 2822).

### 5.10 RSS — feed parsing (`RSS::Private::Parser`, `rss_parser.cpp`)

- Runs on the RSS worker thread with `QXmlStreamReader`; a custom entity resolver maps the full **XHTML 1.0 named entity set** (lat1 + symbol + special: `nbsp`, `copy`, `eacute`, `mdash`, `hearts`, … ≈250 names) to numeric character references so undeclared HTML entities don't abort parsing.
- `m_fallbackDate = now` per parse.
- Root detection: skip elements until `<rss>` (then find first `<channel>` child) or `<feed>` (Atom). Error string if XML error: `"%1 (line: %2, column: %3, offset: %4)."` (errorString, line, column, char offset); if no channel found: "Invalid RSS feed.". Articles parsed before an XML error are still kept.
- **"Unchanged" short-circuit**: the parser instance remembers the last seen RSS `<lastBuildDate>` / Atom feed-level `<updated>` text (in memory only, initially empty each app start). If the new value equals the remembered one, parsing of the channel stops immediately (no further items processed).
- Element names are compared by **local name** (namespace prefixes ignored).

#### 5.10.1 RSS 2.0 (`<channel>`)
- `title` → feed title (raw text); `lastBuildDate` → see short-circuit; each `item` → article:
  - `title` → `title` (trimmed)
  - `enclosure`: if `type="application/x-bittorrent"` → `torrentURL = @url`; else if `type` empty/missing → remember `@url` as alternate torrent URL; other types ignored.
  - `link`: text trimmed; if it starts with `magnet:` (ci) → `torrentURL`, else → `link`.
  - `description` → `description` (raw text **including child elements' text**, not trimmed)
  - `pubDate` → `date` via RFC 822 parser (5.10.3); unparseable/empty → fallback date.
  - `author` → `author` (trimmed); `guid` → `id` (trimmed).
  - any other child element `<x>` → stored as `article["x"]` = element text incl. children (e.g. `category`, `comments`, `creator`, `infoHash`, `magnetURI`, `size`, `seeds`, `contentLength`…).
  - After the item: if `torrentURL` empty → alternate enclosure URL.
#### 5.10.2 Atom (`<feed>`)
- `xml:base` attribute of `<feed>` remembered as base URL. `title` → feed title; feed-level `updated` → short-circuit key; each `entry` → article:
  - `title` → `title` (trimmed)
  - `link`: value = `@href` if the element has any attributes, else element text (trimmed); **no `rel` filtering, last `<link>` wins**; magnet → `torrentURL`; else `link` = `base + href` (plain string concatenation when base non-empty).
  - `summary` or `content` → `description` (text incl. children, trimmed); only the first non-empty one is kept; later ones skipped.
  - `updated` → `date` via ISO 8601 (`Qt::ISODate`); invalid → fallback date.
  - `author` → reads child `name` → `author`.
  - `id` → `id` (trimmed); others stored by element name.
#### 5.10.3 Date parsing for RSS `pubDate` (port of KDElibs RFC 822/2822 parser)
- Empty → fallback date. Primary regex:
  `^(?:([A-Z][a-z]+),\s*)?(\d{1,2})(\s+|-)([^-\s]+)(\s+|-)(\d{2,4})\s+(\d\d):(\d\d)(?::(\d\d))?\s+(\S+)$`
  (groups: weekday, day, sep, month, sep, year, hour, min, sec, zone). Both separators must be `-` or both whitespace, else fallback.
- Else obsolete asctime form: `^([A-Z][a-z]+)\s+(\S+)\s+(\d\d)\s+(\d\d):(\d\d):(\d\d)\s+(\d\d\d\d)$` (weekday, month, day, h, m, s, year).
- Month must be one of `Jan…Dec` (English short; unknown ⇒ month index 12 ⇒ invalid date ⇒ fallback). Weekday names (short or long) looked up but not validated.
- Year with < 4 digits: 2-digit < 50 ⇒ +2000, else +1900 (3-digit ⇒ +1900).
- Seconds 60 = leap second → 59 with validation (must be 23:59:60 UTC else fallback).
- Zone: `^([+-])(\d\d)(\d\d)$` numeric offset (minutes > 59 ⇒ **invalid QDateTime returned**, not fallback); single letter military zone (not `J`) ⇒ −0000; `UT`/`GMT` ⇒ 0; `EDT` −4h, `EST`/`CDT` −5h, `CST`/`MDT` −6h, `MST`/`PDT` −7h, `PST` −8h; other all-alpha zone ⇒ 0 (treated as −0000); zone containing non-letters ⇒ invalid QDateTime.
- Result is UTC (`QTimeZone::UTC`) adjusted by `−offset`; invalid ⇒ fallback.
#### 5.10.4 Article identity & insertion (`Parser::addArticle`, `Feed::updateArticles`)
- If `torrentURL` empty → `torrentURL = link`.
- `id` fallback chain: `guid`/`id` → `torrentURL` → `title`; if all empty, article dropped. Duplicate ids within one parse dropped. Articles are prepended (so the result list is reversed relative to document order).
- `Feed::handleParsingFinished`: set `hasError` if error; update title (emit `titleChanged`) and `lastBuildDate`; `updateArticles`; store; log WARNING `Failed to parse RSS feed at '%1'. Reason: %2` on error and INFO `RSS feed at '%1' updated. Added %2 new articles.`; `isLoading=false`.
- `updateArticles(list)`: `dummyPubDate = now`; iterate parsed articles in order: if an article with that id already exists → skip it and set `dummyPubDate = existing.date − 1 ms`; otherwise if its date is invalid assign `dummyPubDate` (prevents date-less articles from jumping ahead of known ones). Merge existing + new by date descending, truncate to `maxArticlesPerFeed`, then add surviving new ones oldest-first. Each added article: insertion-sorted by date (newest first); if its position ≥ max it is rejected; unread ones increment the unread count; emits `newArticle`; oldest removed if over capacity.
- Read state: `Article::markAsRead()` sets `isRead=true` in its data; feed unread count decremented; deferred save. `Feed::markAsRead()` marks all, saves immediately. `Folder::markAsRead()` recurses. Folder unread count = sum of children. Folder article list = merge of children sorted by date desc.

### 5.11 RSS auto-downloader (`RSS::AutoDownloader`, `RSS::AutoDownloadRule`)

#### 5.11.1 Rule fields (`AutoDownloadRuleData`) and `download_rules.json`
- File: `<Config>/rss/download_rules.json` — JSON object keyed by **rule name**; written immediately on add/rename/remove, deferred 5 s on modification or after a rule accepts an article; also on shutdown. Max 10 MiB on read. Missing file → import legacy rules from settings file `qBittorrent-rss` key `download_rules` (legacy dict format). Error strings: "Couldn't save RSS AutoDownloader data in %1. Error: %2" (CRITICAL), "Failed to read RSS AutoDownloader rules. %1", "Couldn't load RSS AutoDownloader rules. Reason: %1" (CRITICAL), "Invalid data format.".
- Rule JSON (all keys written by `toJsonObject`):

  | JSON key | Type | Default (`fromJsonObject`) | Meaning |
  |---|---|---|---|
  | (object key) | string | — | rule name (unique) |
  | `enabled` | bool | `true` | rule active |
  | `priority` | int | `0` | evaluation order, **ascending** (lower first) |
  | `useRegex` | bool | `false` | regex vs wildcard mode for must/mustNot |
  | `mustContain` | string | `""` | expressions (see matching) |
  | `mustNotContain` | string | `""` | expressions |
  | `episodeFilter` | string | `""` | e.g. `1x2;8-15;5;30-;` |
  | `affectedFeeds` | array<string> (a single string also accepted) | `[]` | feed **URLs** the rule applies to |
  | `lastMatch` | string RFC 2822 (empty if never) | invalid | date of the last accepted article (article's date, not wall clock) |
  | `ignoreDays` | int | `0` | "Ignore Subsequent Matches for N days" (0 = off) |
  | `smartFilter` | bool | `false` | use smart episode filter |
  | `previouslyMatchedEpisodes` | array<string> (single string accepted) | `[]` | episode keys already downloaded (e.g. `1x5`, `1x5-REPACK`, `2017.01.05`) |
  | `addPaused` | bool or null | null | DEPRECATED mirror of `torrentParams.stopped` |
  | `torrentContentLayout` | string or null | null | DEPRECATED mirror (`Original`/`Subfolder`/`NoSubfolder`) |
  | `savePath` | string | `""` | DEPRECATED mirror |
  | `assignedCategory` | string | `""` | DEPRECATED mirror |
  | `torrentParams` | object | — | full `AddTorrentParams` (below). If absent, the deprecated keys are used: `savePath`, `assignedCategory`, `addPaused`, `torrentContentLayout` (or legacy bool `createSubfolder`: true→`Original`, false→`NoSubfolder`); non-empty `savePath` forces `use_auto_tmm=false`. |

- `torrentParams` object (`serializeAddTorrentParams` / `parseAddTorrentParams`): always `category` (string), `tags` (array of strings), `save_path`, `download_path` (strings), `operating_mode` (`"AutoManaged"`|`"Forced"`), `skip_checking` (bool), `upload_limit` / `download_limit` (int B/s, default −1 = unlimited/none), `seeding_time_limit` / `inactive_seeding_time_limit` (int minutes, default −2 = use global), `share_limit_action` (`"Default"`(−1)/`"Stop"`(0)/`"Remove"`(1)/`"EnableSuperSeeding"`(2)/`"RemoveWithContent"`(3); default `Default`), `ratio_limit` (double, default −2 = use global; −1 = unlimited), `ssl_certificate`, `ssl_private_key`, `ssl_dh_params` (PEM strings); and only when set: `add_to_top_of_queue` (bool), `stopped` (bool), `stop_condition` (`"None"`/`"MetadataReceived"`/`"FilesChecked"`), `content_layout` (`"Original"`/`"Subfolder"`/`"NoSubfolder"`), `use_auto_tmm` (bool), `use_download_path` (bool). Unset optionals mean "use session default".
- **Export/import** (RSS Downloader dialog buttons "&Import..." / "&Export..."): 
  - JSON (`Rules (*.json)`): same object format as `download_rules.json` (pretty-printed). Import: each rule → `setRule` (overwrites same-named rules). Errors: "Import error" / "Failed to import the selected rules file. Reason: %1"; "Failed to read the file. %1".
  - Legacy (`Rules (legacy) (*.rssrules)`): `QDataStream` (version `Qt_4_5`) serialization of `QVariantHash{ ruleName → QVariantHash }` with keys `name`, `must_contain`, `must_not_contain`, `save_path`, `affected_feeds` (QStringList), `enabled` (bool, import default **false**), `category_assigned`, `use_regex`, `add_paused` (int: 0 default, 1 always/stopped, 2 never), `episode_filter`, `last_match` (QDateTime), `ignore_days`. Stream error → "Invalid data format".
  - Export with no rules → "Invalid action" / "The list is empty, there is nothing to export."; extension auto-appended; write failure → "I/O Error" / "Failed to create the destination file. Reason: %1". Dialog titles "Export RSS rules" / "Import RSS rules", start dir home.

#### 5.11.2 Matching algorithm (`AutoDownloadRule::matches(articleData)`) — applied to the article **title** only
1. **Ignore period**: if `ignoreDays > 0` and `lastMatch` valid and `articleDate < lastMatch + ignoreDays days` → reject.
2. **Must Contain** (`matchesMustContainExpression`): list empty → pass. Else pass if **any** expression matches.
3. **Must Not Contain**: list empty → pass. Else reject if **any** expression matches.
4. **Episode filter** (if non-empty).
5. **Smart episode filter** (if enabled).
- Expression lists: `setMustContain(text)` / `setMustNotContain(text)`: regex mode → the whole text is ONE expression; wildcard mode → `text.split('|')` (each alternative an expression). A single empty string ⇒ empty list. (Order: `useRegex` is applied before the lists when loading.) Stored back joined with `|`.
- `matchesExpression(title, expr)`:
  - empty expression → **true** (so `expr|` with an empty alternative matches everything in Must Contain and excludes everything in Must Not Contain).
  - regex mode → `QRegularExpression(expr, CaseInsensitive)` search (unanchored) in title.
  - wildcard mode → split `expr` on `\s+` (skip empty) — **every** token must match; each token converted by `QRegularExpression::wildcardToRegularExpression(token, UnanchoredWildcardConversion | NonPathWildcardConversion)` (`*` any chars, `?` one char, `[...]` sets), case-insensitive, unanchored ⇒ word order irrelevant.
  - Compiled regexes are cached per rule (cache cleared when useRegex/must/mustNot/episode filter change).
- **Episode filter** (`matchesEpisodeFilterExpression`): (clears `lastComputedEpisodes` first) filter must match `(^\d{1,4})x(.*;$)` (case-insensitive) else reject. `season` = group 1 string (as typed, e.g. `1` or `01`), `seasonOurs = toInt(season)`; group 2 split on `;`; for each non-empty token, strip leading zeros (keeping one digit):
  - token contains `-` (range): find in title with `\bs0?(\d{1,4})[ -_\.]?e(0?\d{1,4})(?:\D|\b)` else `\b(\d{1,4})x(0?\d{1,4})(?:\D|\b)` (both case-insensitive); if found, `seasonTheirs`, `episodeTheirs` = ints:
    - token ends with `-` (infinite, `N-`): accept if (`seasonTheirs == seasonOurs` and `episodeTheirs >= N`) or `seasonTheirs > seasonOurs`.
    - `A-B`: if `A > B` ignore token; accept if `seasonTheirs == seasonOurs` and `A <= episodeTheirs <= B`.
  - single number `E`: accept if title matches `\b(?:s0?<season>[ -_\.]?e0?<E>|<season>x0?<E>)(?:\D|\b)` (case-insensitive; `<season>` is the literal season string from the filter). Note `[ -_\.]` is a character **range** from space to `_` (bug-compatible).
  - No token accepted → reject.
  - UI validation regex (red text + warning icon if not matching, empty is valid): `^(^\d{1,4}x(\d{1,4}(-(\d{1,4})?)?;){1,}){1,1}` (case-insensitive). Tooltip: "Matches articles based on episode filter." / "Example: **1x2;8-15;5;30-;** will match 2, 5, 8 through 15, 30 and onward episodes of season one" / "Episode filter rules: " / "Season number is a mandatory non-zero value" / "Episode number is a mandatory positive value" / "Filter must end with semicolon" / "Three range types for episodes are supported: " / "Single number: **1x25;** matches episode 25 of season one" / "Normal range: **1x25-40;** matches episodes 25 through 40 of season one" / "Infinite range: **1x25-;** matches episodes 25 and upward of season one, and all episodes of later seasons".
- **Smart episode filter** (`matchesSmartEpisodeFilter`):
  - Global regex = `(?:_|\b)(?:<f1>|<f2>|…)(?:_|\b)` built from `RSS/AutoDownloader/SmartEpisodeFilter` lines, options **CaseInsensitive | ExtendedPatternSyntax (whitespace in pattern ignored) | UseUnicodeProperties**. Default filter lines:
    1. `s(\d+)e(\d+)` — Format 1: s01e01
    2. `(\d+)x(\d+)` — Format 2: 01x01
    3. `(\d{4}[.\-]\d{1,2}[.\-]\d{1,2})` — Format 3: 2017.01.01
    4. `(\d{1,2}[.\-]\d{1,2}[.\-]\d{4})` — Format 4: 01.01.2017
    i.e. default full regex `(?:_|\b)(?:s(\d+)e(\d+)|(\d+)x(\d+)|(\d{4}[.\-]\d{1,2}[.\-]\d{1,2})|(\d{1,2}[.\-]\d{1,2}[.\-]\d{4}))(?:_|\b)`.
  - `computeEpisodeName(title)`: first match; for each capture group 1..n that is non-empty: if it parses as int → decimal int (leading zeros dropped) else raw text; join with `x`. Examples: `S01E05` → `1x5`; `1x05` → `1x5`; `2017.01.05` → `2017.01.05`. No match → empty.
  - Empty episode name → **reject** (articles without recognizable episode are never downloaded when smart filter is on).
  - If episode name ∉ `previouslyMatchedEpisodes` → accept, remember `[ep]` in `lastComputedEpisodes`.
  - If already matched: if `DownloadRepacks` is false → reject. Else `isRepack` = title contains `REPACK` (ci), `isProper` = contains `PROPER` (ci); neither → reject. Key = `ep + (isRepack ? "-REPACK" : "") + (isProper ? "-PROPER" : "")`; if key already in list → reject; else accept and remember key; if both REPACK and PROPER, also remember `ep-REPACK` and `ep-PROPER`.
- **`accepts(articleData)`** (mutating, used by the processor; the UI preview uses side-effect-free `matches`): if `matches` → `lastMatch = article date`; append `lastComputedEpisodes` to `previouslyMatchedEpisodes`; return true.

#### 5.11.3 Processing pipeline
- Starts when `AutoDownloader/EnableProcessing` is true **and** the BitTorrent session has finished restoring (`Session::restored`). `startProcessing()`: `resetProcessingQueue()` then re-enqueue every article of the root folder via `handleNewArticle`, and subscribe to `Folder::newArticle` of the root (all feeds).
- `handleNewArticle(article)`: enqueue a job `{feedURL, articleData}` if article **unread** and `torrentUrl()` non-empty (jobs whose torrent URL is currently awaiting download are skipped).
- `resetProcessingQueue()` (on rule add/change and processing start): clear queue; if enabled, enqueue all unread articles with a torrent URL from all feeds.
- Queue is drained one job per event-loop turn (0 ms single-shot timer).
- `processJob(job)`: iterate rules in **priority order (ascending)**; skip disabled rules and rules whose `affectedFeeds` doesn't contain the job's feed URL; first rule whose `accepts()` returns true wins (later rules not tried):
  1. mark rules dirty, deferred save; INFO `RSS article '%1' is accepted by rule '%2'. Trying to add torrent...`
  2. `torrentURL = articleData["torrentURL"]`; if not `isSupportedTorrentURL` → WARNING `Failed to add torrent from RSS article. Reason: unsupported torrent URL. Only HTTP(S) URLs, magnet URIs and info hashes are supported. Article: "%1". URL: "%2"` and stop.
  3. `AddTorrentManager::addTorrent(torrentURL, rule.addTorrentParams())` (no dialog; params: category, tags, save/download paths, stopped, stop condition, content layout, auto-TMM, queue top, limits, share limits, skip checking…).
  4. If the URL is a magnet/info-hash (parses directly) → mark article read immediately. Otherwise remember job in `waitingJobs[torrentURL]`; when `AddTorrentManager` reports `torrentAdded(source)` → mark article read; on `addTorrentFailed` with `DuplicateTorrent` → mark read; other failures → article stays unread (no retry until next queue reset/restart).
- Disabling processing clears the queue and unsubscribes. Feed URL change → rules' `affectedFeeds` and pending jobs updated to the new URL, rules saved.

### 5.12 RSS UI

#### 5.12.1 RSS tab (`RSSWidget`, `rsswidget.ui`)
- Main-window tab title **"RSS (%1)"** with total unread count (updated live); toggled by View ▸ RSS Reader (`GUI/RSSWidget/Enabled`); shortcut **Alt+3**.
- Warning banner (hidden when processing enabled): "Fetching of RSS feeds is disabled now! You can enable it in application settings."
- Toolbar (left→right): **"New subscription"** (icon `list-add`), **"Mark items read"** (icon `task-complete`), **"Update all"** (tooltip "Refresh RSS streams", icon `view-refresh`), spacer, filter line edit (placeholder "Filter feed items...", max width 200 px), **"RSS Downloader..."** (icon `downloading`). (Icons not shown on macOS buttons.)
- Left pane: feed tree (`FeedListWidget`). Right pane: label "Torrents: (double-click to download)", article list (`ArticleListWidget`) above article preview (`HtmlBrowser`), vertical splitter; horizontal splitter between tree and right pane.
- **Feed tree**:
  - Header "RSS feeds"; single column; extended selection; internal drag & drop; sortable (sticky item always first).
  - Sticky top item **"Unread  (%1)"** (two spaces; icon `mail-inbox`) = global unread view; selected by default on open.
  - Item text `"<name>  (<unread count>)"` (two spaces) for folders and feeds, updated live.
  - Icons: folder `directory`; feed: `loading` while loading, `task-reject` (fallback `unavailable`) on error, else the downloaded favicon (`<uidHex>.ico`) or fallback `application-rss`.
  - Drag & drop: cannot drop onto the Unread item or onto feeds, cannot drag the Unread item; dropping on a folder (or empty area = root) moves all selected items (keeping names); destination folder is expanded.
  - Expanded folders persisted (`GUI/RSSWidget/OpenedFolders`, backslash paths; only deepest expanded folders stored).
  - Keyboard: **F2** rename, **Delete** (platform delete key) delete; **double-click** = rename.
- **Feed tree context menu**:
  - With selection: "Update" (icon `view-refresh`), "Mark items read", separator; then if exactly one item selected and it is not the Unread item: "Rename..." , "Feed options..." (feeds only; icon `edit-rename`), "Delete" (icon `edit-clear`), separator, "New folder..." (folders only); if multiple selected: "Delete", separator. Then "New subscription..."; and if the first selected item is a feed: separator, "Copy feed URL".
  - Empty area (selection cleared): "New subscription...", "New folder...", separator, "Update all feeds".
- **Actions**:
  - *New subscription*: destination folder = selected folder, or parent of selected feed, or root (also root if Unread selected). Opens "RSS Feed Options" dialog (`RSSFeedDialog`): "URL:" (prefilled with clipboard text if it has a supported scheme, else `https://`; OK disabled while empty), "Refresh interval:" spin box (0…INT_MAX, suffix " sec", adaptive step, 0 displays **"Default"** = use global interval). On OK: `addFeed(url.trimmed(), <folder>\<url>, interval)`; on error show warning (title "qBittorrent") and re-show dialog. Destination folder expanded; new feed selected.
  - *New folder...*: input "Please choose a folder name" / "Folder name:" default "New folder"; trimmed; empty ignored; created under the destination folder as above; errors shown in warning.
  - *Rename...*: input "Please choose a new name for this RSS feed" / "New feed name:" (prefilled current name); performs `moveItem` to same parent; error → warning "Rename failed" + message, re-prompt.
  - *Feed options...*: same "RSS Feed Options" dialog prefilled with URL and per-feed interval; on OK applies interval, then `setFeedURL` (error → warning).
  - *Delete*: confirmation "Deletion confirmation" / "Are you sure you want to delete the selected RSS feeds?" (Yes/No, default No); Unread item ignored.
  - *Update*: refresh each selected item (folder = recursive); if Unread item selected → refresh all.
  - *Mark items read*: mark selected items read (Unread item → everything).
  - *Copy feed URL*: URLs of selected feeds joined with `\n` to clipboard.
  - *Update all* / *Update all feeds*: refresh root folder.
  - *RSS Downloader...*: opens the RSS Downloader dialog (5.12.3).
- **Article list** (`ArticleListWidget`, `QListWidget`, extended selection, small icons):
  - Shows articles of the selected feed/folder (folder = all descendant articles, newest first); Unread item → only unread articles of all feeds (read ones remain until list rebuilt). Filter text = case-insensitive substring of title. New articles inserted at the top live; removed articles disappear.
  - Display: title only. Read: foreground theme color `RSS.ReadArticle` or palette Inactive WindowText, icon `rss_read_article` (fallback `sphere`). Unread: theme color `RSS.UnreadArticle` or palette Link color, icon `rss_unread_article`. (No bold.)
  - Selecting another article marks the **previously current** article as read; the last viewed article is marked read when the widget is destroyed.
  - Context menu (only entries applicable to the selection): "Download torrent" (if any selected article has a torrent URL; icon `downloading`), "Open news URL" (if any has a link; icon `application-url`).
  - **Double-click** article = Download torrent.
  - *Download torrent*: for each selected article: mark read; if `torrentUrl()` not supported (5.9.4) → blocked with WARNING `Blocked adding torrent from RSS article. Unsupported torrent URL. Only HTTP(S) URLs, magnet URIs and info hashes are supported. Article: "%1". URL: "%2".`; else `AddTorrentManager::addTorrent(url)` with default options (Add-torrent dialog per preference). Summary warning: "Blocked adding torrent from RSS article. The following article has an unsupported torrent URL and it may be malicious behaviour:\n%1" (+ "\nThere are %1 more articles with the same issue.").
  - *Open news URL*: mark read; empty link counted; `file://` link blocked (WARNING `Blocked opening RSS article URL. URL pointing to local file might be malicious behaviour. Article: "%1". URL: "%2".`); else open in browser. Summary: blocked → "Blocked opening RSS article URL. The following article URL is pointing to local file and it may be malicious behaviour:\n%1"; else empty → "The following article has no news URL provided:\n%1" (each + "\nThere are %1 more articles with the same issue.").
- **Article preview** (`renderArticle`) HTML template (colors from the browser palette: Highlight, HighlightedText, AlternateBase):
  ```html
  <div style='border: 2px solid red; margin-left: 5px; margin-right: 5px; margin-bottom: 5px;'>
    <div style='background-color: "{Highlight}"; font-weight: bold; color: "{HighlightedText}";'>{title}</div>
    [if date valid]   <div style='background-color: "{AlternateBase}";'><b>Date: </b>{date: system locale ShortFormat, local time}</div>
    [if Unread view]  <div style='background-color: "{AlternateBase}";'><b>Feed: </b>{feed title}</div>
    [if author]       <div style='background-color: "{AlternateBase}";'><b>Author: </b>{author}</div>
    [if link]         <div style='background-color: "{AlternateBase}";'><a href='{link}' target='_blank'><b>Open link</b></a></div>
  </div>
  <div style='margin-left: 5px; margin-right: 5px;'>{body}</div>
  ```
  - Body: if `Qt::mightBeRichText(description)` → inserted as HTML. Otherwise BBCode → HTML conversion (regexes are **non-greedy** (InvertedGreediness) and case-insensitive), then wrapped in `<pre>…</pre>`:
    - `\[img\](.+)\[/img\]` → `<img src="\1">`
    - `\[url=(")?(.+)\1\]` → `<a href="\2">`; literal `[/url]` → `</a>`
    - `\[(/)?([bius])\]` → `<\1\2>`
    - `\[color=(")?(.+)\1\]` → `<span style="color:\2">`; `[/color]` → `</span>`
    - `\[size=(")?(.+)\d\1\]` → `<span style="font-size:\2px">`; `[/size]` → `</span>`
  - Relative URL fix-up: base = article link with path and query removed (`scheme://host[:port]`), normalized to end with `/`. Regex (case-insensitive): `((<a\s+[^>]*?href|<img\s+[^>]*?src)\s*=\s*["'])((https?|ftp):)?(\/\/[^\/]*)?(\/?[^\/"].*?)(["'])`. For each match: has scheme → leave; protocol-relative `//host/...` → `<base scheme>:` + `//host` (note: path part is dropped by the original code — bug-compatible); otherwise `base + path` (leading `/` stripped).
  - Re-rendered when the palette changes (theme switch).
- **`HtmlBrowser`** (QTextBrowser): images loaded asynchronously through a `QNetworkAccessManager` with a disk cache at `<Cache>/rss`, **max 50 MiB**; scheme-less image URLs get `http`; cache `PreferCache`; failed/empty image responses are replaced by a 32×32 warning icon PNG stored in the cache with 1-day expiry; the document is re-rendered after each image arrives, preserving scroll position. Links open externally.

#### 5.12.2 Options dialog RSS page (summary; full keys in 5.9.1)
- Group "RSS Reader": "Enable fetching RSS feeds", "Feeds refresh interval:", "Same host request delay:", "Maximum number of articles per feed:".
- Group "RSS Torrent Auto Downloader": "Enable auto downloading of RSS torrents", button "Edit auto downloading rules..." (opens 5.12.3).
- Group "RSS Smart Episode Filter": "Download REPACK/PROPER episodes", "Filters:" multi-line text (one regex per line).

#### 5.12.3 RSS Downloader dialog (`AutomatedRssDownloader`, title "RSS Downloader")
- Warning banner (hidden when auto-downloading enabled): "Auto downloading of RSS torrents is currently disabled. You can enable it in application settings."
- Three-pane splitter (`GUI/Qt6/RSSFeedDownloader/HSplitterSizes`; rule-definition splitter `GUI/Qt6/RSSFeedDownloader/RuleDefSplitterState`; dialog size `RssFeedDownloader/geometrySize`); only the right (matching articles) pane collapsible.
  1. **"Download Rules"** list: rule names with checkboxes (= `enabled`), sorted, extended selection. Tool buttons: rename (tooltip "Rename selected rule. You can also use the F2 hotkey to rename."; enabled only with exactly one selected), remove, add. **F2** rename, **Delete** remove, **double-click** rename. Context menu: "Add new rule..."; with one selected: "Delete rule", separator, "Rename rule..."; with several: "Delete selected rules"; then (any selection) separator, "Clear downloaded episodes..." (confirm "Clear downloaded episodes" / "Are you sure you want to clear the list of downloaded episodes for the selected rule?" → empties `previouslyMatchedEpisodes` of the current rule).
  2. **Rule definition** (scroll area, enabled only when exactly one rule selected):
     - "Priority:" spin (INT_MIN…INT_MAX).
     - "Use Regular Expressions" checkbox.
     - "Must Contain:" line edit + validity icon; "Must Not Contain:" line edit + validity icon. Invalid regex/wildcard → red text + warning icon (`dialog-warning`), tooltip "Position %1: %2" in regex mode. Field tooltips: regex mode "Regex mode: use Perl-compatible regular expressions"; wildcard mode "Wildcard mode: you can use" • "? to match any single character" • "* to match zero or more of any characters" • "Whitespaces count as AND operators (all words, any order)" • "| is used as OR operator" + "If word order is important use * instead of whitespace."; both followed by "An expression with an empty | clause (e.g. expr|)" + " will match all articles." (Must Contain) / " will exclude all articles." (Must Not Contain).
     - "Episode Filter:" line edit + validity icon (tooltip in 5.11.2).
     - "Use Smart Episode Filter" checkbox, tooltip "Smart Episode Filter will check the episode number to prevent downloading of duplicates.\nSupports the formats: S01E01, 1x1, 2017.12.31 and 31.12.2017 (Date formats also support - as a separator)".
     - "Ignore Subsequent Matches for (0 to Disable)" + spin 0–365 (suffix " days", 0 shows "Disabled").
     - Last-match label: "Last Match: %1 days ago" (days from lastMatch to now) or "Last Match: Unknown".
     - Group **"Torrent parameters"** (`AddTorrentParamsWidget`): "Torrent Management Mode:" (Default / Manual / Automatic; tooltip "Automatic mode means that various torrent properties(eg save path) will be decided by the associated category"), "Save at" path (+ "Note: the current defaults are displayed for reference."), "Use another path for incomplete torrents:" (Default / Yes / No + path), "Category:" (editable combo of categories), "Tags:" (read-only list + "..." button "Add/remove tags", hint "Click [...] button to add/remove tags."), "Start torrent:" (Default / Yes / No ⇒ `stopped` = !value), "Content layout:" (Default / Original / Create subfolder / Don't create subfolder), "Stop condition:" (Default / None / Metadata received / Files checked), "Add to top of queue:" (Default / Yes / No), "Skip hash check" checkbox, "Torrent share limits" group (ratio / seeding time / inactive seeding time / action — same widget as Share limits dialog).
  3. **"Apply Rule to Feeds:"** checklist of all feeds (by name, data = URL, sorted); tri-state when multiple rules selected (checked if all selected rules include the feed, partially if some); hidden/disabled with no selection; toggling updates `affectedFeeds` of every selected rule.
  4. **"Matching RSS Articles"** tree (sorted): for each selected rule (using unsaved edits for the current one) and each affected feed, top-level bold item = feed name (icon `directory`), children = titles of that feed's articles for which `rule.matches()` is true (de-duplicated). Recomputed live on every edit.
- Edits are written to the rule (`setRule`) when the selection changes and when the dialog closes (OK/Cancel both close; no explicit save button semantics).
- New rule: input "New rule name" / "Please type the name of the new download rule."; duplicate → "Rule name conflict" / "A rule with this name already exists, please choose another name.". New rules default: enabled, priority 0, everything empty, no feeds.
- Remove: confirm "Rule deletion confirmation" / "Are you sure you want to remove the download rule named '%1'?" or "Are you sure you want to remove the selected download rules?".
- Rename: input "Rule renaming" / "Please type the new rule name" (trimmed); conflict message as above, re-prompt.
- Buttons: "&Import...", "&Export...", standard OK/Cancel box.

---

## 6. WebAPI v2

Source of truth: `src/webui/webapplication.{h,cpp}`, `src/webui/webui.cpp`, `src/webui/clientdatastorage.cpp`, `src/webui/api/*controller.cpp`, `src/webui/api/serialize/serialize_torrent.cpp`, `src/base/http/*`, `WebAPI_Changelog.md`.

### 6.1 Versions and identity

| Item | Value | Where |
|---|---|---|
| qBittorrent version | `5.2.4` (`QBT_VERSION` = `"v5.2.4"`, `QBT_VERSION_2` = `"5.2.4"`) | `src/base/version.h.in` |
| WebAPI version | `2.15.1` (`API_VERSION {2, 15, 1}`) | `src/webui/webapplication.h:60` |
| `app/version` response | `v5.2.4` (plain text, **with** leading `v`) | `appcontroller.cpp versionAction` |
| `app/webapiVersion` response | `2.15.1` (plain text) | `appcontroller.cpp webapiVersionAction` |
| Base path | `/api/v2/<scope>/<action>`; regex `^/api/v2/(?<scope>[A-Za-z_][A-Za-z_0-9]*)/(?<action>[A-Za-z_][A-Za-z_0-9]*)$` | `webapplication.h` |
| Scopes | `auth`, `app`, `clientdata`, `log`, `sync`, `transfer`, `torrents`, `rss`, `search`, `torrentcreator` | `sessionStartImpl` |

Action dispatch: the action name is mapped to a Qt slot `<action>Action` by name (`QMetaObject::invokeMethod`). Action names are case-sensitive (e.g. `torrents/SSLParameters` has a capital `SSL`). Unknown action within a known scope -> `404` with body `Endpoint does not exist`.

### 6.2 HTTP server layer (transport)

Implemented in `src/base/http/` (custom HTTP/1.1 server on `QTcpServer`):

- Listens on `Preferences/WebUI/Address` (default `*` = any address, IPv4+IPv6) and `Preferences/WebUI/Port` (default `8080`). Optional UPnP/NAT-PMP mapping of the WebUI port (profile name `"webui"`) when "Use UPnP / NAT-PMP to forward the port from my router" is on.
- Server never uses a proxy (`QNetworkProxy::NoProxy`).
- **Connection limit:** 500 concurrent connections; extra connections are silently closed (log: `Too many connections. Exceeded CONNECTIONS_LIMIT (500). Connection closed.`).
- **Keep-alive:** idle connections are dropped after 7 s; scanned every 2 s. Every normal response carries `Connection: keep-alive`.
- **Methods:** request method must be all-uppercase. Parser accepts only `GET`, `HEAD`, `POST`; anything else -> `501 Not Implemented` + `Connection: close`. Malformed request -> `400 Bad Request` + close. `HEAD` is processed as `GET` and the body is stripped (Content-Length still set).
- **Body size:** `MAX_CONTENT_SIZE` = 64 MiB. If buffered data exceeds 64 MiB x 1.1 -> `413 Payload Too Large` + close.
- **Parameter sources:** for `GET` (and `HEAD`) the handler sees only query-string params; for `POST` it sees only body params (query string is ignored). Query parsing: split on `&`, then percent-decode name and value, `+` -> space; params without a name are ignored.
- **POST bodies:** `application/x-www-form-urlencoded` (`+` -> space, then fully decoded) or `multipart/form-data` (boundary from Content-Type). In multipart, a part with `filename=` in Content-Disposition becomes an uploaded file `{filename, type, data}`; a part with only `name=` becomes a string param. **Uploaded files are keyed by their filename** (`data[torrent.filename]`), so two uploads with the same filename collide (the last wins). The multipart field name of file parts is irrelevant.
- **Gzip:** if `Accept-Encoding` contains `gzip` or `*` with q > 0, responses larger than 1024 bytes (except `image/gif`, `image/png`) are gzip-compressed at level 6, but only if the result saves more than 24 bytes.
- **HTTPS:** when enabled, cert/key files (PEM; max size `Utils::Net::MAX_SSL_FILE_SIZE`) are loaded. Cipher list = Qt supported ciphers minus: names containing `-cbc-`, `idea`, `rc4`; names starting `adh-`, `aecdh-`, `psk-`, `rsa-`; and the RSA-kx / AES-CBC shorthand suites (`AES256-GCM-SHA384`, `AES128-GCM-SHA256`, `AES256-SHA256`, `AES128-SHA256`, `AES256-SHA`, `AES128-SHA`, `ECDHE-ECDSA-AES256-SHA384`, `ECDHE-RSA-AES256-SHA384`, `DHE-RSA-AES256-SHA256`, `ECDHE-ECDSA-AES128-SHA256`, `ECDHE-RSA-AES128-SHA256`, `DHE-RSA-AES128-SHA256`, `ECDHE-ECDSA-AES256-SHA`, `ECDHE-RSA-AES256-SHA`, `DHE-RSA-AES256-SHA`, `ECDHE-ECDSA-AES128-SHA`, `ECDHE-RSA-AES128-SHA`, `DHE-RSA-AES128-SHA`). Peer verification off. On HTTPS setup failure: log `WebUI: HTTPS setup failed, fallback to HTTP` (CRITICAL) and serve plain HTTP.
- Log messages (WebUI component): `WebUI: HTTPS setup successful`, `WebUI: Now listening on IP: %1, port: %2`, `Unable to bind to IP: %1, port: %2. Reason: %3`, `Credentials are not set` (error state when WebUI enabled but username or password hash empty), `Using built-in WebUI.`, `Using custom WebUI. Location: "%1".`, `WebUI translation for selected locale (%1) has been successfully loaded.`, `Couldn't load WebUI translation for selected locale (%1).`
- WebUI enabled default: `false` in the GUI build, `true` in the headless (`qbittorrent-nox`) build.
- **Headless temporary password:** in `nox`, if no password hash is stored, a random 9-character password (alphabet `23456789ABCDEFGHIJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz`) is generated for this run only and printed to stdout: `The WebUI administrator username is: %1` / `The WebUI administrator password was not set. A temporary password is provided for this session: %1` / `You should set your own password in program preferences.` It is discarded as soon as a real password is stored.
- Dynamic DNS updater (`Net::DNSUpdater`) is owned by the WebUI component and only runs while the WebUI is enabled (see preferences.md, WebUI page).

### 6.3 Request processing pipeline (per request, in order)

1. Reset state; collect params (6.2).
2. Detect API-key mode: `Authorization: Bearer <token>` (scheme match is case-insensitive; exactly one space after the scheme; token trimmed, non-empty).
3. **Block suspicious requests -> `401 Unauthorized`:**
   - if not API-key mode **and** CSRF protection is on **and** the request is cross-site (6.5.2); or
   - if Host-header validation is on and the Host header is invalid (6.5.3).
4. Resolve the client address (reverse-proxy aware, 6.5.4). Used for auth bypass, banning and logs.
5. Session init: API-key session (6.4.3) or cookie/Basic session (6.4.1, 6.4.2).
6. Route:
   - Path does not match `/api/v2/...`: in API-key mode -> `404`; otherwise serve a static WebUI file (6.6).
   - No session and `scope/action` not public -> `403 Forbidden`. The **only public API is `auth/login`**.
   - Controller lookup: the session's controller for the scope; if none and scope is `auth` -> the shared AuthController (in API-key mode `auth/*` -> `403`); otherwise -> `404`.
   - HTTP method filter (6.7): actions listed as POST-only reject any other method with `405 Method Not Allowed`; all other actions accept `GET` or `POST` only (else `405`).
   - Invoke the action.
7. Append prebuilt security headers (6.5.5) to **every** response, including error responses.

#### 6.3.1 Response conventions

| Controller result | HTTP status | Body / Content-Type |
|---|---|---|
| Action never called `setResult` (null result) | `204 No Content` | empty |
| `setResult(QString)` (including empty string) | `200 OK` | text, `text/plain; charset=UTF-8` |
| `setResult(QJsonObject / QJsonArray)` | `200 OK` | compact JSON, `application/json` |
| `setResult(QByteArray, mime, filename)` | `200 OK` | raw bytes with given MIME (default text/plain); if filename given adds `Content-Disposition: attachment; filename="<filename>"` |
| Status set to `Async` | `202 Accepted` | as above |

Endpoints that return `204` on success (no `setResult`): `auth/login`, `app/deleteAPIKey`, `clientdata/store`, `torrents/addTrackers`, `torrents/editTracker`, `torrents/removeTrackers`, `torrents/setComment`, `torrents/setTags`, `rss/setFeedRefreshInterval`, and `rss/markAsRead` when `itemPath` does not exist. Most other mutating endpoints return `200` with an empty `text/plain` body (kept for backward compatibility per changelog 2.11.8).

Error mapping (`APIErrorType` -> HTTP). Body is the error message if any, otherwise the status text; `text/plain; charset=UTF-8`:

| APIErrorType | HTTP |
|---|---|
| `BadParams` | `400 Bad Request` |
| `Unauthorized` | `401 Unauthorized` |
| `AccessDenied` | `403 Forbidden` |
| `NotFound` | `404 Not Found` |
| (method filter) | `405 Method Not Allowed` |
| `Conflict` | `409 Conflict` |
| `BadData` | `415 Unsupported Media Type` |
| (static file read error / alt-UI violation) | `500 Internal Server Error` |

Missing required params -> `400` with body `Missing required parameters: <comma-separated names>`. Required means "key present"; an empty value satisfies `requireParams`.

Boolean params are parsed with `parseBool`: only case-insensitive `true` / `false` are recognised; anything else is "absent" and falls back to the documented default.

### 6.4 Authentication and sessions

#### 6.4.1 Cookie session (browser and default API clients)

- Cookie name: `QBT_SID_<WebUI port>` (e.g. `QBT_SID_8080`); renamed when the port changes.
- Session ID: base64 of 24 random bytes (6 x `quint32`), unique among live sessions.
- Cookie attributes: `HttpOnly`; `Path=/`; `Expires` = now + session timeout (or now + 1 year if timeout <= 0); `Secure` if "secure cookie" is on **and** the origin is trustworthy (HTTPS enabled, or reverse-proxy support on and `X-Forwarded-Proto: https`); `SameSite=Lax` if CSRF protection is on, else `SameSite=None` when Secure, else unset.
- Session timeout: `Preferences/WebUI/SessionTimeout` seconds (default 3600). `<= 0` means never expire. Expiry is idle-based (timestamp refreshed on each request). Expired sessions are dropped on lookup and swept on every new session creation.
- Cookie refresh: the `Set-Cookie` is re-sent when the refresh timer elapses; refresh period = `min(timeout/2, 1 day)` (Safari caps cookie persistence at 7 days). Any preference change resets all sessions' refresh timers to 0 so the next request re-issues the cookie.
- Each session owns its own controller instances (`app`, `clientdata`, `log`, `torrentcreator` (shared manager), `rss`, `search`, `torrents`, `transfer`, `sync`). Consequence: search jobs, `sync/maindata` state, and `torrents/fetchMetadata` caches are **per session**.

#### 6.4.2 Login, Basic auth and auth bypass

- If no valid session cookie:
  - **Auth bypass** (`isAuthNeeded() == false`) -> a session is created automatically:
    - client address is loopback and "Bypass authentication for clients on localhost" is on (pref `LocalHostAuth` = false; default: auth required for localhost), or
    - "Bypass authentication for clients in whitelisted IP subnets" is on and the client IP is in one of the configured subnets.
  - **HTTP Basic** (`Authorization: Basic base64(user:pass)`, added in 2.15.0): split at the first `:` (username must be non-empty); valid -> session created (cookie set); invalid -> `401`.
- `POST auth/login` params `username`, `password` (form fields):
  - Already have a session -> `204` (no-op).
  - IP banned -> `403` body `Your IP address has been banned after too many failed authentication attempts.`
  - Credentials valid -> new session, `Set-Cookie`, `204`. Clears the failure counter for that IP. Log `WebAPI login success. IP: %1`.
  - Invalid -> `401` (since 2.14.0). Log `WebAPI login failure. Reason: invalid credentials, attempt count: %1, IP: %2, username: %3`.
- Credential check: username compared in constant time; password verified against stored `PBKDF2-HMAC-SHA512`, 100000 iterations, 16-byte random salt, 64-byte output, stored as `base64(salt):base64(hash)` (`Preferences/WebUI/Password_PBKDF2`). Default username `admin`.
- **Ban:** per client address. If `MaxAuthenticationFailCount` (default 5) > 0, each failure increments the counter; on reaching the max, a ban timer of `BanDuration` (default 3600 s) starts. While banned, login returns `403` (log `WebAPI login failure. Reason: IP has been banned, IP: %1, username: %2`). When the ban expires the record is erased. Counters live in memory only.
- `POST auth/logout`: deletes the session, sends an expired cookie (`Expires` = yesterday, `Path=/`), `200` empty body.

#### 6.4.3 API key (Bearer) auth (added 2.14.1)

- Format: `qbt_` + 28 chars from the password alphabet above (total 32 chars). A stored key that does not match this format is ignored.
- `Authorization: Bearer <key>` compared in constant time. On match, a session whose ID **is the key itself** is reused or created (no cookie, never expires).
- In API-key mode: CSRF check is skipped; non-API paths -> `404`; `auth/*` -> `403`. Wrong/missing key -> no session -> `403` for any API.
- Managed via `POST app/rotateAPIKey` (generate + store + return) and `POST app/deleteAPIKey` (clear). Exposed read-only in `app/preferences` as `web_ui_api_key`.

### 6.5 Security checks

#### 6.5.1 Settings involved (defaults)
`CSRFProtection` true, `ClickjackingProtection` true, `SecureCookie` true, `HostHeaderValidation` true, `ServerDomains` `*`, `HTTPS/Enabled` false, `ReverseProxySupportEnabled` false, `TrustedReverseProxiesList` empty, `CustomHTTPHeadersEnabled` false, `AlternativeUIEnabled` false (full key list and JSON names in preferences.md, WebUI section).

#### 6.5.2 CSRF (cross-site request) check
- Target origin = `X-Forwarded-Host` (falling back to `Host`) when reverse-proxy support is on, else `Host`. If the value has no `://`, `http://` is prepended before URL parsing.
- If both `Origin` and `Referer` are absent -> allowed (permissive on purpose).
- If `Origin` present -> cross-site iff host or port differ from the target origin (scheme is not compared). Logs `WebUI: Origin header & Target origin mismatch! Source IP: '%1'. Origin header: '%2'. Target origin: '%3'`.
- Else `Referer` is compared the same way (log `WebUI: Referer header & Target origin mismatch! ...`).
- Cross-site -> `401`.

#### 6.5.3 Host header validation
- If the Host header has a port and it differs from the local socket port -> invalid (log `WebUI: Invalid Host header, port mismatch. Request source IP: '%1'. Server port: '%2'. Received Host header: '%3'`).
- Valid if the host equals the local address the request arrived on, or matches any entry of "Server domains" (`;`-separated, trimmed; wildcard patterns, case-insensitive, regex search). Otherwise invalid (log `WebUI: Invalid Host header. Request source IP: '%1'. Received Host header: '%2'`) -> `401`.

#### 6.5.4 Reverse proxy
- Enabled only if "Enable reverse proxy support" is on **and** the trusted list (`;`-separated IPs or CIDR subnets; bare IPv4 -> /32, bare IPv6 -> /128) parses to at least one subnet.
- Client address is overridden only when the socket peer is in the trusted list: the first **global** IP in `X-Forwarded-For` (comma-separated), else the first parseable entry, else the socket peer.
- `X-Forwarded-Proto: https` makes the origin "trustworthy" for the Secure cookie flag.

#### 6.5.5 Prebuilt response headers (all responses)
- `X-XSS-Protection: 1; mode=block`
- `X-Content-Type-Options: nosniff`
- Built-in UI only: `Cross-Origin-Opener-Policy: same-origin`, `Referrer-Policy: same-origin`
- Clickjacking protection on: `X-Frame-Options: SAMEORIGIN`
- `Content-Security-Policy`: built-in UI: `default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; script-src 'self' 'unsafe-inline'; object-src 'none'; form-action 'self'; frame-src 'self' blob:;` then append ` frame-ancestors 'self';` if clickjacking protection, then ` upgrade-insecure-requests;` if HTTPS. With an alternative UI the base policy is empty (only the appended parts, header omitted if empty).
- Custom headers (if enabled): newline-separated `Name: value` lines; lines without `:` are skipped with warning `Missing ':' separator in WebUI custom HTTP header: "%1"`. Appended after the built-ins.

### 6.6 Static WebUI file serving

- Root: built-in resources `:/www` or, if "Use alternative WebUI" is on, the configured folder. Sub-folders: `/public` (served without session; login page) and `/private` (served with session; falls back to `/public` if the private file is missing). `/` maps to `/index.html`.
- Path containing `\`, or a `.` / `..` segment -> `400`.
- Alternative UI hardening: only regular files (`500 Unacceptable file type, only regular file is allowed.`), and no symlinked directories between the file and the root (`500 Symlinks inside alternative UI folder are forbidden.`). In `nox`, if `index.html` itself fails these checks the alternative UI is automatically disabled.
- Max file size 10 MiB (`500` if exceeded; missing file -> `404`).
- MIME type from file name + content. Text files (`text/plain` descendants: html/js/css...) are translated: `QBT_TR(<source>)QBT_TR[CONTEXT=<ctx>]` replaced by the translation from `translations/webui_<locale>.qm` (falls back to source text; `"` escaped as `&#34;`), `${LANG}` -> first 2 chars of locale, `${CACHEID}` -> random per-process base-36 cache-buster, and in `private/views/preferences.html` `${LANGUAGE_OPTIONS}` -> `<option value="xx">Localized name</option>` list built from available `webui_*.qm`. Translated output is cached in memory keyed by path and invalidated by mtime / locale / root change.
- `Cache-Control`: `image/*` -> `private, max-age=604800`; CSS/JS -> `private, max-age=43200`; everything else -> `no-store`.

### 6.7 Endpoint method matrix

POST-only (any other method -> 405):

`app/deleteAPIKey`, `app/rotateAPIKey`, `app/sendTestEmail`, `app/setCookies`, `app/setPreferences`, `app/shutdown`, `auth/login`, `auth/logout`, `clientdata/store`, `rss/addFeed`, `rss/addFolder`, `rss/markAsRead`, `rss/moveItem`, `rss/refreshItem`, `rss/removeItem`, `rss/removeRule`, `rss/renameRule`, `rss/setFeedURL`, `rss/setRule`, `search/delete`, `search/enablePlugin`, `search/installPlugin`, `search/start`, `search/stop`, `search/uninstallPlugin`, `search/updatePlugins`, `torrentcreator/addTask`, `torrentcreator/deleteTask`, `torrents/add`, `torrents/addPeers`, `torrents/addTags`, `torrents/addTrackers`, `torrents/addWebSeeds`, `torrents/bottomPrio`, `torrents/createCategory`, `torrents/createTags`, `torrents/decreasePrio`, `torrents/delete`, `torrents/deleteTags`, `torrents/editCategory`, `torrents/editTracker`, `torrents/editWebSeed`, `torrents/fetchMetadata`, `torrents/filePrio`, `torrents/increasePrio`, `torrents/parseMetadata`, `torrents/reannounce`, `torrents/recheck`, `torrents/removeCategories`, `torrents/removeTags`, `torrents/removeTrackers`, `torrents/removeWebSeeds`, `torrents/rename`, `torrents/renameFile`, `torrents/renameFolder`, `torrents/setAutoManagement`, `torrents/setCategory`, `torrents/setComment`, `torrents/setDownloadLimit`, `torrents/setDownloadPath`, `torrents/setForceStart`, `torrents/setLocation`, `torrents/setSavePath`, `torrents/setShareLimits`, `torrents/setSSLParameters`, `torrents/setSuperSeeding`, `torrents/setTags`, `torrents/setUploadLimit`, `torrents/start`, `torrents/stop`, `torrents/toggleFirstLastPiecePrio`, `torrents/toggleSequentialDownload`, `torrents/topPrio`, `transfer/banPeers`, `transfer/setDownloadLimit`, `transfer/setSpeedLimitsMode`, `transfer/setUploadLimit`, `transfer/toggleSpeedLimitsMode`.

GET or POST:

`app/buildInfo`, `app/cookies`, `app/defaultSavePath`, `app/getDirectoryContent`, `app/networkInterfaceAddressList`, `app/networkInterfaceList`, `app/preferences`, `app/processInfo`, `app/version`, `app/webapiVersion`, `clientdata/load`, `log/main`, `log/peers`, `rss/items`, `rss/matchingArticles`, `rss/rules`, `rss/setFeedRefreshInterval` (mutating but not restricted), `search/downloadTorrent` (mutating but not restricted), `search/plugins`, `search/results`, `search/status`, `sync/maindata`, `sync/torrentPeers`, `torrentcreator/status`, `torrentcreator/torrentFile`, `torrents/categories`, `torrents/count`, `torrents/downloadLimit`, `torrents/export`, `torrents/files`, `torrents/info`, `torrents/pieceAvailability`, `torrents/pieceHashes`, `torrents/pieceStates`, `torrents/properties`, `torrents/saveMetadata`, `torrents/SSLParameters`, `torrents/tags`, `torrents/trackers`, `torrents/uploadLimit`, `torrents/webseeds`, `transfer/downloadLimit`, `transfer/info`, `transfer/speedLimitsMode`, `transfer/uploadLimit`.

Total: 2 auth + 16 app + 2 clientdata + 2 log + 2 sync + 9 transfer + 61 torrents + 14 rss + 11 search + 4 torrentcreator = 123 endpoints.

**Not present in 5.2.4** (clients written for older APIs will get 404 `Endpoint does not exist`): `torrents/pause`, `torrents/resume` (renamed `stop`/`start` in API 2.11.0), and the `paused` / `root_folder` / `cookie` params of `torrents/add` (silently ignored; use `stopped`, `contentLayout`).

### 6.8 Common parameter conventions

- **Torrent IDs** (`hash`, `hashes`, `id`): the TorrentID = 40-char lowercase hex. For v1 torrents it is the v1 SHA-1 infohash; for v2/hybrid torrents it is the v2 SHA-256 infohash truncated to 20 bytes (40 hex).
- **Multiple hashes:** `|`-separated. The literal `all` (as the only element) means every torrent in the session (`applyToTorrents`). Unknown hashes are silently skipped (no error) in bulk actions.
- **Lists of URLs / names:** `|`-separated unless noted. Tags: `,`-separated. Categories in `removeCategories`: `\n`-separated. `torrents/add` URLs: `\n`-separated.
- **Speed limits:** bytes per second. Torrent-level: `0`/negative = unlimited (stored as 0). Global: `0`/`-1` = unlimited; stored internally in KiB/s (values 1..1024 round to 1 KiB/s; otherwise integer-divided by 1024) and reported back as KiB x 1024.
- **Share limits:** ratio `-2` = use default (category, then global), `-1` = no limit, else >= 0. Seeding / inactive-seeding time in **minutes**: `-2` default, `-1` no limit. Values below -2 are coerced to -1.
- **Times:** epoch seconds; `-1` for invalid/unknown dates.
- **Paths** in responses: `Path::toString()` = native separators (save paths, content paths); file paths inside torrents use `/` (platform-independent).

### 6.9 `auth/*`

| Endpoint | Method | Params | Success | Errors |
|---|---|---|---|---|
| `auth/login` | POST | `username`, `password` | `204` + `Set-Cookie: QBT_SID_<port>=...`; `204` if already logged in | `401` invalid; `403` banned; `403` if using Bearer |
| `auth/logout` | POST | - | `200` empty; expired cookie | `403` without session |

### 6.10 `app/*`

#### `app/version` (GET/POST)
Plain text `v5.2.4`.

#### `app/webapiVersion` (GET/POST)
Plain text `2.15.1`.

#### `app/buildInfo` (GET/POST)
JSON object:
| Key | Type | Meaning |
|---|---|---|
| `qt` | string | Qt version (`QT_VERSION_STR`) |
| `libtorrent` | string | libtorrent version |
| `boost` | string | Boost version |
| `openssl` | string | OpenSSL version |
| `zlib` | string | zlib version |
| `bitness` | int | `64` / `32` |
| `platform` | string | `linux` / `macos` / `windows` / `unknown` |

#### `app/processInfo` (GET/POST, added 2.15.1)
`{"launch_time": <int epoch seconds when the process started>}`.

#### `app/shutdown` (POST)
Replies `200` empty, then exits the application after 100 ms.

#### `app/preferences` (GET/POST)
JSON object with every preference key (223 keys; full list, order, types and defaults in `preferences.md`). Read-only keys (returned but ignored by `setPreferences`): `add_trackers_url_list` (content fetched from the "add trackers from URL" URL), `current_interface_name`, `web_ui_api_key`. `web_ui_password` is never returned.

#### `app/setPreferences` (POST)
- Param `json`: JSON object (string) with any subset of keys. Unknown keys are ignored; invalid JSON yields an empty object (no-op, still `200`).
- Applies each present key, then saves (`pref->apply()`); `200` empty.
- Special rules:
  - `max_ratio_enabled: false` -> global ratio limit -1; otherwise `max_ratio` (if present) is applied. Same pattern for `max_seeding_time_enabled`/`max_seeding_time` and `max_inactive_seeding_time_enabled`/`max_inactive_seeding_time` (changelog 2.11.6: send only one of each pair).
  - `schedule_from_hour`+`schedule_from_min` only applied when both present; same for `schedule_to_hour`+`schedule_to_min`.
  - `web_ui_username`: `400` `WebUI username must be at least 3 characters long` / `WebUI username cannot contain a colon`.
  - `web_ui_password`: `400` `WebUI password must be at least 6 characters long`; stored as PBKDF2 hash.
  - `bypass_local_auth: true` stores `LocalHostAuth = false` (inverted).
  - `locale` change installs the new translator immediately.
  - Errors are thrown mid-way: keys processed before the failing key are already applied in memory but `apply()` is not reached for that call.

#### `app/defaultSavePath` (GET/POST)
Plain text: default save path (native separators).

#### `app/sendTestEmail` (POST)
If e-mail notification is enabled, sends subject `Test email`, body `This is a test email.\nThank you for using qBittorrent.\n` from the configured sender to the configured recipient. Always `200` empty (no delivery feedback).

#### `app/getDirectoryContent` (GET/POST)
| Param | Required | Values |
|---|---|---|
| `dirPath` | yes | absolute path; empty or starting with `:` -> `400 Invalid directory path`; relative -> `400`; non-existent -> `404 Directory does not exist` |
| `mode` | no (default `all`) | `all`, `dirs`, `files`; other -> `400 Invalid mode, allowed values: all, dirs, files` |
| `withMetadata` | no (default false) | bool (added 2.11.8) |

Response: JSON array. Without metadata: array of full path strings (excluding `.` and `..`). With metadata: array of objects `{name, type: "dir"|"file", size (files only), creation_date, last_access_date, last_modification_date}` (epoch seconds, -1 if unknown).

#### `app/cookies` (GET/POST)
JSON array of the download manager's cookie jar: `{name, domain, path, value, expirationDate (epoch s)}`.

#### `app/setCookies` (POST)
Param `cookies`: JSON array of the same objects (all keys optional). Replaces the entire cookie jar. Errors `400`: JSON parse error text, `cookies must be array`, non-object element (empty message).

#### `app/rotateAPIKey` (POST, 2.14.1)
Generates a new key, stores it, returns `{"apiKey": "qbt_..."}`.

#### `app/deleteAPIKey` (POST, 2.14.1)
Clears the stored key. `204`.

#### `app/networkInterfaceList` (GET/POST)
JSON array `{name: humanReadableName, value: systemName}` for every interface that has at least one address.

#### `app/networkInterfaceAddressList` (GET/POST)
Param `iface` (required; may be empty). Empty -> all addresses of all interfaces; else the addresses of that interface. JSON array of strings; IPv6 rendered in canonical form.

### 6.11 `clientdata/*` (added 2.13.1)

Persistent key/value store shared by all WebUI clients, file `<data dir>/web_clientdata.json`, max 1 MiB (compact JSON).

| Endpoint | Method | Params | Result |
|---|---|---|---|
| `clientdata/load` | GET/POST | `keys` optional JSON array of strings | JSON object with all stored data, or only the requested existing keys. Errors `400`: JSON parse error, `` `keys` must be an array ``, `` Items of `keys` must be strings `` |
| `clientdata/store` | POST | `data` required JSON object | Merge: a `null` value deletes the key, other values insert/replace. File rewritten only if something changed. `204`. Errors: `400` parse error / `` `data` must be an object ``; `409` `Total web client data must not be larger than 1048576 bytes` or `Failed to save web client data. Error: "%1"` |

### 6.12 `log/*`

Logger keeps two ring buffers of 20000 entries each (general messages and peer-ban events). IDs are monotonically increasing integers.

#### `log/main` (GET/POST)
| Param | Default | Meaning |
|---|---|---|
| `normal` | true | include type 1 |
| `info` | true | include type 2 |
| `warning` | true | include type 4 |
| `critical` | true | include type 8 |
| `last_known_id` | -1 | return only entries with id > this (if not a valid int -> -1 = all) |

Response array of `{id: int, message: string, timestamp: int, type: int}`; type values `1` NORMAL, `2` INFO, `4` WARNING, `8` CRITICAL. **`timestamp` is seconds since epoch** (`QDateTime::currentSecsSinceEpoch()`; the source comment saying "milliseconds" is wrong). IDs start at 0 and increase by 1 per message.

#### `log/peers` (GET/POST)
Param `last_known_id` (default -1). Response array of `{id, ip: string, timestamp: int (seconds since epoch), blocked: bool, reason: string}`. Entries come from two libtorrent alerts: `peer_blocked_alert` -> `blocked: true` with reason one of `IP filter`, `filtered port (%1)`, `I2P mixed mode restrictions`, `privileged port (%1)`, `μTP is disabled`, `TCP is disabled`; `peer_ban_alert` -> `blocked: false`, empty reason (peer banned, e.g. sent bad data / manually banned).

### 6.13 `transfer/*`

#### `transfer/info` (GET/POST)
| Key | Type | Meaning |
|---|---|---|
| `dl_info_speed` | int | global payload download rate B/s |
| `dl_info_data` | int | payload downloaded this session (bytes) |
| `up_info_speed` | int | global payload upload rate B/s |
| `up_info_data` | int | payload uploaded this session |
| `dl_rate_limit` | int | effective global download limit B/s (alt limit if alternative mode on), 0 = unlimited |
| `up_rate_limit` | int | same for upload |
| `last_external_address_v4` | string | last external IPv4 seen (empty if unknown) |
| `last_external_address_v6` | string | last external IPv6 |
| `dht_nodes` | int | DHT nodes |
| `connection_status` | string | `connected` (listening and has incoming connections), `firewalled` (listening, no incoming), `disconnected` (not listening) |

#### Other transfer endpoints
| Endpoint | Method | Params | Response |
|---|---|---|---|
| `transfer/speedLimitsMode` | GET/POST | - | text `1` if alternative speed limits active else `0` |
| `transfer/setSpeedLimitsMode` | POST | `mode` int (required) | any non-zero = alternative. `400` `'mode': invalid argument` if not an int. `200` |
| `transfer/toggleSpeedLimitsMode` | POST | - | toggles; `200` |
| `transfer/uploadLimit` | GET/POST | - | text: current effective upload limit B/s |
| `transfer/downloadLimit` | GET/POST | - | text: current effective download limit B/s |
| `transfer/setUploadLimit` | POST | `limit` (required, B/s; `0` -> `-1` unlimited) | sets the limit of the **currently active** mode (alt vs normal). `200` |
| `transfer/setDownloadLimit` | POST | `limit` | same for download |
| `transfer/banPeers` | POST | `peers` (required) `\|`-separated `host:port` / `[ipv6]:port` | each parsed peer IP is added to the permanent banned-IP list (port ignored). `200` |

### 6.14 `sync/*` — incremental state synchronisation

Both endpoints are GET/POST and are per-session (each WebSession has its own `SyncController`).

#### 6.14.1 `sync/maindata`

Param: `rid` (int, optional, default 0) = the last response id the client successfully processed.

**Response id (`rid`) semantics**
- Server keeps `lastSentID`, `acceptedID` and a pending change buffer (`syncBuf`) per session.
- New id = `(lastSentID % 1000000) + 1` (cycles 1..1000000).
- If `rid > 0` and `rid == lastSentID`: the client confirmed the previous response -> `acceptedID = rid`, the change buffer is cleared (those changes were delivered).
- If `rid > 0` and `rid == acceptedID`: a **partial (diff) update** is sent (= all changes accumulated since `acceptedID`, i.e. if the client re-sends an older accepted rid because it lost a response, it gets the cumulative diff again).
- Otherwise (rid 0, unknown, too old): **full update** — the whole snapshot is sent with `"full_update": true`.
- Every response contains `"rid": <new id>`.

**First call on a session** builds a snapshot of everything and subscribes to session events (category added/removed/options changed, subcategory support changed, tag added/removed, torrent added/about-to-be-removed/category changed/metadata received/stopped/started/save path changed/saving mode changed/tag added/tag removed, `torrentsUpdated` (periodic stats tick), trackers added/removed/reset, tracker entry statuses updated). Between polls the controller only records *which* items changed (sets of IDs); it serialises them on the next poll.

**Response keys** (each present only when non-empty):

| Key | Type | Content |
|---|---|---|
| `rid` | int | response id (always) |
| `full_update` | bool | `true` only on full updates |
| `torrents` | object | `{<hash>: <torrent object or partial object>}`. Full update: complete objects. Diff: only changed fields of changed torrents; new torrents appear complete. The `hash` field itself is **removed** (it is the key). |
| `torrents_removed` | array<string> | hashes of torrents removed since last accepted |
| `categories` | object | `{<name>: <category object or changed fields>}` |
| `categories_removed` | array<string> | removed category names |
| `tags` | array<string> | full list (full update) or newly added tags (diff) |
| `tags_removed` | array<string> | removed tags |
| `trackers` | object | `{<tracker URL>: [hash, ...]}`. For changed trackers the **complete** hash list is sent (not a diff). |
| `trackers_removed` | array<string> | tracker URLs no longer used by any torrent |
| `server_state` | object | full on full update; on diff only the changed keys |

**Torrent object in maindata** = the `torrents/info` object (6.15.2) minus `hash`, plus three announce flags:
| Key | Type | Meaning |
|---|---|---|
| `has_tracker_warning` | bool | some tracker endpoint is `Working` but has a non-empty message |
| `has_tracker_error` | bool | some tracker is in state `TrackerError` (5) |
| `has_other_announce_error` | bool | some tracker is `NotWorking` (4) or `Unreachable` (6) |
These flags are recomputed only for torrents whose trackers changed / were announced (added, stopped, trackers changed, statuses updated); otherwise the previous values are carried over.

**Category object** (`torrents/categories` uses the same shape):
| Key | Type | Meaning |
|---|---|---|
| `name` | string | full category name (`parent/child` for subcategories) |
| `savePath` | string | category save path (may be empty = default; relative paths are relative to the default save path) |
| `download_path` | string \| false \| null | `null` = use global default for "incomplete" path; `false` = explicitly disabled; string = enabled with that path |
| `ratio_limit` | number | -2 default / -1 none / value |
| `seeding_time_limit` | int (min) | -2 / -1 / value |
| `inactive_seeding_time_limit` | int (min) | -2 / -1 / value |
| `share_limit_action` | string | `Default`, `Stop`, `Remove`, `RemoveWithContent`, `EnableSuperSeeding` |
(Key naming is intentionally mixed: `savePath` camelCase for backward compatibility, the rest snake_case.) Subcategories are always enabled since 2.15.0 (`use_subcategories` key removed from server_state).

**`server_state` keys** (all always present on full update):
| Key | Type | Meaning |
|---|---|---|
| `dl_info_speed` | int | global payload DL rate B/s |
| `dl_info_data` | int | payload downloaded this session |
| `up_info_speed` | int | global payload UL rate B/s |
| `up_info_data` | int | payload uploaded this session |
| `dl_rate_limit` | int | effective global DL limit B/s (0 = unlimited) |
| `up_rate_limit` | int | effective global UL limit B/s |
| `alltime_dl` | int | all-time downloaded bytes (persisted statistics) |
| `alltime_ul` | int | all-time uploaded bytes |
| `total_wasted_session` | int | bytes wasted this session |
| `global_ratio` | string | `alltime_ul/alltime_dl` formatted with 2 decimals, or `"-"` if either is 0 |
| `total_peer_connections` | int | current peer connections |
| `read_cache_hits` | string | read cache hit % (2 decimals) or `"0"` (libtorrent 1.x only meaningful) |
| `total_buffers_size` | int | used disk buffers x 16 KiB (bytes) |
| `write_cache_overload` | string | `100 * diskWriteQueue / peersCount` (2 decimals) or `"0"` |
| `read_cache_overload` | string | `100 * diskReadQueue / peersCount` or `"0"` |
| `queued_io_jobs` | int | disk job queue length |
| `average_time_queue` | int | average disk job time (ms) |
| `total_queued_size` | int | bytes queued for disk I/O |
| `last_external_address_v4` | string | |
| `last_external_address_v6` | string | |
| `dht_nodes` | int | |
| `connection_status` | string | `connected` / `firewalled` / `disconnected` |
| `free_space_on_disk` | int | free bytes on the default save path volume (session's free-disk-space checker re-runs every 30 s on the I/O thread) |
| `queueing` | bool | torrent queueing enabled |
| `use_alt_speed_limits` | bool | alternative speed limits active |
| `refresh_interval` | int | UI refresh interval ms (Advanced setting "Transfer list refresh interval", default 1500) |

**Generic diff algorithm** (`processMap`, used for torrent objects, category objects, server_state and by `sync/torrentPeers`):
- For each key in the new map: scalar (string/number/bool/datetime/null) -> included if `prev[key] != value`; nested map -> recursive diff, included if non-empty; hash-of-maps -> `processHash` (new items whole, changed items as diff, vanished items listed under `<key>_removed`); list of scalars -> `processList` (new elements under `<key>`, vanished under `<key>_removed`; if the previous list was empty the full list is sent).
- Keys that disappear from a flat map are **not** reported.

#### 6.14.2 `sync/torrentPeers`

Params: `hash` (torrent id; unknown -> `404`), `rid` (int, default 0).

Uses the generic `generateSyncData`: one `lastData` / `lastAcceptedData` pair per session (not per torrent), so a client switching torrents must send `rid=0` (otherwise it receives a diff against the other torrent's peers).
- New id = `(lastResponseId % 1000000) + 1` where `lastResponseId` = id of the last response (or 0 if `rid <= 0`).
- If `rid == lastResponseId` the last response becomes accepted. If `rid` equals the accepted response id -> diff against accepted data; else full update (`full_update: true`).

Response keys:
| Key | Type | Meaning |
|---|---|---|
| `rid` | int | |
| `full_update` | bool | full updates only |
| `show_flags` | bool | = "Resolve peer countries" setting (whether country fields are meaningful) |
| `peers` | object | `{<"ip:port" or I2P destination>: <peer object or changed fields>}` |
| `peers_removed` | array<string> | keys of disconnected peers |

Peer object:
| Key | Type | Meaning |
|---|---|---|
| `client` | string | client name from handshake (`simplified()`, non-printables removed) |
| `peer_id_client` | string | first 8 chars of the peer ID if all printable ASCII 32..126; `Unknown` otherwise; empty if peer id not yet known |
| `progress` | float | peer's progress 0..1 |
| `dl_speed` | int | payload download rate from this peer B/s |
| `up_speed` | int | payload upload rate to this peer B/s |
| `downloaded` | int | total downloaded from peer |
| `uploaded` | int | total uploaded to peer |
| `connection` | string | `μTP` if uTP socket, else `BT` (standard BitTorrent) or `Web` (web seed) |
| `flags` | string | space-separated flag letters (see below) |
| `flags_desc` | string | `\n`-separated lines `<letter> = <explanation>` |
| `relevance` | float | (#pieces the peer has that we lack) / (#pieces we lack); 0 if we lack nothing |
| `files` | string | `\n`-separated paths of the file(s) containing the piece currently being downloaded from this peer (only if metadata present) |
| `ip` | string | IP (non-I2P peers only) |
| `port` | int | port (non-I2P only) |
| `host_name` | string | reverse-DNS host name when "Resolve peer host names" is on, else empty (2.15.1; non-I2P only) |
| `country_code` | string | lowercase ISO-3166 alpha-2 from GeoIP when "Resolve peer countries" is on, else `null` |
| `country` | string | localized country name, else `null` |
| `i2p_dest` | string | I2P destination (I2P peers only; then `ip`/`port`/`host_name`/`country*` are absent) (2.13.0) |
Peers whose IP is null and not I2P are skipped.

Flag letters (in this order of evaluation):
| Letter | Explanation (exact UI string) |
|---|---|
| `d` | Interested (local) and choked (peer) |
| `D` | Interested (local) and unchoked (peer) |
| `u` | Interested (peer) and choked (local) |
| `U` | Interested (peer) and unchoked (local) |
| `K` | Not interested (local) and unchoked (peer) |
| `?` | Not interested (peer) and unchoked (local) |
| `O` | Optimistic unchoke |
| `S` | Peer snubbed |
| `I` | Incoming connection |
| `H` | Peer from DHT |
| `X` | Peer from PEX |
| `L` | Peer from LSD |
| `E` | Encrypted traffic |
| `e` | Encrypted handshake |
| `P` | μTP |
| `h` | Peer is using NAT hole punching |

### 6.15 `torrents/*`

#### 6.15.1 State strings (`state` field)

| TorrentState | API string |
|---|---|
| Error | `error` |
| MissingFiles | `missingFiles` |
| Uploading | `uploading` |
| StoppedUploading | `stoppedUP` |
| QueuedUploading | `queuedUP` |
| StalledUploading | `stalledUP` |
| CheckingUploading | `checkingUP` |
| ForcedUploading | `forcedUP` |
| Downloading | `downloading` |
| DownloadingMetadata | `metaDL` |
| ForcedDownloadingMetadata | `forcedMetaDL` |
| StoppedDownloading | `stoppedDL` |
| QueuedDownloading | `queuedDL` |
| StalledDownloading | `stalledDL` |
| CheckingDownloading | `checkingDL` |
| ForcedDownloading | `forcedDL` |
| CheckingResumeData | `checkingResumeData` |
| Moving | `moving` |
| (other) | `unknown` |

(`pausedUP`/`pausedDL` were renamed `stoppedUP`/`stoppedDL` in API 2.11.0.)

#### 6.15.2 Torrent object (`serialize()`; used by `torrents/info` and `sync/maindata`)

| Key | Type | Meaning / computation |
|---|---|---|
| `hash` | string | TorrentID (omitted in maindata) |
| `infohash_v1` | string | v1 SHA-1 hex, `""` for pure v2 |
| `infohash_v2` | string | v2 SHA-256 hex (64), `""` for pure v1 |
| `name` | string | display name (custom name if renamed) |
| `has_metadata` | bool | metadata available |
| `created_by` | string | "created by" field of .torrent |
| `creation_date` | int | epoch s, -1 unknown |
| `private` | bool \| null | private flag; `null` if no metadata yet |
| `total_size` | int | sum of all file sizes (including unwanted) |
| `pieces_num` | int | piece count |
| `piece_size` | int | piece length bytes |
| `magnet_uri` | string | generated magnet link (with trackers) |
| `size` | int | wanted (selected) size |
| `progress` | float | 0..1 progress of wanted data |
| `total_wasted` | int | failed + redundant bytes |
| `pieces_have` | int | pieces downloaded |
| `dlspeed` | int | payload download B/s (0 while stopped) |
| `upspeed` | int | payload upload B/s (0 while stopped) |
| `priority` | int | queue position, 1-based; `0` when not in the queue (seeding torrents, or queueing disabled) |
| `num_seeds` | int | connected seeds |
| `num_complete` | int | seeds in swarm (tracker scrape `num_complete` if > -1, else libtorrent `list_seeds`) |
| `num_leechs` | int | connected leechers (= peers - seeds) |
| `num_incomplete` | int | leechers in swarm (scrape `num_incomplete` if > -1, else `list_peers - list_seeds`) |
| `state` | string | 6.15.1 |
| `eta` | int | seconds; `8640000` (= 100 days, `MAX_ETA`) means infinite. Stopped -> MAX_ETA. Finished torrents: min of ratio ETA, seeding-time ETA, inactive-seeding-time ETA (MAX_ETA if no limits). Downloading: from averaged speed. |
| `seq_dl` | bool | sequential download |
| `f_l_piece_prio` | bool | first/last piece priority |
| `category` | string | `""` = uncategorized |
| `tags` | string | tags joined with `", "` |
| `super_seeding` | bool | |
| `force_start` | bool | forced mode and not stopped |
| `save_path` | string | effective save path (native separators) |
| `download_path` | string | incomplete-download path (`""` if not used); with Auto TMM this is the category's download path |
| `content_path` | string | single-file: the file's absolute path; multi-file: root folder path, or the save location if there is no common root folder; `""` without metadata |
| `root_path` | string | absolute root folder path, `""` if no common root folder or no metadata |
| `added_on` | int | epoch s |
| `completion_on` | int | epoch s, -1 if not completed |
| `tracker` | string | current working tracker URL, else first tracker URL, else `""` |
| `trackers_count` | int | number of trackers |
| `dl_limit` | int | per-torrent DL limit B/s, `0` = unlimited |
| `up_limit` | int | per-torrent UL limit B/s, `0` = unlimited |
| `downloaded` | int | all-time downloaded bytes |
| `uploaded` | int | all-time uploaded bytes |
| `downloaded_session` | int | payload downloaded this session |
| `uploaded_session` | int | payload uploaded this session |
| `amount_left` | int | bytes left of wanted data |
| `completed` | int | wanted bytes done (`total_wanted_done`) |
| `connections_count` | int | current connections |
| `connections_limit` | int | per-torrent connection limit |
| `max_ratio` | number | effective ratio limit (resolves -2 through category, then global); -1 = none |
| `max_seeding_time` | int | effective seeding time limit (min) |
| `max_inactive_seeding_time` | int | effective inactive seeding limit (min) |
| `ratio` | number | real ratio = all-time up / all-time down (if all-time down < 1% of total done, total done is used); infinite -> `-1`; 0/0 -> 0 |
| `ratio_limit` | number | torrent's own setting (-2/-1/value) |
| `popularity` | number | ratio / active months (`active_duration` in months), 0 if never active |
| `seeding_time_limit` | int | own setting (min) |
| `inactive_seeding_time_limit` | int | own setting (min) |
| `share_limit_action` | string | own setting: `Default`, `Stop`, `Remove`, `RemoveWithContent`, `EnableSuperSeeding` (2.12.0) |
| `seen_complete` | int | last time a full copy was seen, epoch s / -1 |
| `auto_tmm` | bool | Automatic Torrent Management |
| `time_active` | int | seconds active (libtorrent `active_duration`) |
| `seeding_time` | int | seconds finished/seeding (`finished_duration`) |
| `last_activity` | int | epoch s of last payload transfer (`now - timeSinceActivity`); if never active -> `added_on` |
| `availability` | number | distributed copies (`distributed_copies`, -1 if unknown) |
| `reannounce` | int | seconds until next announce |
| `comment` | string | torrent comment |
| `files` | array | only with `includeFiles=true` and metadata present: same objects as `torrents/files` (without `is_seed`) |
| `trackers` | array | only with `includeTrackers=true`: same objects as `torrents/trackers` but **without** the 3 DHT/PeX/LSD pseudo-rows |
| `has_tracker_warning`, `has_tracker_error`, `has_other_announce_error` | bool | **maindata only** (6.14.1) |

#### 6.15.3 `torrents/info` (GET/POST)

| Param | Type | Default | Semantics |
|---|---|---|---|
| `filter` | string | `all` | `all`, `downloading`, `seeding`, `completed`, `stopped`, `running`, `active`, `inactive`, `stalled`, `stalled_uploading`, `stalled_downloading`, `checking`, `moving`, `errored`. Unknown value -> `all`. |
| `category` | string | absent = any | `""` = uncategorized only; otherwise the category **and its subcategories** (`belongsToCategory`: equal or prefix `cat/`) |
| `tag` | string | absent = any | `""` = untagged only; else torrents having that tag (trimmed) |
| `hashes` | string | absent | `\|`-separated ids to restrict to |
| `private` | bool | absent = any | `true` private only / `false` non-private only |
| `includeFiles` | bool | false | add `files` (2.11.8) |
| `includeTrackers` | bool | false | add `trackers` |
| `sort` | string | none | any key of the torrent object; must exist in the first result object else `400 'sort' parameter is invalid`. Compares bool/double/float/int/longlong/string (strings: plain code-point `<`, case-sensitive). |
| `reverse` | bool | false | descending |
| `limit` | int | 0 | <= 0 = unlimited |
| `offset` | int | 0 | negative = from end (`size + offset`) |

Filter semantics:
| filter | Match |
|---|---|
| `downloading` | state in {downloading, metaDL, forcedMetaDL, stalledDL, checkingDL, stoppedDL, queuedDL, forcedDL} |
| `seeding` | state in {uploading, stalledUP, checkingUP, queuedUP, forcedUP} |
| `completed` | state in {uploading, stalledUP, checkingUP, stoppedUP, queuedUP, forcedUP} |
| `stopped` | torrent is stopped |
| `running` | not stopped |
| `active` | dlspeed > 0 or upspeed > 0 |
| `inactive` | not active |
| `stalled` | stalledUP or stalledDL |
| `stalled_uploading` | stalledUP |
| `stalled_downloading` | stalledDL |
| `checking` | checkingUP, checkingDL, checkingResumeData |
| `moving` | moving |
| `errored` | error or missingFiles |

Response: JSON array of torrent objects (empty array if none). Order without `sort` = session iteration order (unspecified).

#### 6.15.4 Read-only torrent endpoints

| Endpoint | Params | Response | Errors |
|---|---|---|---|
| `torrents/count` | - | text: number of torrents | |
| `torrents/properties` | `hash` | object (below) | `404` unknown hash |
| `torrents/trackers` | `hash` | array: 3 pseudo-rows + real trackers (below) | `404` |
| `torrents/webseeds` | `hash` | array of `{url}` | `404` |
| `torrents/files` | `hash`, `indexes` (optional, `\|`-separated) | array (below); `[]` if no metadata | `404`; `409` `"%1" is not a valid file index.` / `Index %1 is out of bounds.` |
| `torrents/pieceStates` | `hash` | array of ints per piece: `0` not downloaded, `1` requested/downloading, `2` downloaded | `404` |
| `torrents/pieceHashes` | `hash` | array of lowercase hex piece hashes (`[]` without metadata) | `404` |
| `torrents/pieceAvailability` | `hash` | array of ints: number of peers having each piece (2.15.1) | `404` |
| `torrents/uploadLimit` | `hashes` | object `{<id as sent>: limit}`; limit B/s (0 = unlimited), `-1` if torrent not found. `all` is **not** expanded here | |
| `torrents/downloadLimit` | `hashes` | same for download | |
| `torrents/categories` | - | object `{<name>: category object}` (6.14.1) | |
| `torrents/tags` | - | array of tag strings | |
| `torrents/export` | `hash` | `.torrent` bytes, `Content-Type: application/x-bittorrent`, `Content-Disposition: attachment; filename="<hash>.torrent"` | `404`; `409` `Unable to export torrent file. Error: %1` |
| `torrents/SSLParameters` | `hash` | `{ssl_certificate, ssl_private_key, ssl_dh_params}` PEM strings | `404` |

`torrents/properties` object:
| Key | Type | Meaning |
|---|---|---|
| `infohash_v1`, `infohash_v2`, `hash`, `name` | string | as in torrent object |
| `time_elapsed` | int | active time s |
| `seeding_time` | int | seeding time s |
| `eta` | int | as torrent object |
| `nb_connections` | int | connections |
| `nb_connections_limit` | int | connection limit |
| `total_downloaded` | int | all-time downloaded |
| `total_downloaded_session` | int | this session |
| `total_uploaded` | int | all-time uploaded |
| `total_uploaded_session` | int | this session |
| `dl_speed` | int | current B/s |
| `dl_speed_avg` | int | `total_downloaded / (time_active - seeding_time)`; -1 if duration 0 |
| `up_speed` | int | current B/s |
| `up_speed_avg` | int | `total_uploaded / time_active`; -1 if 0 |
| `dl_limit` | int | B/s, **-1 = unlimited** (note: differs from torrents/info where 0 = unlimited) |
| `up_limit` | int | B/s, -1 = unlimited |
| `total_wasted` | int | |
| `seeds` | int | connected seeds |
| `seeds_total` | int | swarm seeds |
| `peers` | int | connected leechers |
| `peers_total` | int | swarm leechers |
| `share_ratio` | number | -1 if infinite |
| `popularity` | number | -1 if infinite |
| `availability` | number | distributed copies (2.15.1) |
| `reannounce` | int | s to next announce |
| `total_size` | int | total size (all files) |
| `pieces_num`, `piece_size`, `pieces_have` | int | |
| `created_by` | string | |
| `is_private` | bool | deprecated alias (false without metadata) |
| `private` | bool \| null | null without metadata |
| `addition_date` | int | epoch s |
| `last_seen` | int | last seen complete, epoch s / -1 |
| `completion_date` | int | -1 if incomplete |
| `creation_date` | int | -1 unknown |
| `save_path` | string | |
| `download_path` | string | |
| `comment` | string | |
| `has_metadata` | bool | |
| `progress` | float | |

`torrents/trackers` row:
| Key | Type | Meaning |
|---|---|---|
| `url` | string | tracker URL; pseudo rows `** [DHT] **`, `** [PeX] **`, `** [LSD] **` |
| `tier` | int | tier (0-based); pseudo rows `-1` |
| `updating` | bool | announce in progress (real trackers) |
| `status` | int | `0` Disabled (pseudo rows only: DHT/PeX/LSD globally disabled or torrent private), `1` Not contacted yet, `2` Working, `4` Not working, `5` Tracker error, `6` Unreachable. (`3` "Updating" is no longer emitted; see `updating`.) Pseudo rows: `2` if enabled and torrent not private |
| `msg` | string | last tracker message; pseudo rows: `This torrent is private` for private torrents else `""` |
| `num_peers` | int | peers reported (-1 unknown); pseudo rows 0 |
| `num_seeds` | int | seeds reported by tracker; pseudo rows: number of connected (non-connecting) seed peers obtained from that source |
| `num_leeches` | int | leechers; pseudo rows: connected leechers from that source |
| `num_downloaded` | int | completed count from scrape (-1 unknown); pseudo rows 0 |
| `next_announce` | int | epoch s of next announce (earliest among endpoints) — real trackers only (2.13.0) |
| `min_announce` | int | epoch s of min re-announce time — real trackers only |
| `endpoints` | array | real trackers only: per local listen endpoint `{name, updating, status, msg, bt_version (1 or 2), num_peers, num_seeds, num_leeches, num_downloaded, next_announce, min_announce}`; `name` is the local endpoint address string |

`torrents/files` row:
| Key | Type | Meaning |
|---|---|---|
| `index` | int | file index |
| `name` | string | path inside the torrent with `/` separators (includes root folder) |
| `size` | int | bytes |
| `progress` | float | 0..1 |
| `priority` | int | `0` Do not download, `1` Normal, `6` High, `7` Maximum |
| `is_seed` | bool | **only on the first element**: whether the torrent is finished/seeding |
| `piece_range` | [int, int] | first and last piece index (inclusive) |
| `availability` | float | fraction of the file available in the swarm (`fetchAvailableFileFractions`) |

#### 6.15.5 `torrents/add` (POST, `multipart/form-data` for files; urlencoded OK for URLs only)

| Param | Type | Default | Meaning |
|---|---|---|---|
| (file parts) | files | - | any multipart part with a `filename` = a `.torrent` file (keyed by filename; duplicates collide) |
| `urls` | string | - | `\n`-separated: magnet links, bare v1 (40 hex / 32 base32) or v2 (64 hex) info-hashes, http(s)/ftp URLs (downloaded, max size = "torrent file size limit" pref, via proxy if "use proxy for general purposes"), local paths or `file://` URLs. Each trimmed; empty lines skipped. |
| `savepath` | string | `""` | save path (trimmed); empty = default/category |
| `downloadPath` | string | `""` | incomplete path |
| `useDownloadPath` | bool | unset | use incomplete path; unset = default |
| `category` | string | `""` | category name; if it does not exist it is **created** (with missing parent categories); if the name is invalid the torrent is added uncategorized |
| `tags` | string | - | `,`-separated |
| `skip_checking` | bool | false | skip hash check |
| `stopped` | bool | unset | add stopped (unset = global "Do not start the download automatically") |
| `addToTopOfQueue` | bool | unset | queue position top (unset = global default) |
| `forced` | bool | false | add in forced (ignore queue) mode |
| `stopCondition` | string | unset | `None`, `MetadataReceived`, `FilesChecked` (Qt enum key names; invalid -> `None`) |
| `contentLayout` | string | unset | `Original`, `Subfolder`, `NoSubfolder` (invalid -> `Original`) |
| `rename` | string | `""` | custom torrent name |
| `upLimit` | int | -1 | B/s |
| `dlLimit` | int | -1 | B/s |
| `ratioLimit` | float | -2 | |
| `seedingTimeLimit` | int | -2 | minutes |
| `inactiveSeedingTimeLimit` | int | -2 | minutes |
| `shareLimitAction` | string | `Default` | `Default`/`Stop`/`Remove`/`RemoveWithContent`/`EnableSuperSeeding` |
| `autoTMM` | bool | unset | Automatic Torrent Management; unset = global default |
| `sequentialDownload` | bool | false | |
| `firstLastPiecePrio` | bool | false | |
| `filePriorities` | string | - | `,`-separated ints (0,1,6,7). Only allowed with exactly one URL whose metadata is already cached via `fetchMetadata`/`parseMetadata`; count must equal file count (2.11.9) |
| `downloader` | string | - | name of an installed search plugin used to download non-magnet URLs (2.13.1); invalid -> `400` |
| `ssl_certificate`, `ssl_private_key`, `ssl_dh_params` | string | - | PEM SSL parameters for SSL torrents |

Processing:
1. For each URL: look up cached metadata (`fetchMetadata`/`parseMetadata` caches, keyed by source URL -> infohash -> descriptor). If cached metadata exists -> add directly (with `filePriorities` if given). Else if `downloader` given and URL is not a magnet/hash -> download via search plugin's `nova2dl.py`, counted as **pending**. Else -> `AddTorrentManager::addTorrent(url)`: magnet/hash -> added (counted success with its id); http URL -> async download (counted **pending**); failure -> **failure**. Cache entries for the URL are then dropped.
2. Each uploaded file -> parsed; invalid -> `415` `Error: '<filename>' is not a valid torrent file.` (aborts the request); add success -> id recorded; duplicate/other failure -> failure count.
3. Response (2.14.0): if at least one success or pending -> `{"success_count": n, "failure_count": n, "pending_count": n, "added_torrent_ids": ["<id>", ...]}`, status `202` if `pending_count > 0` else `200`. If nothing succeeded and nothing pending -> `409` (empty body "Conflict").
Errors `400`: `Cannot specify filePriorities when adding multiple torrents`, `Cannot specify filePriorities when uploading torrent files`, `Priority must be an integer`, `Priority is not valid`, `` `downloader` must be a valid search plugin ``, `Length of filePriorities must equal number of files in torrent`, `` `filePriorities` may only be specified when metadata has already been fetched ``.

#### 6.15.6 Metadata preview endpoints (2.11.9)

Per-session caches: `torrentSourceCache` (source string -> infohash) and `torrentMetadataCache` (TorrentID -> TorrentDescriptor). Metadata-only downloads use the session's "download metadata" facility (magnet resolved without adding the torrent).

- `torrents/fetchMetadata` (POST): params `source` (required; trimmed, percent-decoded; magnet, bare hash or http(s) URL), `downloader` (optional search plugin).
  - Empty source -> `400 Must specify URI or hash`. Invalid downloader -> `400 downloader must be a valid search plugin`.
  - Magnet/hash: if metadata cached -> `200` with full info object; else if the torrent is already in the transfer list with metadata -> `200` info from it; else if a metadata download is already running (`isKnownTorrent`) -> `202` `{infohash_v1, infohash_v2, hash}`; else start metadata download -> `202` same object (failure to start -> `400 Unable to download metadata for '%1'`).
  - http(s) URL: starts (once) a download of the .torrent (or plugin download); responds `202 {}`. When finished: .torrent is parsed and cached; a redirect to magnet starts a metadata download. Client polls again; once cached the infohash path answers `200`.
  - Other -> `400 Unable to parse '%1'`.
  - Info object: `{infohash_v1, infohash_v2, hash, info: {files: [{path, length}], length, name, piece_length, pieces_num, private}, trackers: [{url, tier}], webseeds: [url], created_by, creation_date, comment}`.
- `torrents/parseMetadata` (POST multipart): uploaded .torrent files -> caches each and returns an **array** of info objects in upload order (2.13.0). None -> `400 Must specify torrent file(s)`; invalid -> `415 '%1' is not a valid torrent file.`
- `torrents/saveMetadata` (GET/POST): `source` -> returns the cached metadata as a .torrent file (`application/x-bittorrent`, `<id>.torrent`). Unknown source -> `404`; not yet available -> `409 Metadata is not yet available`; export failure -> `409 Unable to export torrent metadata. Error: %1`.
- When a metadata download finishes, the cached descriptor is completed (hybrid torrents also matched via their v1 id).

#### 6.15.7 Mutating torrent endpoints (all POST)

| Endpoint | Params (required unless noted) | Behaviour | Errors / response |
|---|---|---|---|
| `torrents/stop` | `hashes` (`all` ok) | stop | `200` |
| `torrents/start` | `hashes` | start (auto-managed) | `200` |
| `torrents/delete` | `hashes`, `deleteFiles` (bool) | remove torrent; `deleteFiles=true` -> also remove content, using the Advanced pref "Torrent content removing mode" (`Delete` default, or `MoveToTrash`); `.parts` file always deleted | `200` |
| `torrents/recheck` | `hashes` | force recheck | `200` |
| `torrents/reannounce` | `hashes`, `urls` (optional `\|`-separated tracker URLs, percent-decoded) | no urls: force reannounce to all trackers **and** DHT; with urls: reannounce only to matching trackers (2.11.10) | `200` |
| `torrents/setLocation` | `hashes`, `location` | `mkpath(location)`; for each torrent: **disables Auto TMM**, sets save path (moves data). Log `WebUI Set location: moving "%1", from "%2" to "%3"` | `400 Save path cannot be empty`; `409 Cannot make save path` |
| `torrents/setSavePath` | `id` (`\|` ids/`all`), `path` | mkpath + writable check; applies only to torrents **without** Auto TMM | `400 Save path cannot be empty`; `409 Cannot create target directory`; `403 Cannot write to directory` |
| `torrents/setDownloadPath` | `id`, `path` (empty = disable incomplete path) | same checks (skipped for empty); only non-AutoTMM torrents | `409`/`403` as above |
| `torrents/rename` | `hash`, `name` | trims; CR/LF sequences replaced by a space | `409 Incorrect torrent name` (empty); `404` |
| `torrents/setComment` | `hashes`, `comment` (trimmed) | sets comment (2.12.1) | `204` |
| `torrents/setCategory` | `hashes`, `category` (`""` = remove) | category must exist; may disable Auto TMM if pref "when category changed: switch to manual" | `409 Incorrect category name` (non-existent) |
| `torrents/createCategory` | `category`, `savePath` (opt), `downloadPathEnabled` (opt bool), `downloadPath` (opt) | creates category; missing parent categories (`a` for `a/b`) are auto-created with default options; download path option only set when `downloadPathEnabled` present (`true` + `downloadPath`, or `false` = explicitly disabled) | `400 Category cannot be empty`; `409 Incorrect category name` (invalid name: regex below); `409 Unable to create category` (exists) |
| `torrents/editCategory` | `category`, `savePath`, `downloadPathEnabled` (opt), `downloadPath` (opt) | **replaces** category options with the given save/download path (share-limit fields of the category reset to defaults); no-op if equal (2.15.1) | `400 Category cannot be empty`; `404 Category does not exist` |
| `torrents/removeCategories` | `categories` (`\n`-separated) | removes each category and its subcategories; torrents in them move to the parent category | `200` |
| `torrents/addTags` | `hashes`, `tags` (`,`) | adds tags; unknown tags are created globally; tags trimmed; tags containing `,` or empty are invalid | `200` |
| `torrents/setTags` | `hashes`, `tags` (`,`) | makes each torrent's tag set exactly `tags` | `204` |
| `torrents/removeTags` | `hashes`, `tags` (optional) | removes listed tags; if none listed removes **all** tags from the torrents | `200` |
| `torrents/createTags` | `tags` | creates global tags | `200` |
| `torrents/deleteTags` | `tags` | deletes global tags (removed from all torrents) | `200` |
| `torrents/setAutoManagement` | `hashes`, `enable` (bool, default false) | toggles Auto TMM (enabling moves files to category paths) | `200` |
| `torrents/toggleSequentialDownload` | `hashes` | toggle | `200` |
| `torrents/toggleFirstLastPiecePrio` | `hashes` | toggle | `200` |
| `torrents/setForceStart` | `hashes`, `value` (bool) | `true` -> start in Forced mode; `false` -> start in AutoManaged mode (note: also starts stopped torrents) | `200` |
| `torrents/setSuperSeeding` | `hashes`, `value` (bool) | | `200` |
| `torrents/setShareLimits` | `hashes`, `ratioLimit`, `seedingTimeLimit`, `inactiveSeedingTimeLimit`, `shareLimitAction` (all required since 2.12.0) | numbers per 6.8; action string (invalid -> `Default`) | `200` |
| `torrents/setUploadLimit` | `hashes`, `limit` (B/s; 0 -> -1) | per-torrent limit | `200` |
| `torrents/setDownloadLimit` | `hashes`, `limit` | | `200` |
| `torrents/increasePrio` / `decreasePrio` / `topPrio` / `bottomPrio` | `hashes` (`all` is **not** special here; ids parsed directly) | move in queue | `409 Torrent queueing must be enabled` |
| `torrents/filePrio` | `hash`, `id` (`\|`-separated file indexes), `priority` (0/1/6/7) | sets file priorities (only applies if changed) | `400 Priority must be an integer` / `Priority is not valid` / `File IDs must be integers`; `404`; `409 Torrent's metadata has not yet downloaded` / `File ID is not valid` |
| `torrents/renameFile` | `hash`, `oldPath`, `newPath` (paths inside torrent, `/`) | rename a file (may move between folders) | `409 File name has invalid characters`; `404`; `409 <error message>` (e.g. path exists / not found) |
| `torrents/renameFolder` | `hash`, `oldPath`, `newPath` | rename a folder | `409 Folder name has invalid characters`; `404`; `409 <error>` |
| `torrents/addTrackers` | `hash` (`\|`-list or `all`, 2.11.9), `urls` | `urls` split by `\n`; blank lines increment the tier (first group tier 0) | `204` |
| `torrents/editTracker` | `hash`, `url` (original; renamed from `origUrl` in 2.13.0), `newUrl` (opt), `tier` (opt 0..255) | replace URL and/or tier, then force reannounce; no change -> no-op | `400 Must specify at least one of [newUrl, tier]` / `tier must be an integer` / `tier must be between 0 and 255` / `New tracker URL is invalid`; `404`; `409 New tracker URL already exists` / `Tracker not found`; success `204` |
| `torrents/removeTrackers` | `hash` (`\|`-list, `all`, or legacy `*`), `urls` (`\|`, percent-decoded) | remove trackers | `204` |
| `torrents/addPeers` | `hashes`, `peers` (`\|`-separated `ip:port` / `[ipv6]:port`) | connect peers | `400 No valid peers were specified`; response `{<id>: {added: n, failed: n}}` |
| `torrents/addWebSeeds` | `hash`, `urls` (`\|`) | each URL percent-decoded, strict-parsed, scheme must be http/https/ftp | `400 "%1" is not a valid URL` / `URL scheme must be one of [http, https, ftp]`; `404` |
| `torrents/editWebSeed` | `hash`, `origUrl`, `newUrl` | replace a web seed | `400` as above; `404`; `409 "%1" is not an existing URL` |
| `torrents/removeWebSeeds` | `hash`, `urls` (`\|`) | | `400`/`404` |
| `torrents/setSSLParameters` | `hash`, `ssl_certificate`, `ssl_private_key`, `ssl_dh_params` | set SSL torrent params | `404`; `415` if invalid |

Category name validity (`Session::isValidCategoryName`): empty, or matches `^([^\\\/]|[^\\\/]([^\\\/]|\/(?=[^\/]))*[^\\\/])$` — no backslashes, no leading/trailing `/`, no `//`. `/` separates subcategories.

### 6.16 `rss/*`

Item paths use `\` as the folder separator (RSS session path separator), e.g. `Folder\Sub\Feed name`; the root folder is `""`.

| Endpoint | Method | Params | Behaviour / response | Errors |
|---|---|---|---|---|
| `rss/addFolder` | POST | `path` | create folder (parent must exist) | `409` session error (e.g. item exists, parent missing) |
| `rss/addFeed` | POST | `url`, `path` (empty -> use the URL as name), `refreshInterval` (opt, seconds, negative -> 0 = use global) | add feed | `409` e.g. `RSS feed with given URL already exists: %1.` |
| `rss/setFeedURL` | POST | `path`, `url` | change feed URL (no-op if same) and trigger an immediate refresh | `409` `Feed doesn't exist: %1.` / URL already exists |
| `rss/setFeedRefreshInterval` | GET/POST | `path`, `refreshInterval` (int >= 0 seconds; 0 = global default) | per-feed refresh interval; `204` | `400 Invalid 'refreshInterval' value`; `409 Feed doesn't exist: %1.` |
| `rss/removeItem` | POST | `path` | delete feed/folder (recursive) | `409` `Cannot delete root folder.` / `Item doesn't exist: %1.` |
| `rss/moveItem` | POST | `itemPath`, `destPath` | move/rename item | `409` `Cannot move root folder.` / `Item doesn't exist: %1.` / `Can't move a folder into itself or its subfolders.` / dest exists etc. |
| `rss/items` | GET/POST | `withData` (bool, default false) | tree (below) | |
| `rss/markAsRead` | POST | `itemPath`, `articleId` (opt) | with `articleId`: mark that article (by GUID) of the feed read; else mark whole item (feed or folder recursively) read | unknown item -> `204` silently |
| `rss/refreshItem` | POST | `itemPath` | refresh feed / all feeds in folder; unknown item ignored | `200` |
| `rss/setRule` | POST | `ruleName`, `ruleDef` (JSON object) | create or replace auto-download rule (name from `ruleName`) | `200` (invalid JSON -> rule with defaults) |
| `rss/renameRule` | POST | `ruleName`, `newRuleName` | rename | `200` (no error if missing) |
| `rss/removeRule` | POST | `ruleName` | delete | `200` |
| `rss/rules` | GET/POST | - | `{<rule name>: <rule object>}` | |
| `rss/matchingArticles` | GET/POST | `ruleName` | `{<feed name>: [article title, ...]}` for each of the rule's feeds that has matching articles (uses `rule.matches()`, including smart-episode filter state) | |

`rss/items` tree: a folder is an object `{<child name>: <child>}`; a feed is an object:
| Key | Type | When |
|---|---|---|
| `uid` | string | always (`{uuid}` with braces) |
| `url` | string | always |
| `refreshInterval` | int (s) | only if a per-feed interval > 0 is set |
| `title` | string | withData |
| `lastBuildDate` | string | withData |
| `isLoading` | bool | withData |
| `hasError` | bool | withData |
| `articles` | array | withData: each article is its stored data map: `id`, `date` (RFC 2822 string), `title`, `author`, `description`, `torrentURL`, `link`, `isRead`, plus any unrecognised feed-item XML elements under their element name |

Rule object (`rss/rules`, `ruleDef`):
| Key | Type | Default when parsing | Meaning |
|---|---|---|---|
| `enabled` | bool | true | rule enabled |
| `priority` | int | 0 | rule order (lower = evaluated earlier) |
| `useRegex` | bool | false | `mustContain`/`mustNotContain` are regexes |
| `mustContain` | string | "" | wildcard (`*`, `?`, space = AND, `\|` = OR) or regex |
| `mustNotContain` | string | "" | same |
| `episodeFilter` | string | "" | episode filter, must match `(^\d{1,4})x(.*;$)`, e.g. `1x2-6;8;12-;` |
| `affectedFeeds` | array<string> (or single string) | [] | feed URLs |
| `lastMatch` | string | "" | RFC 2822 date of last match |
| `ignoreDays` | int | 0 | ignore subsequent matches for N days |
| `smartFilter` | bool | false | smart episode filter |
| `previouslyMatchedEpisodes` | array<string> (or string) | [] | episodes already downloaded (smart filter) |
| `torrentParams` | object | {} | add-torrent params (keys below); preferred |
| `addPaused` | bool \| null | | deprecated mirror of `torrentParams.stopped` (only read if `torrentParams` absent) |
| `torrentContentLayout` | string \| null | | deprecated (`Original`/`Subfolder`/`NoSubfolder`) |
| `savePath` | string | | deprecated mirror of `torrentParams.save_path` (non-empty -> Auto TMM off) |
| `assignedCategory` | string | | deprecated mirror of `torrentParams.category` |
| (legacy) `createSubfolder` | bool | | very old format, mapped to content layout |

`torrentParams` (`serializeAddTorrentParams`) keys: `category`, `tags` (array), `save_path`, `download_path`, `operating_mode` (`AutoManaged`/`Forced`), `skip_checking`, `upload_limit`, `download_limit`, `seeding_time_limit`, `inactive_seeding_time_limit`, `share_limit_action`, `ratio_limit`, `ssl_certificate`, `ssl_private_key`, `ssl_dh_params`, and only when set: `add_to_top_of_queue`, `stopped`, `stop_condition` (`None`/`MetadataReceived`/`FilesChecked`), `content_layout`, `use_auto_tmm`, `use_download_path`.

### 6.17 `search/*`

Search requires a usable Python 3 (`Utils::ForeignApps::pythonInfo().isValid()`). Max 5 concurrent **running** searches per session. Search ids are random ints in `[1, INT_MAX]`. Finished searches stay listed until `search/delete`.

| Endpoint | Method | Params | Response | Errors |
|---|---|---|---|---|
| `search/start` | POST | `pattern` (trimmed), `category` (`all` or plugin category id), `plugins` (`\|`-list; single `all` = all installed, `enabled`/`multi` = enabled ones, case-insensitive) | `{"id": int}` | `409 Python must be installed to use the Search Engine.`; `409 Unable to create more than 5 concurrent searches.` |
| `search/stop` | POST | `id` | cancels if running; `200` | `404` |
| `search/status` | GET/POST | `id` (opt; 0/absent = all) | array of `{id, status: "Running"\|"Stopped", total: int}` | `404` unknown id |
| `search/results` | GET/POST | `id`, `limit` (<=0 unlimited), `offset` (negative = from end) | `{status: "Running"\|"Stopped", results: [...], total: int}` | `404`; `409 Offset is out of range` |
| `search/delete` | POST | `id` | cancel + forget; `200` | `404` |
| `search/downloadTorrent` | GET/POST | `torrentUrl`, `pluginName` | magnet -> added directly; else plugin's download routine (nova2dl.py) then added with default params; `200` | |
| `search/plugins` | GET/POST | - | array of plugin objects | |
| `search/installPlugin` | POST | `sources` (`\|`-list of URLs or local `.py` paths) | async install; `200` | |
| `search/uninstallPlugin` | POST | `names` (`\|`) | `200` | |
| `search/enablePlugin` | POST | `names` (`\|`), `enable` (bool, default false) | `200` | |
| `search/updatePlugins` | POST | - | async check for updates against the official plugin index, then updates each outdated plugin (logs `All plugins are already up to date.`, `Updating %1 plugins`, `Updating plugin %1`, `Failed to check for plugin updates: %1`); `200` immediately | |

Result object: `{fileName, fileUrl, fileSize (int bytes; 0 if unparsable), nbSeeders (int, -1 if unparsable/negative), nbLeechers (int, -1 likewise), engineName (plugin name resolved from siteUrl), siteUrl, descrLink, pubDate (epoch s; -1 if absent or <= 0)}`.

Plugin object: `{name, version (string), fullName, url, supportedCategories: [{id, name}], enabled: bool}`. `supportedCategories` always starts with `{id: "all", name: "All categories"}` followed by the plugin's categories sorted case-insensitively. Category display names: `all` "All categories", `anime` "Anime", `books` "Books", `games` "Games", `movies` "Movies", `music` "Music", `pictures` "Pictures", `software` "Software", `tv` "TV shows".

### 6.18 `torrentcreator/*` (shared `TorrentCreationManager`, not per session)

Manager limits: max 256 tasks retained (`TorrentCreator/Manager/MaxTasks`); when full, finished tasks are evicted oldest-completion first; if still full -> `409 Too many active tasks`. Worker threads: `TorrentCreator/Manager/NumThreads` (default 1) — tasks run one at a time by default. Task ids are UUIDs without braces.

#### `torrentcreator/addTask` (POST)
| Param | Type | Default | Meaning |
|---|---|---|---|
| `sourcePath` | string | required | file or folder to hash |
| `torrentFilePath` | string | `""` | where to write the .torrent; empty -> saved to a temporary file |
| `private` | bool | false | private flag |
| `format` | string | `hybrid` | libtorrent 2.x only: `v1`, `v2`, `hybrid` (anything else -> hybrid) |
| `optimizeAlignment` | bool | true | libtorrent 1.2 only |
| `paddedFileSizeLimit` | int | -1 | libtorrent 1.2 only |
| `pieceSize` | int | 0 | bytes; 0 = auto |
| `comment` | string | "" | |
| `source` | string | "" | `info.source` field |
| `trackers` | string | "" | `\|`-separated, each percent-decoded; empty entries start a new tier |
| `urlSeeds` | string | "" | `\|`-separated web seeds (empty entries ignored) |
| `startSeeding` | bool | `true` if `torrentFilePath` empty, else `false` | after creation, add the torrent for seeding: not stopped, layout `Original`, save path = parent of source, skip checking, stop condition None, Auto TMM off, no download path |
Response `{"taskID": "<uuid>"}`.

#### `torrentcreator/status` (GET/POST)
Param `taskID` (optional; empty = all tasks; unknown -> `404`). Array of:
| Key | Presence | Meaning |
|---|---|---|
| `taskID`, `sourcePath`, `pieceSize`, `private`, `timeAdded`, `status` | always | `status`: `Queued`, `Running`, `Finished`, `Failed`; `timeAdded` = `QDateTime::toString()` (Qt default text format, e.g. `Mon Oct 13 12:34:56 2026`) |
| `format` | lt2 | `v1`/`v2`/`hybrid` |
| `optimizeAlignment`, `paddedFileSizeLimit` | lt1.2 | |
| `comment`, `torrentFilePath`, `source`, `trackers` (array), `urlSeeds` (array) | if non-empty | |
| `timeStarted`, `timeFinished` | if set | Qt text date |
| `errorMessage` | finished + failed | e.g. `Failed to start seeding.` or creation error |
| `pieceSize` | finished OK | overwritten with the actual piece size chosen |
| `progress` | running | int 0..100 |

#### `torrentcreator/torrentFile` (GET/POST)
`taskID` -> the .torrent bytes (`application/x-bittorrent`, filename `<taskID>.torrent`). `404` unknown; `409 Torrent creation is still unfinished.` / `Torrent creation failed.` / file read error.

#### `torrentcreator/deleteTask` (POST)
`taskID` -> remove the task from the manager (destroying it requests interruption of a running creator). `404` unknown. `200`.

### 6.19 WebAPI changelog (recent, from `WebAPI_Changelog.md`)

| Version | Changes |
|---|---|
| 2.15.1 | `app/processInfo` (`launch_time`); `sync/torrentPeers` adds `host_name`; `torrents/pieceAvailability`; `torrents/properties` adds `availability`; `torrents/editCategory` no error when unchanged, `404` for unknown category |
| 2.15.0 | `sync/maindata` drops `use_subcategories` (always on); Basic auth accepted |
| 2.14.1 | `app/rotateAPIKey`, `app/deleteAPIKey` |
| 2.14.0 | unknown endpoint -> `404 Endpoint does not exist`; `auth/login` invalid credentials -> `401`; `torrents/add` returns `success_count`/`pending_count`/`failure_count`/`added_torrent_ids`, `202` when pending, `409` when all fail |
| 2.13.1 | `downloader` param for `torrents/add` and `torrents/fetchMetadata`; `clientdata/load` + `clientdata/store` |
| 2.13.0 | `torrents/trackers` adds `next_announce`, `min_announce`, `endpoints`; statuses `5` (Tracker error) and `6` (Unreachable); `torrents/editTracker` gains `tier`, always `204`, `origUrl` renamed `url`; `sync/torrentPeers` adds `i2p_dest`; `torrents/parseMetadata` returns an array |
| 2.12.1 | `torrents/setComment` |
| 2.12.0 | `share_limit_action` torrent field; `torrents/setShareLimits` requires `shareLimitAction` |
| 2.11.10 | categories serialize `download_path` as `null` instead of omitting; `torrents/reannounce` accepts a tracker list (changelog calls it `trackers`; the code reads param `urls`) |
| 2.11.9 | `torrents/fetchMetadata`, `torrents/parseMetadata`, `torrents/saveMetadata`; `torrents/add` accepts cached metadata and `filePriorities`; `addTrackers`/`removeTrackers` accept `all` and `\|`-lists |
| 2.11.8 | `204 No Content` for empty responses (some endpoints still `200`); `torrents/info` `includeFiles`; `app/getDirectoryContent` `withMetadata` |
| 2.11.7 | maindata `has_tracker_warning`, `has_tracker_error`, `has_other_announce_error` |
| 2.11.6 | `setPreferences` share-limit "enabled"/value pairs: send only one of each |

### 6.20 Parity / implementation notes for the Rust core

- The macOS app does not need to ship the HTML WebUI, but the WebAPI is a de-facto standard used by third-party tools (Sonarr/Radarr/Prowlarr, mobile remotes, browser extensions); exposing a compatible `/api/v2` surface (at least `auth`, `app/version|webapiVersion|preferences|setPreferences|defaultSavePath|buildInfo`, `torrents/info|properties|files|trackers|add|delete|start|stop|setCategory|createCategory|categories|addTags|setShareLimits|topPrio|bottomPrio|setLocation|recheck|reannounce|setForceStart|filePrio`, `transfer/info`, `sync/maindata`) gives instant ecosystem compatibility. Report `webapiVersion` truthfully if a subset is implemented.
- Quirks clients rely on: string `"Ok."`-style bodies are **gone** (empty bodies now); `dl_limit` is `0`=unlimited in `info` but `-1` in `properties`; `priority` 0 means "not queued"; `eta` 8640000 = infinity; `ratio` -1 = infinity; `private` null without metadata; `is_seed` only on first file; `tags` is a `", "`-joined string in torrent objects but `,`-separated in params; categories use `savePath` (camel) next to `download_path` (snake); log timestamps are seconds.
- Per-session state (sync snapshots, search jobs, metadata caches) must be scoped to the session, and API-key "sessions" persist forever (keyed by the key).
- `sync/maindata` diff correctness depends on: snapshot-per-session, rid wrap at 1,000,000, cumulative change buffer until the client acknowledges, and full-update fallback on any mismatch.
