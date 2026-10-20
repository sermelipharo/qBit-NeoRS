import Foundation

/// Settings search over the catalog: every word of the query must appear in
/// the label, the section, segment or pane title, a keyword, qBittorrent's
/// own label or group for the setting, or a stored key (WebAPI, GUI ini or
/// imported qBittorrent ini name). Labels and titles match in the UI language
/// and in English. Covers every tier with a row, advanced ones included
/// (matching rules: `SettingsSearchIndex`).
@MainActor
enum SettingsSearch {
    struct Result: Identifiable {
        let spec: SettingSpec
        var id: String { spec.id }
        /// "Pane › Section".
        var path: String { "\(spec.pane.title) › \(spec.section.title)" }
        var title: String { displayTitle(spec.title) }
    }

    static let limit = 40

    /// Built on the first search.
    private static let index = SettingsSearchIndex(items)

    /// Best match first: a query that is exactly a stored key, then matches in
    /// the label, section, keyword, key, explanation; then catalog (pane) order.
    static func search(_ query: String) -> [Result] {
        index.search(query, limit: limit).compactMap { SettingsCatalog.spec($0).map(Result.init) }
    }

    /// The label without its trailing colon.
    nonisolated static func displayTitle(_ title: String) -> String {
        title.trimmingCharacters(in: CharacterSet(charactersIn: ":：").union(.whitespaces))
    }

    /// The catalog as search items, with English texts when the UI isn't English.
    private static var items: [SettingsSearchIndex.Item] {
        let source = SourceStrings()
        func texts(_ text: String) -> [String] {
            [text] + (source?.english(text).map { [$0] } ?? [])
        }
        return SettingsCatalog.all.map { spec in
            let segment = spec.section.segment?.title ?? spec.section.interfaceSegment?.title
            let aliases = spec.keys.flatMap(SettingsSearchAliases.aliases(for:))
            let labels = aliases.filter { !isKey($0) }
            return SettingsSearchIndex.Item(
                id: spec.id,
                tier: spec.tier,
                titles: texts(spec.title),
                path: ([spec.section.title, spec.pane.title] + (segment.map { [$0] } ?? [])).flatMap(texts),
                keywords: spec.keywords + labels + (source == nil ? [] : labels.compactMap(translated)),
                keys: spec.keys + aliases.filter(isKey),
                help: spec.help
            )
        }
    }

    /// qBittorrent's wording in the UI language: most of its labels came with
    /// its translations into the string catalog ("Torrent Queueing").
    private static func translated(_ label: String) -> String? {
        let text = Bundle.main.localizedString(forKey: label, value: nil, table: nil)
        return text == label ? nil : text
    }

    /// An ini or WebAPI key among the aliases (`BitTorrent/Session/Port`, `random_port`) rather than a label.
    private static func isKey(_ alias: String) -> Bool {
        !alias.contains(" ") && (alias.contains("/") || alias.contains("_"))
    }
}
