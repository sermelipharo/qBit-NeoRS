import Foundation

/// One editable rule of the torrent name parser: a regular expression for a
/// technical token (matched as a whole token: letters or digits never touch
/// it on either side) and how its tag reads.
struct NameRule: Codable, Hashable, Sendable, Identifiable {
    /// Stable identifier; built-in rules use "default.*" (their names are localized).
    var id: String
    var isEnabled: Bool
    /// The user's name for the rule; empty for a built-in rule, which shows its localized name.
    var name: String
    var category: NameRuleCategory
    var pattern: String
    /// Tag text with `$1`… for capture groups (`$0` is the whole match); empty uses the matched text.
    var template: String
    var isCaseSensitive: Bool

    init(
        id: String = "user." + UUID().uuidString, isEnabled: Bool = true, name: String = "", category: NameRuleCategory,
        pattern: String, template: String = "", isCaseSensitive: Bool = false
    ) {
        self.id = id
        self.isEnabled = isEnabled
        self.name = name
        self.category = category
        self.pattern = pattern
        self.template = template
        self.isCaseSensitive = isCaseSensitive
    }

    /// The name shown in Settings.
    var displayName: String {
        if !name.isEmpty { return name }
        return NameRuleDefaults.localizedName(of: id) ?? pattern
    }

    private enum CodingKeys: String, CodingKey {
        case id, isEnabled = "enabled", name, category, pattern, template, isCaseSensitive = "caseSensitive"
    }

    /// Missing fields take their defaults, so hand-written or older files still load.
    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? "user." + UUID().uuidString
        isEnabled = try container.decodeIfPresent(Bool.self, forKey: .isEnabled) ?? true
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        category = try container.decodeIfPresent(NameRuleCategory.self, forKey: .category) ?? .other
        pattern = try container.decode(String.self, forKey: .pattern)
        template = try container.decodeIfPresent(String.self, forKey: .template) ?? ""
        isCaseSensitive = try container.decodeIfPresent(Bool.self, forKey: .isCaseSensitive) ?? false
    }
}
