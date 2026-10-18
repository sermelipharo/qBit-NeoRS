import Foundation

/// The JSON file of exported name rules:
/// `{"format": "qbitneors.name-rules", "version": 1, "rules": [...]}`.
/// Imports are limited in size and count, and every rule is validated
/// (including the timed run); rules that fail are imported switched off.
struct NameRulesDocument: Codable, Sendable {
    static let format = "qbitneors.name-rules"
    static let currentVersion = 1
    static let maxFileSize = 1_048_576
    static let maxRules = 500

    var format = Self.format
    var version = Self.currentVersion
    var rules: [NameRule]

    enum ImportError: Error, Equatable {
        case tooLarge
        case notRules
        case tooManyRules
    }

    struct Imported: Equatable {
        var rules: [NameRule]
        /// Rules switched off because they failed validation.
        var disabledCount: Int
    }

    static func export(_ rules: [NameRule]) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(Self(rules: rules))
    }

    static func importRules(from data: Data) throws(ImportError) -> Imported {
        guard data.count <= maxFileSize else { throw .tooLarge }
        guard let document = try? JSONDecoder().decode(Self.self, from: data), document.format == format else { throw .notRules }
        guard document.rules.count <= maxRules else { throw .tooManyRules }
        var seen = Set<String>()
        var disabled = 0
        let rules = document.rules.map { original in
            var rule = original
            // Duplicate identifiers would confuse selection and reordering.
            if !seen.insert(rule.id).inserted {
                rule.id = "user." + UUID().uuidString
                seen.insert(rule.id)
            }
            if rule.isEnabled, NameRuleValidator.issue(rule) != nil {
                rule.isEnabled = false
                disabled += 1
            }
            return rule
        }
        return Imported(rules: rules, disabledCount: disabled)
    }
}
