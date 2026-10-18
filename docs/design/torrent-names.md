# Torrent names and the cards view

qBit NeoRS extension, not qBittorrent parity. Release names such as
`Movie.Name.2021.2160p.WEB-DL.DDP5.1.Atmos.HEVC-GROUP` mix a title with
technical information. The app splits them with editable regular-expression
rules: the transfer list shows the title semibold and the rest in regular
weight, and a second view mode shows torrents as cards with the title on top
and the technical information as tags in fixed styles. The rules cover
video, music, software, games, ROM sets, audiobooks, books, comics and adult
release names, and each torrent gets a content kind shown as a symbol on its
card. Everything is GUI-only (no core or WebAPI change).

## Parser (`Features/TorrentNames/Parsing`, pure, unit-tested)

* A rule is `{id, enabled, name, category, pattern, template, caseSensitive}`.
  Patterns are ICU regular expressions, case-insensitive unless the rule says
  otherwise. The template builds the tag text with `$1`… (`$0` is the whole
  match); empty uses the matched text (`2160p|4K|UHD` → `4K`).
* Rules run in order. A match counts only as a whole token: no letter or digit
  touches it on either side, whatever the separator (dots, underscores,
  spaces, dashes, brackets). The check is done in Swift rather than with
  `\p{L}` look-arounds in every expression, because ICU keeps a copy of those
  sets per expression (5.3 MB for the built-in rules instead of 0.7 MB).
* Each match claims its text: it is masked before the next rule runs, so
  earlier rules win (`ENG Sub` is subtitles, not an English audio track) and
  later rules still match around it (`UHD` claimed as 4K, `BluRay Remux` still
  matches).
* Title: the technical tokens at the very start are skipped (`[Group] `, site
  prefixes); the title is the text from there to the first technical token,
  trimmed of separators and dangling brackets, and unwrapped when it is all in
  brackets (`[Title]`). Dots and underscores become spaces unless the title
  already has spaces; a dot between digits stays (`ubuntu-24.04.2-desktop`).
  If no title is left, the first stretch with letters or digits between tokens
  is used, else the whole name.
* Years: of several years before the first other token only the last one is a
  year (`Blade.Runner.2049.2017` keeps "Blade Runner 2049"); a year at the
  very start is part of the title (`1917.2019…`). Years are 1900–2039.
* A release group after the title (`-GROUP`, trailing `[Group]`, `от Group`)
  counts only after another tag, so `Spider-Man` keeps its "Man".
* Tags are ordered by category (the order below), then by position, without
  duplicates (case and leading zeros ignored: `S1` = `S01`).
* With "Show the year as a tag" off, the year is appended to the title
  (`Title (2021)`).
* Input is capped at 512 UTF-16 units. Results are cached per name
  (`NameParseCache`, 4 096 entries, emptied when the rules or the year option
  change). About 0.21 ms per name with the 181 built-in rules (optimized
  build; 0.14 ms with the earlier 122). Cards parse every listed name once to
  size the grid (see Cards); the list parses only names that are drawn;
  never in a view `body`. The 181 compiled rules take about 0.8 MB.

### Content kind (`ContentKindDetector`)

Each parse result carries a kind, guessed from fixed markers (not the
editable rules, so editing a rule never changes a kind) and the tags. The
first that applies wins:

1. adult: `XXX` (upper case) or a `Studio.YY.MM.DD.` name;
2. repacked or current-console games: FitGirl, DODI, xatab, ElAmigos, scene
   game groups, GOG, `NSP`/`XCI`/`PKG`, `[Switch]`, PS4/PS5, Xbox One/Series;
3. ROM collections: No-Intro, Redump, TOSEC, MAME, `ROMs`, `CHD`, `[!]`,
   `(USA)`/`(Europe)`/`(Japan)`/`(World)`, `(Rev 1)`;
4. audiobooks: `M4B`, audiobook words, `читает`, `Narrated by`, (un)abridged;
5. comics: `CBZ`/`CBR`, manga, comic, webtoon, `c001`;
6. video: resolution, HDR or video codec tags, a video source (not WEB,
   vinyl, CD, Digital or DVD alone), episodes without audio tags (podcasts
   have episodes too), course and documentary words;
