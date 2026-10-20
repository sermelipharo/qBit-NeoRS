import Foundation

/// Maps the app's translated strings back to their English source, so
/// settings search also finds English labels while the UI is in another
/// language. Built from the compiled string tables while the search index is
/// built, then dropped.
struct SourceStrings {
    private let english: [String: String]

    /// `translations`: key → translated text; `source`: key → English text,
    /// for keys that aren't the English text themselves.
    init(translations: [String: String], source: [String: String]) {
        var english: [String: String] = [:]
        // Sorted, so a translation shared by several keys always maps to the same one.
        for key in translations.keys.sorted() {
            guard let translated = translations[key], english[translated] == nil else { continue }
            let text = source[key] ?? key
            if text != translated { english[translated] = text }
        }
        self.english = english
    }

    /// The tables of `bundle` for the language its UI resolved to; nil while
    /// that is English or a table is missing.
    init?(bundle: Bundle = .main, table: String = "Localizable") {
        guard let language = bundle.preferredLocalizations.first, !language.hasPrefix("en"),
              let translations = Self.strings(bundle, table, language)
        else { return nil }
        self.init(translations: translations, source: Self.strings(bundle, table, "en") ?? [:])
    }

    /// The English source of a translated string, if it differs.
    func english(_ translated: String) -> String? {
        english[translated]
    }

    private static func strings(_ bundle: Bundle, _ table: String, _ language: String) -> [String: String]? {
        guard let url = bundle.url(forResource: table, withExtension: "strings", subdirectory: nil, localization: language),
              let dictionary = NSDictionary(contentsOf: url) as? [String: String]
        else { return nil }
        return dictionary
    }
}
