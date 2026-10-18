//! GUI-only settings of the macOS app. It keeps them in `UserDefaults`
//! under qBittorrent's own ini key names (`GUIPreferenceKey`,
//! `FeatureDefaults`), so most map one to one; this module converts the
//! values to the types the app reads and writes them as a property list:
//! `<profile>/config/gui-preferences.plist` is the app's store for a
//! `--profile` launch, and the same file is imported into the
//! `mk.lex.qbitneors` defaults domain for the default profile.

use super::ini::IniFile;

#[derive(Clone, Debug, PartialEq)]
pub enum PlistValue {
    Bool(bool),
    Int(i64),
    Real(f64),
    String(String),
    Strings(Vec<String>),
}

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int,
    Text,
    TextList,
    /// Qt enum name or number to an integer.
    Enum(&'static [(&'static str, i64)]),
}

use Kind::*;

const FILTER_FORMAT: &[(&str, i64)] = &[("PlainText", 0), ("Wildcards", 1), ("Regex", 2)];

/// (key, type). The key is both the ini key and the defaults key.
const GUI_KEYS: &[(&str, Kind)] = &[
    ("Appearance/ColorScheme", Enum(&[("System", 0), ("Light", 1), ("Dark", 2)])),
    ("Preferences/General/AlternatingRowColors", Bool),
    ("GUI/TransferList/UseTorrentStatesColors", Bool),
    ("GUI/TransferList/ProgressBarFollowsTextColor", Bool),
    ("Preferences/General/HideZeroValues", Bool),
    ("Preferences/General/HideZeroComboValues", Int),
    ("Preferences/Downloads/DblClOnTorDl", Int),
    ("Preferences/Downloads/DblClOnTorFn", Int),
    ("TransferListFilters/HideZeroStatusFilters", Bool),
    ("TransferListFilters/SeparateTrackerStatusFilter", Bool),
    ("Preferences/General/TorrentContentDragEnabled", Bool),
    // Hidden (2) doesn't exist on macOS: minimized is the closest.
    ("GUI/StartUpWindowState", Enum(&[("Normal", 0), ("Minimized", 1), ("Hidden", 1)])),
    ("Preferences/General/ExitConfirm", Bool),
    ("ShutdownConfirmDlg/DontConfirmAutoExit", Bool),
    ("Preferences/Desktop/ShowSpeedInDock", Bool),
    ("Preferences/Desktop/ShowMacOSMenuBarIcon", Bool),
    ("Preferences/General/PreventFromSuspendWhenDownloading", Bool),
    ("Preferences/General/PreventFromSuspendWhenSeeding", Bool),
    ("Preferences/General/StatusbarFreeDiskSpaceDisplayed", Bool),
    ("Preferences/Advanced/updateCheck", Bool),
    ("AddNewTorrentDialog/Enabled", Bool),
    ("AddNewTorrentDialog/TopLevel", Bool),
    ("GUI/ConfirmActions/MergeTrackers", Bool),
    ("Preferences/Advanced/DisableRecursiveDownload", Bool),
    ("GUI/Notifications/Enabled", Bool),
    ("Application/GUI/Notifications/TorrentAdded", Bool),
    ("Preferences/Advanced/confirmRemoveAllTags", Bool),
    ("GUI/ConfirmActions/RemoveTrackerFromAllTorrents", Bool),
    ("GUI/DownloadTrackerFavicon", Bool),
    ("AddNewTorrentDialog/SavePathHistoryLength", Int),
    ("SpeedWidget/Enabled", Bool),
    ("Preferences/General/StatusbarDisplayed", Bool),
    ("GUI/MainWindow/FiltersSidebarVisible", Bool),
    ("Preferences/General/SpeedInTitleBar", Bool),
    ("GUI/RSSWidget/Enabled", Bool),
    ("Preferences/Search/SearchEnabled", Bool),
    ("GUI/Log/Enabled", Bool),
    ("GUI/Log/Types", Int),
    ("Preferences/Downloads/AutoShutDownqBTOnCompletion", Bool),
    ("Preferences/Downloads/AutoSuspendOnCompletion", Bool),
    ("Preferences/Downloads/AutoShutDownOnCompletion", Bool),
    ("Preferences/Downloads/AutoRebootOnCompletion", Bool),
    ("TransferList/UseRegexAsFilteringPattern", Bool),
    ("TransferListFilters/selectedFilterIndex", Int),
    ("TransferListFilters/statusFilterState", Bool),
    ("TransferListFilters/CategoryFilterState", Bool),
    ("TransferListFilters/TagFilterState", Bool),
    ("TransferListFilters/TrackerStatusFilterState", Bool),
    ("TransferListFilters/trackerFilterState", Bool),
    ("MainWindow/LastDir", Text),
    ("TorrentProperties/Visible", Bool),
    ("TorrentProperties/CurrentTab", Int),
    ("GUI/PropertiesWidget/FilterPatternFormat", Enum(FILTER_FORMAT)),
    ("SpeedWidget/period", Int),
    ("AddNewTorrentDialog/RememberLastSavePath", Bool),
    ("AddNewTorrentDialog/DefaultCategory", Text),
    ("AddNewTorrentDialog/SavePathHistory", TextList),
    ("AddNewTorrentDialog/DownloadPathHistory", TextList),
    ("GUI/AddNewTorrentDialog/FilterPatternFormat", Enum(FILTER_FORMAT)),
    ("AddTrackersDialog/TrackersListURL", Text),
    // Feature windows (`FeatureDefaults`).
    ("Search/StoreOpenedSearchTabs", Bool),
    ("Search/StoreOpenedSearchTabResults", Bool),
    ("Search/HistoryLength", Int),
    ("SearchTab/UseRegexAsFilteringPattern", Bool),
    ("TorrentCreator/PieceSize", Int),
    ("TorrentCreator/PrivateTorrent", Bool),
    ("TorrentCreator/StartSeeding", Bool),
    ("TorrentCreator/IgnoreRatio", Bool),
    ("TorrentCreator/OptimizeAlignment", Bool),
    ("TorrentCreator/PaddedFileSizeLimit", Int),
    ("TorrentCreator/LastAddPath", Text),
    ("TorrentCreator/LastSavePath", Text),
    ("TorrentCreator/TrackerList", Text),
    ("TorrentCreator/WebSeedList", Text),
    ("TorrentCreator/Comments", Text),
    ("TorrentCreator/Source", Text),
    ("Preferences/Downloads/ScanDirsLastPath", Text),
];