7. other games: PS2/PS3/PSP/Vita, Xbox, DLC, `MULTi10`, GOTY;
8. books: EPUB, FB2, MOBI, AZW3, DjVu, eBook words, or a PDF tag without
   platform or version tags ("PDF Editor v12" is software);
9. music: audio tags or music words (Discography, Album, OST, EP, Single,
   podcast, kbps, FLAC, MP3, V0, vinyl);
10. software: platform or version tags, OS and installer words;
11. the file extension (`.mkv`, `.flac`, `.m4b`, `.epub`, `.cbz`, `.nsp`,
    `.chd`, `.iso`/`.dmg`/`.exe` …).

Still unknown, the category name decides (`TorrentNamesModel.contentKind`,
cached per category): words starting with movie, film, series, anime, tv,
course, music, audio, podcast, game, book, audiobook, comic, manga, ROM,
emulat…, software, app, linux, iso, and their Russian forms. With parsing
off only the extension and the category are used. The torrent's file list
is not used: it is loaded only for the selected torrent.

| Kind | Symbol |
|---|---|
| Video | `film` |
| Music (and podcasts) | `music.note` |
| Software (and OS images) | `app.badge` |
| Game | `gamecontroller` |
| ROM Collection | `memorychip` |
| Audiobook | `headphones` |
| Book | `book` |
| Comic (and manga) | `book.pages` |
| Adult | `18.circle` |
| Other | `doc` |

### Safety

`NSRegularExpression` has no timeout. A rule is accepted only after a timed
run (60 ms budget) on realistic names and long adversarial inputs (512 × `a`,
`1`, `a.`, `[a]`, Cyrillic …), using `.reportProgress` so a catastrophic
pattern such as `(a+)+$` is stopped within the budget. Patterns are limited
to 300 characters and tag texts to 80; a template may not use a group the
pattern lacks. Saving in the editor runs the timed check on every enabled
rule that differs from the saved or built-in ones; imported files are limited
to 1 MB and 500 rules, and failing rules arrive switched off; stored rules
that fail are switched off at launch. The parser itself skips the remaining
rules once 25 ms are spent on one name (it does not use `.reportProgress`,
which makes matching about four times slower).

## Categories and their fixed styles (`NameChipStyle`)

Tags are rectangles with 5 pt corners, 18 pt high, SF Pro Rounded 11 pt
medium text, 6 pt side padding, 4 pt apart (rows 4 pt apart). Colors are derived from a system tint per
appearance (light, dark, both Increase Contrast variants): fill = tint at
16 % (dark 26 %, Increase Contrast 24 / 34 %), text = tint mixed with 50 %
black (dark: 35 % white; Increase Contrast 62 / 55 %), so yellow and mint
stay readable. Outlined categories have no fill and a tint border; with
Increase Contrast every tag gets a border. With "Differentiate without color"
each tag also shows its category symbol. Volume, Version and Region are
outlined in the tint of their filled sibling (episodes, platforms,
languages), so related tags read as related. On cards the torrent's category
and tags follow as neutral tags (quaternary fill, secondary text) with their
own icon and color.

