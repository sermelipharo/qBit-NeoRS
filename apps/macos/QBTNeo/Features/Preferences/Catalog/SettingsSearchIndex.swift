import Foundation

/// Matching and ranking behind settings search, free of the catalog so it can
/// be unit-tested. Every word of the query must appear in one of an item's
/// texts; case, diacritics and width are ignored, so it works the same in
/// every language. Stored keys also match with spaces in place of `_`, `/`
/// and `.`, and split at their capitals (`ColorScheme` → "color scheme"); a
/// query that is exactly a key ranks that setting first.
struct SettingsSearchIndex: Sendable {
    struct Item: Sendable {
        let id: String
        let tier: SettingTier
        /// Row label: localized, plus its English source when the UI isn't English.
        let titles: [String]
        /// Section, segment and pane titles (localized and English).
        let path: [String]
        /// Our search words and qBittorrent's labels and group names.
        let keywords: [String]
        /// Stored keys: WebAPI, GUI ini, and qBittorrent ini names we import.
        let keys: [String]
        let help: String?
    }

    /// One item's searchable text, folded once, by relevance.
    private struct Entry: Sendable {
        let id: String
        /// Titles; path; keywords; keys; help.
        let fields: [String]
        let all: String
        /// Normalized forms of the keys, for exact matches.
        let keys: Set<String>
    }

    private let entries: [Entry]

    /// Items without a row of their own (hidden, platform) are left out; every other tier is searchable.
    init(_ items: [Item]) {
        entries = items.filter(\.tier.hasRow).map { item in
            let keyForms = item.keys.flatMap(Self.keyForms)
            let fields = [
                item.titles.map(Self.fold),
                item.path.map(Self.fold),
                item.keywords.map(Self.fold),
                keyForms,
                [item.help.map(Self.fold) ?? ""],
            ].map { $0.joined(separator: Self.separator) }
            let exact = Set(item.keys.map { Self.normalize(Self.fold($0)) })
            return Entry(id: item.id, fields: fields, all: fields.joined(separator: Self.separator), keys: exact)
        }
    }

    /// Ids of the matching items: an exact key first, then by where the words
    /// were found (label, section, keyword, key, explanation), the query as
    /// typed before its punctuation-split pieces, then in item order.
    func search(_ query: String, limit: Int) -> [String] {
        let folded = Self.fold(query)
        let words = Self.words(folded)
        let normalized = Self.normalize(folded)
        let keyWords = Self.words(normalized)
        // Punctuation alone matches nothing useful.
        guard !keyWords.isEmpty else { return [] }
        let ranked = entries.enumerated().compactMap { offset, entry -> (rank: Int, offset: Int, id: String)? in
            let found = [words, keyWords].first { !$0.isEmpty && $0.allSatisfy(entry.all.contains) }
            guard let found else { return nil }
            if entry.keys.contains(normalized) { return (-1, offset, entry.id) }
            let field = entry.fields.firstIndex { field in found.allSatisfy(field.contains) } ?? entry.fields.count
            // Matches of the query as typed (`queueing_en`) before those of its pieces ("queueing", "en").
            let loose = found != words ? entry.fields.count + 1 : 0
            return (loose + field, offset, entry.id)
        }
        return ranked.sorted { ($0.rank, $0.offset) < ($1.rank, $1.offset) }.prefix(limit).map(\.id)
    }

    // MARK: Text forms

    private static let separator = " \u{1F} "

    static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: .current)
    }

    /// Letters and digits only, single spaces between the runs: `queueing_enabled` → "queueing enabled".
    static func normalize(_ text: String) -> String {
        text.components(separatedBy: CharacterSet.alphanumerics.inverted).filter { !$0.isEmpty }.joined(separator: " ")
    }

    /// A key as typed, with separators as spaces, and split at its capitals:
    /// `Appearance/ColorScheme` → "appearance/colorscheme", "appearance colorscheme", "appearance color scheme".
    static func keyForms(_ key: String) -> [String] {
        let folded = fold(key)
        var forms = [folded, normalize(folded)]
        let split = normalize(fold(splitCamelCase(key)))
        if split != forms[1] { forms.append(split) }
        return forms
    }

    /// Spaces between `aB` and before the last capital of a run followed by a
    /// lowercase letter (`IPFilter` → "IP Filter", `DHTEnabled` → "DHT Enabled").
    static func splitCamelCase(_ text: String) -> String {
        let chars = Array(text)
        var out = ""
        for (index, char) in chars.enumerated() {
            if index > 0, char.isUppercase {
                let previous = chars[index - 1]
                let nextIsLower = index + 1 < chars.count && chars[index + 1].isLowercase
                if previous.isLowercase || previous.isNumber || (previous.isUppercase && nextIsLower) { out.append(" ") }
            }
            out.append(char)
        }
        return out
    }

    private static func words(_ text: String) -> [String] {
        text.split(whereSeparator: \.isWhitespace).map(String.init)
    }
}