/// UI lock password hash (`Locking/password_PBKDF2`, `@ByteArray`).
const LOCK_KEY: &str = "Locking/password_PBKDF2";
/// Search history (`SearchUI/History.txt` in qBittorrent's data folder).
pub const SEARCH_HISTORY_KEY: &str = "Search/History";

/// The GUI settings found in the ini, plus the search history lines.
/// Returns (key, value) pairs in a stable order and the ini keys used.
pub fn collect(ini: &IniFile, search_history: Option<&str>) -> (Vec<(String, PlistValue)>, Vec<String>) {
    let mut out = Vec::new();
    let mut used = Vec::new();
    for &(key, kind) in GUI_KEYS {
        let Some(v) = ini.get(key) else { continue };
        let value = match kind {
            Bool => v.to_bool().map(PlistValue::Bool),
            Int => v.to_int().map(PlistValue::Int),
            Text => Some(PlistValue::String(v.to_string_value())),
            TextList => Some(PlistValue::Strings(v.to_string_list().into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())),
            Enum(names) => {
                let s = v.to_string_value();
                let t = s.trim();
                t.parse::<i64>().ok().or_else(|| names.iter().find(|(n, _)| n.eq_ignore_ascii_case(t)).map(|(_, i)| *i)).map(PlistValue::Int)
            }
        };
        if let Some(value) = value {
            out.push((key.to_owned(), value));
            used.push(key.to_owned());
        }
    }
    if let Some(hash) = ini.byte_array(LOCK_KEY).and_then(|b| String::from_utf8(b).ok()).filter(|h| h.contains(':')) {
        out.push((LOCK_KEY.to_owned(), PlistValue::String(hash)));
        used.push(LOCK_KEY.to_owned());
    }
    if let Some(text) = search_history {
        let lines: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        if !lines.is_empty() {
            out.push((SEARCH_HISTORY_KEY.to_owned(), PlistValue::Strings(lines)));
        }
    }
    (out, used)
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            // Not representable in XML 1.0.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => o.push(c),
        }
    }
    o
}