| Category | Tint | Style | Symbol | Examples |
|---|---|---|---|---|
| Season & Episode | red | filled | `tv` | S01E02, S01E01–E03, S1–S5, E05, E1–8/10, Complete |
| Volume & Part | red | outlined | `books.vertical` | Vol. 01–12, Vol. 3, Ch. 001–050, #12, Disc 2 |
| Resolution | blue | filled | `aspectratio` | 8K, 4K, 1080p, 720p |
| HDR & Dolby Vision | orange | filled | `sun.max` | Dolby Vision, HDR10+, HDR, HLG |
| Source | purple | filled | `opticaldisc` | Remux, WEB-DL, WEB-DLRip, WEBRip, BluRay, BDRip, HDTV, DVD, CAM, Amazon, Netflix, Vinyl, CD, Digital |
| Video Codec | teal | filled | `film` | HEVC, H.264, AV1, 10-bit |
| Audio | indigo | filled | `speaker.wave.2` | Atmos, TrueHD 7.1, DTS-HD MA 5.1, DD+ 5.1, AAC, FLAC, ALAC, 24/96, MP3 320, V0, 128 kbps |
| Language & Dub | green | filled | `character.bubble` | Дубляж, Dub, MVO, DVO, Multi, Multi 10, RUS, ENG, En,Fr,De, Ru/En |
| Subtitles | yellow | filled | `captions.bubble` | MultiSub, ENG Sub, RUS Sub, Hardsub, Subs |
| Edition | brown | filled | `star` | Director's Cut, Extended, IMAX, Remastered, Deluxe Edition, GOTY, +5 DLC, Discography, EP, Single, Unabridged, Collection |
| Platform | cyan | filled | `desktopcomputer` | x64, ARM64, macOS, Windows, Linux, Portable, GOG, PC, PS5, Switch, Xbox 360, SNES |
| Version | cyan | outlined | `number` | v25.1.0, 3.2.1, Build 17928, Update 5, Rev 1 |
| Region | green | outlined | `globe` | USA, Europe, USA, Europe, Japan, PAL, NTSC-U |
| Format | pink | filled | `doc` | EPUB, FB2, PDF, Kindle, DjVu, CBZ, CBR, M4B, NSP, PKG, CHD, ISO, MP4 |
| Year | gray | filled | `calendar` | 2021, 2019–2020 |
| Release Group | gray | outlined | `person.2` | FLUX, SubsPlease, xatab, FitGirl, No-Intro, Udemy, studios before a short date, publishers |
| Other | mint | outlined | `tag` | REPACK, 2025.01.01, Лицензия, Verified, Beta, Merged, XXX, Documentary, 42 lectures, читает …, catalog numbers |
| Hidden (no tag) | — | — | `eye.slash` | file extensions, CRC32 checksums, site prefixes and tags, the word "audiobook" |

## Built-in rules (`NameRuleDefaults`, 181 rules, names localized)

In order: hidden clean-up (extension, CRC32, `www.site.com - `, `[site.org]`);
resolution; dates and years; season/episode forms (`S01E02`, `S01E01-E03`,
`1x02`, `S01-S03`, `S01`, `Season 2`, `Season 1-5`, `Сезон 2`, `2 сезон`,
`Серии 1-8 из 10`, `[01-12]`, `Ep 05`, anime ` - 05`, Complete); HDR;
sources and streaming services; video codecs; audio formats, channels, bit
depth and sample rate, bitrates; subtitles before audio languages; Russian
dub and voice-over types; editions; platforms and versions; groups named by
a word (`RePack by …`, `от …`); scene flags, book formats, 3D; release groups
in leading brackets, after the last dash and in trailing brackets.

Added for other kinds of content: course platform prefixes (`Udemy - `),
`Studio.YY.MM.DD` studios and dates; volumes, chapters, comic issues and
discs; vinyl, CD and digital sources; MP3 VBR; language counts
(`MULTi10`), ROM language lists (`(En,Fr,De)`) and interface languages
(`[Ru/En]`); music release types (`(EP)`, `[Single]`, `EP`), discography,
collections, (un)abridged, GOTY and DLC; Linux, PlayStation, PSP/Vita,
Switch, Xbox, retro consoles, PC; build, update, revision and plain
`3.2.1` versions; ROM regions, PAL/NTSC; file formats (EPUB, FB2, PDF,
Kindle, DjVu, CBZ, CBR, M4B, disc and game images, video containers);
narrators (`читает …`, `Narrated by …`), publishers, well-known repack and
release groups, ROM set names; verified dumps, ROM flags and set types, `XXX`,
documentaries, lecture counts, catalog numbers. Words that are often part of
titles are matched only where they cannot be (`[Switch]` but not
"The Switch", `(Single)` but not "A Single Man", `[Collection]` but not
"Chillout Collection"); `Vol.`/`v01` and `c001` need two or three digits.

The unit tests check 122 realistic names with their title, content kind and
every tag: 50 movies, series, anime, music, software, games, Linux images
and Russian releases, and 72 more of other kinds (software and Linux images,
music and podcasts, games, ROM sets, audiobooks, books, comics, adult
releases with invented placeholder names, documentaries, courses, TV and
Russian releases, and words that must stay in titles such as "xXx", "A
Single Man", "The Switch", "Genesis"); all ten kinds are covered. Also
every built-in rule's validity and speed, the kind detector (extensions,
tags, whole-token markers, category names), validation, the cache and
import.

