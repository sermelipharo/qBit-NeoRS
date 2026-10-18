import Foundation
import Observation

/// The torrent name rules and the parse results the transfer list and the
/// cards show. Rules persist in the GUI defaults (only once edited; until
/// then the built-in rules apply, so new built-in rules arrive with updates).
/// The parser is created on first use and results are cached per name, so a
/// table refresh only does a dictionary lookup.
@Observable @MainActor
final class TorrentNamesModel {
    private(set) var rules: [NameRule]
    /// Bumped whenever parse results or how they are shown may differ
    /// (rules, the year option, the on/off switches); views redraw names.
    private(set) var revision = 0

    @ObservationIgnored private let gui: GUIPreferences
    @ObservationIgnored private var cache: NameParseCache?
    @ObservationIgnored private var observedSwitches: Switches?
    @ObservationIgnored private var categoryKinds: [String: ContentKind] = [:]

    private static let rulesKey = GUIPreferenceKey.torrentNameRules.rawValue

    init(gui: GUIPreferences) {
        self.gui = gui
        rules = Self.loadRules(gui.defaults)
        observedSwitches = Switches(gui)
        observeSwitches()
    }

    var isCustomized: Bool { rules != NameRuleDefaults.rules }

    /// Title and tags of `name`, or nil when parsing is off.
    func parsed(_ name: String) -> ParsedName? {
        guard gui.parseTorrentNames else { return nil }
        return parsedIgnoringSwitch(name)
    }

    /// Parses even while the switch is off (the Settings preview).
    func parsedIgnoringSwitch(_ name: String) -> ParsedName {
        if let cache { return cache.parsed(name) }
        let made = NameParseCache(parser: makeParser(rules))
        cache = made
        return made.parsed(name)
    }

    /// What a torrent most likely contains: from its name (cached with the
    /// parse result; only the file extension while parsing is off), else from
    /// its category's name ("Movies", "Музыка").
    func contentKind(name: String, category: String) -> ContentKind {
        let fromName = gui.parseTorrentNames
            ? parsedIgnoringSwitch(name).kind
            : ContentKindDetector.kind(fileExtension: (name as NSString).pathExtension) ?? .other
        guard fromName == .other, !category.isEmpty else { return fromName }
        if let cached = categoryKinds[category] { return cached }
        let kind = ContentKindDetector.kind(categoryName: category) ?? .other
        if categoryKinds.count > 256 { categoryKinds.removeAll(keepingCapacity: true) }
        categoryKinds[category] = kind
        return kind
    }

    /// Whether the list emphasizes titles (parsing on and the list option on).
    var emphasizesListTitles: Bool { gui.parseTorrentNames && gui.torrentNamesSemiboldTitles }

    /// Whether cards show tags.
    var showsCardTags: Bool { gui.parseTorrentNames && gui.torrentNamesCardTags }

    /// Replaces the rules (already validated by the editor) and saves them.
    func setRules(_ new: [NameRule]) {
        guard new != rules else { return }
        rules = new
        if isCustomized, let data = try? JSONEncoder().encode(new) {
            gui.defaults.set(data, forKey: Self.rulesKey)
        } else {
            gui.defaults.removeObject(forKey: Self.rulesKey)
        }
        rebuild()
    }

    func resetToDefaults() {
        setRules(NameRuleDefaults.rules)
    }

    /// A parser for `rules` with the current options (the editor's preview uses its draft).
    func makeParser(_ rules: [NameRule]) -> NameParser {
        NameParser(rules: rules, options: .init(yearAsChip: gui.torrentNamesYearAsTag))
    }

    private func rebuild() {
        cache?.replaceParser(makeParser(rules))
        revision &+= 1
    }

    // MARK: Switches

    /// The GUI settings that change parse results or their display.
    private struct Switches: Equatable {
        var enabled: Bool
        var semibold: Bool
        var cardTags: Bool
        var yearAsTag: Bool

        @MainActor init(_ gui: GUIPreferences) {
            enabled = gui.parseTorrentNames
            semibold = gui.torrentNamesSemiboldTitles
            cardTags = gui.torrentNamesCardTags
            yearAsTag = gui.torrentNamesYearAsTag
        }
    }

    private func observeSwitches() {
        withObservationTracking {
            _ = Switches(gui)
        } onChange: { [weak self] in
            Task { @MainActor in
                self?.switchesChanged()
            }
        }
    }

    private func switchesChanged() {
        let next = Switches(gui)
        if next != observedSwitches {
            let yearChanged = next.yearAsTag != observedSwitches?.yearAsTag
            observedSwitches = next
            if yearChanged {
                rebuild()
            } else {
                revision &+= 1
            }
        }
        observeSwitches()
    }

    // MARK: Persistence

    /// Stored rules, or the built-in ones. Edited rules that fail validation
    /// (a hand-edited defaults file) are switched off for this run.
    private static func loadRules(_ defaults: UserDefaults) -> [NameRule] {
        guard let data = defaults.data(forKey: rulesKey),
              let stored = try? JSONDecoder().decode([NameRule].self, from: data), !stored.isEmpty
        else { return NameRuleDefaults.rules }
        let builtIn = Set(NameRuleDefaults.rules)
        return stored.map { rule in
            guard rule.isEnabled, !builtIn.contains(rule), NameRuleValidator.issue(rule) != nil else { return rule }
            var off = rule
            off.isEnabled = false
            return off
        }
    }
}
