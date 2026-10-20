import Foundation
import Observation

/// The rules editor's draft: edits apply only with Done, after every changed
/// rule passed validation (including the timed run). Live feedback while
/// typing uses the cheap checks; the preview parses with the draft.
@Observable @MainActor
final class NameRulesEditorModel {
    /// The draft (rows bind their checkboxes to it).
    var rules: [NameRule] {
        didSet {
            refreshIssues(from: oldValue)
            rebuildPreview()
        }
    }
    var selection: NameRule.ID?
    var previewText = "Movie.Name.2021.2160p.WEB-DL.DDP5.1.Atmos.DV.HDR.HEVC-GROUP.mkv" {
        didSet { updatePreview() }
    }
    private(set) var preview: ParsedName = .plain("")
    /// Cheap-check problems by rule (timed problems are added by `validateForSaving`).
    private(set) var issues: [NameRule.ID: NameRuleIssue] = [:]
    /// A message after an import or a failed save.
    var notice: String?

    @ObservationIgnored private let names: TorrentNamesModel
    @ObservationIgnored private var parser: NameParser
    @ObservationIgnored private let original: [NameRule]

    init(names: TorrentNamesModel) {
        self.names = names
        original = names.rules
        rules = names.rules
        parser = names.makeParser(names.rules)
        for rule in rules {
            if let issue = NameRuleValidator.quickIssue(rule) { issues[rule.id] = issue }
        }
        selection = rules.first?.id
        updatePreview()
    }

    var hasChanges: Bool { rules != original }
    var isDefault: Bool { rules == NameRuleDefaults.rules }
    var enabledCount: Int { rules.count { $0.isEnabled } }

    var selectedIndex: Int? {
        selection.flatMap { id in rules.firstIndex { $0.id == id } }
    }

    var selectedRule: NameRule? {
        selectedIndex.map { rules[$0] }
    }

    // MARK: Editing

    /// Replaces a rule (fields edited in the detail form or the row's checkbox).
    func update(_ rule: NameRule) {
        guard let index = rules.firstIndex(where: { $0.id == rule.id }), rules[index] != rule else { return }
        rules[index] = rule
    }

    func add() {
        let rule = NameRule(
            name: String(localized: "names.newRule", defaultValue: "New Rule", comment: "Name of a new torrent name rule"),
            category: .other, pattern: "TAG"
        )
        let index = selectedIndex.map { $0 + 1 } ?? rules.count
        rules.insert(rule, at: index)
        selection = rule.id
    }

    func removeSelected() {
        guard let index = selectedIndex else { return }
        rules.remove(at: index)
        selection = rules.indices.contains(index) ? rules[index].id : rules.last?.id
    }

    func move(from source: IndexSet, to destination: Int) {
        rules.move(fromOffsets: source, toOffset: destination)
    }

    func resetToDefaults() {
        rules = NameRuleDefaults.rules
        selection = rules.first?.id
        notice = nil
    }

    // MARK: Import and export

    func exportData() throws -> Data {
        try NameRulesDocument.export(rules)
    }

    func importRules(from url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        // Check the size before reading, so a huge file is never loaded.
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
        do {
            guard size <= NameRulesDocument.maxFileSize else { throw NameRulesDocument.ImportError.tooLarge }
            let imported = try NameRulesDocument.importRules(from: Data(contentsOf: url))
            rules = imported.rules
            selection = rules.first?.id
            notice = imported.disabledCount == 0
                ? String(localized: "names.import.done", defaultValue: "Imported \(imported.rules.count) rules.", comment: "Name rules import")
                : String(localized: "names.import.disabled",
                         defaultValue: "Imported \(imported.rules.count) rules; \(imported.disabledCount) didn't pass the checks and were switched off.",
                         comment: "Name rules import")
        } catch let error as NameRulesDocument.ImportError {
            notice = switch error {
            case .tooLarge: String(localized: "names.import.tooLarge", defaultValue: "The file is larger than 1 MB.", comment: "Name rules import")
            case .notRules: String(localized: "names.import.notRules", defaultValue: "The file doesn't contain torrent name rules.",
                                   comment: "Name rules import")
            case .tooManyRules: String(localized: "names.import.tooMany", defaultValue: "The file has more than \(NameRulesDocument.maxRules) rules.",
                                       comment: "Name rules import")
            }
        } catch {
            notice = error.localizedDescription
        }
    }

    // MARK: Saving

    /// Runs every check on the enabled rules that differ from the saved or
    /// built-in ones; selects the first failing rule. True when all pass.
    func validateForSaving() -> Bool {
        let known = Set(original).union(NameRuleDefaults.rules)
        for rule in rules where rule.isEnabled && !known.contains(rule) {
            if let issue = NameRuleValidator.issue(rule) {
                issues[rule.id] = issue
                selection = rule.id
                notice = String(localized: "names.save.failed", defaultValue: "Fix “\(rule.displayName)” or switch it off.",
                                comment: "Name rules editor: a rule failed validation")
                return false
            }
            issues[rule.id] = nil
        }
        return true
    }

    func save() -> Bool {
        guard validateForSaving() else { return false }
        names.setRules(rules)
        return true
    }

    // MARK: Issues and preview

    /// Why the Web UI skips a valid rule, if it does (shown under its pattern).
    func webNote(for id: NameRule.ID) -> String? {
        guard issues[id] == nil, let rule = rules.first(where: { $0.id == id }),
              let construct = NameRuleWebCompatibility.unsupportedConstruct(in: rule.pattern) else { return nil }
        return String(localized: "names.editor.pattern.web",
                      defaultValue: "The Web UI can't run “\(construct)” and skips this rule there.",
                      comment: "Name rules editor: the pattern uses regular expression syntax that JavaScript lacks")
    }

    /// Re-checks the rules that changed (all of them after adding, removing or importing).
    private func refreshIssues(from old: [NameRule]) {
        if old.count == rules.count, zip(old, rules).allSatisfy({ $0.id == $1.id }) {
            for (before, after) in zip(old, rules) where before != after {
                issues[after.id] = NameRuleValidator.quickIssue(after)
            }
            return
        }
        // Moved rules keep their issues; otherwise check everything again.
        if Set(old) == Set(rules) { return }
        issues = [:]
        for rule in rules {
            if let issue = NameRuleValidator.quickIssue(rule) { issues[rule.id] = issue }
        }
    }

    private func rebuildPreview() {
        parser = names.makeParser(rules)
        updatePreview()
    }

    private func updatePreview() {
        preview = parser.parse(previewText)
    }
}