Rules persist as JSON in the GUI defaults
(`GUI/TorrentNames/Rules`) only once edited; until then new built-in rules
arrive with updates. Export format:
`{"format": "qbitneors.name-rules", "version": 1, "rules": [...]}`.

## List

The Name cell draws the title range semibold (`TableCellContent.emphasizedRange`)
and the rest regular. The attributed string is built only when the cell's
content or selection changes (cells compare their content), so a refresh of
an unchanged row costs a cache lookup.

## Cards (`MainWindow/TransferCards`)

* `NSCollectionView` with a flow layout: columns of 240–380 pt cards that fill
  the width, 12 pt gaps and insets.
* A card is as tall as its content, and the cards of a grid row take the
  tallest one's height. The title block is pinned to the top and the
  progress block to the bottom, so a shorter card's spare height lies
  between them and the progress bars of a row line up when their cards have
  the same number of status lines. Top block: 12 pt inset; kind symbol
  (13 pt, secondary color, in a 22 pt column), title (Title 3 of the macOS
  type scale, 15 pt semibold, fixed 18 pt lines, one or two lines,
  truncated, full name in the tooltip) and state symbol (in an 18 pt box in
  the top-right corner, centered on the title's first line, always in the
  state color; the title wraps 8 pt before it); 8 pt; tags (none, one or two
  18 pt rows, 4 pt apart). At least 8 pt between the blocks. Bottom block:
  progress bar (4 pt, state color when "Color by state" includes the bar);
  8 pt; status line: the status word in bold SF Pro Rounded (11 pt), tinted
  when "Color by state" includes the status (else the label color), and the
  percent while incomplete on the left, "Ratio 2,40 · 1,39 GiB" (done of
  total while incomplete) on the right; when they do not fit, the ratio gives
  way first, then the percent, then the status is truncated (VoiceOver reads
  all of them). An info line (↓/↑ speeds when non-zero, the ETA while
  downloading) follows only while something is transferring, 2 pt below;
  13 pt lines; 12 pt inset.
