import Foundation
import Testing

struct NameRulesDocumentTests {
    @Test func exportAndImportRoundTrip() throws {
        let data = try NameRulesDocument.export(NameRuleDefaults.rules)
        let imported = try NameRulesDocument.importRules(from: data)
        #expect(imported.rules == NameRuleDefaults.rules)
        #expect(imported.disabledCount == 0)
    }

    @Test func rejectsOtherFiles() {
        #expect(throws: NameRulesDocument.ImportError.notRules) {
            try NameRulesDocument.importRules(from: Data(#"{"rules": []}"#.utf8))
        }
        #expect(throws: NameRulesDocument.ImportError.notRules) {
            try NameRulesDocument.importRules(from: Data("not json".utf8))
        }
        #expect(throws: NameRulesDocument.ImportError.tooLarge) {
            try NameRulesDocument.importRules(from: Data(count: NameRulesDocument.maxFileSize + 1))
        }
    }

    @Test func rejectsTooManyRules() throws {
        let rules = (0 ... NameRulesDocument.maxRules).map { NameRule(id: "r\($0)", category: .other, pattern: "x\($0)") }
        let data = try NameRulesDocument.export(rules)
        #expect(throws: NameRulesDocument.ImportError.tooManyRules) { try NameRulesDocument.importRules(from: data) }
    }

    @Test func invalidOrSlowRulesArriveSwitchedOff() throws {
        let rules = [
            NameRule(id: "good", category: .resolution, pattern: "1080p"),
            NameRule(id: "broken", category: .other, pattern: "(unclosed"),
            NameRule(id: "slow", category: .other, pattern: "(a+)+$"),
        ]
        let imported = try NameRulesDocument.importRules(from: NameRulesDocument.export(rules))
        #expect(imported.disabledCount == 2)
        #expect(imported.rules.map(\.isEnabled) == [true, false, false])
    }

    @Test func duplicateIDsAreReplaced() throws {
        let rules = [NameRule(id: "same", category: .other, pattern: "a"), NameRule(id: "same", category: .other, pattern: "b")]
        let imported = try NameRulesDocument.importRules(from: NameRulesDocument.export(rules))
        #expect(Set(imported.rules.map(\.id)).count == 2)
    }

    @Test func minimalHandWrittenRulesLoad() throws {
        let json = #"""
            {"format": "qbitneors.name-rules", "version": 1,
             "rules": [{"pattern": "WEB-?DL", "category": "source"}, {"pattern": "x", "category": "future"}]}
            """#
        let imported = try NameRulesDocument.importRules(from: Data(json.utf8))
        #expect(imported.rules.count == 2)
        #expect(imported.rules[0].isEnabled)
        #expect(imported.rules[0].category == .source)
        #expect(imported.rules[1].category == .other)
    }
}