/// An XML property list (`defaults import` and `UserDefaults` read it).
pub fn to_plist(values: &[(String, PlistValue)]) -> String {
    let mut s = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n<dict>\n",
    ));
    for (k, v) in values {
        s.push_str(&format!("\t<key>{}</key>\n", xml_escape(k)));
        match v {
            PlistValue::Bool(true) => s.push_str("\t<true/>\n"),
            PlistValue::Bool(false) => s.push_str("\t<false/>\n"),
            PlistValue::Int(i) => s.push_str(&format!("\t<integer>{i}</integer>\n")),
            PlistValue::Real(r) => s.push_str(&format!("\t<real>{r}</real>\n")),
            PlistValue::String(t) => s.push_str(&format!("\t<string>{}</string>\n", xml_escape(t))),
            PlistValue::Strings(list) => {
                s.push_str("\t<array>\n");
                for t in list {
                    s.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(t)));
                }
                s.push_str("\t</array>\n");
            }
        }
    }
    s.push_str("</dict>\n</plist>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_gui_settings() {
        let ini = IniFile::parse(
            concat!(
                "[GUI]\nStartUpWindowState=Minimized\nDownloadTrackerFavicon=true\nLog\\Enabled=false\n",
                "[Preferences]\nGeneral\\ExitConfirm=false\nDownloads\\DblClOnTorDl=4\nGeneral\\SpeedInTitleBar=true\n",
                "[AddNewTorrentDialog]\nSavePathHistory=/a, /b & c\nDownloadPathHistory=, /tmp/x\nEnabled=true\n",
                "[Appearance]\nColorScheme=Dark\n",
                "[Locking]\npassword_PBKDF2=\"@ByteArray(c2FsdA==:aGFzaA==)\"\n",
                "[MainWindow]\ngeometry=@ByteArray(\\x1)\n",
            )
            .as_bytes(),
        );
        let (vals, used) = collect(&ini, Some("ubuntu\n\n  debian  \n"));
        let get = |k: &str| vals.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
        assert_eq!(get("GUI/StartUpWindowState"), Some(PlistValue::Int(1)));
        assert_eq!(get("Preferences/General/ExitConfirm"), Some(PlistValue::Bool(false)));
        assert_eq!(get("Preferences/Downloads/DblClOnTorDl"), Some(PlistValue::Int(4)));
        assert_eq!(get("AddNewTorrentDialog/SavePathHistory"), Some(PlistValue::Strings(vec!["/a".into(), "/b & c".into()])));
        assert_eq!(get("AddNewTorrentDialog/DownloadPathHistory"), Some(PlistValue::Strings(vec!["/tmp/x".into()])));
        assert_eq!(get("Appearance/ColorScheme"), Some(PlistValue::Int(2)));
        assert_eq!(get("Locking/password_PBKDF2"), Some(PlistValue::String("c2FsdA==:aGFzaA==".into())));
        assert_eq!(get("Search/History"), Some(PlistValue::Strings(vec!["ubuntu".into(), "debian".into()])));
        assert_eq!(get("MainWindow/geometry"), None);
        assert!(used.contains(&"GUI/DownloadTrackerFavicon".to_string()));

        let plist = to_plist(&vals);
        assert!(plist.contains("<key>GUI/StartUpWindowState</key>\n\t<integer>1</integer>"));
        assert!(plist.contains("<string>/b &amp; c</string>"));
        assert!(plist.contains("<key>Preferences/General/ExitConfirm</key>\n\t<false/>"));
        assert!(plist.ends_with("</dict>\n</plist>\n"));
    }
}