* The tinted status word uses `TorrentState.cardTextNSColor`: the state
  color, except where it is below 4.5:1 on the card (#F9F9F9 light,
  #2F2F2F dark): stalled green and blue take the active green and blue, and
  in dark mode queued brown, stopped gray, completed purple and error red are
  lighter (#D29922, #9198A1, #BC8CFF, #FF7B72). Measured contrast 4.5–6.8:1
  (state colors as they were: 2.85–3.07:1 for stalled green/blue, 3.03:1
  dark brown, 3.94:1 dark purple and red). On a gray (unfocused) selection
  (#EEEEEE) green, blue and purple are 4.4–4.5:1.
* Heights (measured on the demo data, `--snapshot-only cards`): 101 pt idle
  and 116 pt transferring with one tag row, 123 / 138 pt with two, +18 pt
  for a two-line title, 75 / 90 pt without tags (before: one height for
  idle and transferring cards, 116 pt with one tag row, 138 pt with two).
  Of the 50 demo cards, 3 columns of 317 pt: 16 are 101 pt (were 116).
* Tags: the name's tags, then the category and tags as neutral tags with
  their icon and color; when the rows are full, the name's last tags give way
  to "+N" so the category and tags stay visible.
* `TransferCardHeights` keeps each card's title, tags and labels with the
  title's single-line width (one line if it fits, else two) and whether it
  is transferring (speeds or an ETA: two status lines), and computes the
  rows from them; a resize redoes only arithmetic (tag widths are cached), a
  sync re-lays out the grid only when a changed torrent's title, tags,
  labels or transferring changed. Sizing the grid parses every listed name once (cached);
  drawing and reconfiguring still touch only realized cards.
* One self-drawn view per card (no subviews), reused by the collection view
  (9 views for a 1 180 × 780 window with the details pane); a new size
  redraws it.
* Selection: tinted fill plus an accent outline in the key window, gray when
  not focused; the context-menu target gets a focus-ring outline.
* Shared with the list through `TransferListModel`: selection, sorting,
  filters, search, the properties pane. Same context menu, double-click and
  Return action, Delete / Shift-Delete, F2 rename, drag to sidebar categories
  and tags, drops of .torrent files and links, ⌘A, Shift/⌘-click and arrow
  keys (the collection view's own). A sync reconfigures only realized cards of
  changed torrents.
* VoiceOver: a card is one element whose label lists the kind, the title,
  each tag with its category, the category and tags, status (once; the
  state symbol is not read separately), progress, ratio, size, speeds and
  ETA; each tag is also a child element ("1080p,
  Resolution", "Anime, Category").
* View › as List (⌘1) / as Cards (⌘2) like Finder, also in the Command Palette
  and as a Settings option; the mode persists (`GUI/TransferList/ViewMode`).
  Switching keeps the scroll position (by the top torrent); a released and
  rebuilt main window restores mode, selection and scroll. Only the chosen
  view exists, so the cards cost nothing in list mode.
* Toolbar: a List / Cards switch (`TransferViewModePicker`, a two-segment
  control with `list.bullet` / `square.grid.2x2`) is the first trailing group,
  like Finder's view picker. Each segment has a VoiceOver label (List, Cards)
  and a tooltip with the command and shortcut ("View as List (⌘1)"); it edits
  the same setting as the View menu, ⌘1/⌘2, the palette and Settings, so they
  stay in sync. Measured in 1 180 pt window captures: the add buttons, the
  window title and the right edge of the search field do not move; the
  system gives the search field 41 pt less (326 → 285 pt), so the command
  group moves 41 pt right (656.5 → 697.5 pt); the switch is 75 pt wide.
  Placed with the add buttons instead, the title would move too and the
  search field shrinks the same. At the minimum width (900 pt) the search
  field becomes its button and nothing overflows, in English and Russian.

## Settings

Interface has two segments (a ninth pane would overflow the Russian toolbar):
Main Window (with "Show torrents: List / Cards") and Torrent Names: "Parse
technical info from names", "List: show titles in semibold", "Cards: show
technical info as tags", "Show the year as a tag", the rules (summary and
"Edit Rules…") and a preview of four current torrents. The editor sheet lists
the rules (on/off, category in its tag style, name, pattern, warning), with
drag to reorder, add, remove, import, export and reset; the selected rule's
name, category, pattern (live validation), tag text and case sensitivity; and
a "Try a name" field parsed with the draft rules. Done validates and saves;
Cancel discards. All rows are in the settings search.

## Measurements

Release build, scratch profiles with generated torrents (sparse data, no
peers, DHT/LSD/PeX/UPnP off), window at its saved size with the details pane,
`--assume-visible`, `scripts/measure-idle.sh` (footprint after 25 s, CPU and
wake-ups over 30 s), two runs each:

| Profile | Mode | Footprint | Peak | CPU ms/s | Wake-ups/s |
|---|---|---|---|---|---|
| 16 torrents | list | 73.3–74.0 MB | 76 MB | 2.4–2.5 | 2.8 |
| 16 torrents | cards | 76.6–76.9 MB | 97 MB | 1.8–1.9 | 2.7 |
| 200 torrents | list | 81.7–81.9 MB | 83 MB | 12.9–13.7 | 2.5–2.8 |
| 200 torrents | cards | 85.3–85.8 MB | 102–104 MB | 12.3–14.0 | 2.8 |
| 200 torrents | list, parsing off | 79.3–79.9 MB | 83 MB | 18.4–19.8 | 2.7 |

(The "parsing off" runs were taken before the boundary change, with the
machine otherwise busier; their CPU is not comparable.) Cards cost about
+3.5 MB settled; the higher peak is transient at start-up.

With the content kinds, 181 rules, cards sized to their content and the
toolbar switch, on a new 200-torrent profile of many kinds (12 categories,
tags, 121 seeding, 58 incomplete, 21 stopped), four runs per mode in two
batches on a busy machine (load average 6–8, so CPU is not comparable with
the table above):

| Profile | Mode | Footprint | Peak | CPU ms/s | Wake-ups/s |
|---|---|---|---|---|---|
| 200 torrents, many kinds | list | 83.3–87.1 MB | 90–91 MB | 15.7–26.9 | 2.7–3.2 |
| 200 torrents, many kinds | cards | 83.7–91.1 MB | 108–147 MB | 17.3–28.9 | 2.8–4.3 |

Cards still cost about +3 MB settled over the list (medians 88.2 vs
85.2 MB). The 59 new rules add about 0.3 MB of compiled expressions; the
rest of the difference to the earlier list figure comes from the profile
(more categories, tags and longer names) and the machine load.
