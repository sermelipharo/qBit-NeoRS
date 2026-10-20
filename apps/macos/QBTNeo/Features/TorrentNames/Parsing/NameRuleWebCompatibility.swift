import Foundation

/// Whether the Web UI can run a rule. It translates the ICU syntax rules use
/// (`\w`, `\b`, `\Q…\E`, inline case flags) into JavaScript, but some
/// constructs have no JavaScript equivalent; the Web UI skips such rules and
/// shows those parts of names untouched. A quick scan for those constructs
/// (`crates/qbt-core/webui/src/icuregex.ts` rejects the same ones).
enum NameRuleWebCompatibility {
    /// The first construct the Web UI cannot run ("(?>", "*+", "\G"…), or nil.
    static func unsupportedConstruct(in pattern: String) -> String? {
        let chars = Array(pattern)
        var index = 0
        var inClass = false
        var classStart = 0
        while index < chars.count {
            let char = chars[index]
            let next = index + 1 < chars.count ? chars[index + 1] : nil
            if char == "\\" {
                if let found = escapeIssue(chars, at: index + 1, inClass: inClass) { return found }
                if next == "Q" {
                    index = endOfQuote(chars, from: index + 2)
                    continue
                }
                index += 2
                continue
            }
            if inClass {
                if char == "]", index > classStart { inClass = false }
                if char == "[" { return "[[…]]" }
                if (char == "&" && next == "&") || (char == "-" && next == "-") { return String([char, char]) }
            } else if char == "[" {
                if startsPOSIXSet(chars, at: index) { return "[:…:]" }
                inClass = true
                classStart = index + 1
                if next == "^" { classStart += 1 }
            } else if char == "(", next == "?" {
                if let found = groupIssue(chars, at: index + 2) { return found }
            } else if "*+?}".contains(char), next == "+", !(char == "?" && index > 0 && chars[index - 1] == "(") {
                return String([char, "+"])
            }
            index += 1
        }
        return nil
    }

    private static func escapeIssue(_ chars: [Character], at index: Int, inClass: Bool) -> String? {
        guard index < chars.count else { return nil }
        let char = chars[index]
        if ["G", "X", "N"].contains(char) { return "\\" + String(char) }
        if inClass, ["W", "H", "V", "R"].contains(char) { return "[\\" + String(char) + "]" }
        if char == "p" || char == "P", index + 3 < chars.count, chars[index + 1] == "{", chars[index + 2] == "I", chars[index + 3] == "n" {
            return "\\" + String(char) + "{In…}"
        }
        return nil
    }

    /// `(?` followed by something other than a group JavaScript knows.
    private static func groupIssue(_ chars: [Character], at index: Int) -> String? {
        guard index < chars.count else { return nil }
        let char = chars[index]
        if char == ">" { return "(?>" }
        if [":", "=", "!", "<", "#"].contains(char) { return nil }
        // Inline flags: only case-insensitivity has a JavaScript equivalent.
        var cursor = index
        while cursor < chars.count, chars[cursor].isLetter || chars[cursor] == "-" {
            if chars[cursor] != "i", chars[cursor] != "-" { return "(?" + String(chars[cursor]) }
            cursor += 1
        }
        return nil
    }

    private static func endOfQuote(_ chars: [Character], from start: Int) -> Int {
        var index = start
        while index + 1 < chars.count {
            if chars[index] == "\\", chars[index + 1] == "E" { return index + 2 }
            index += 1
        }
        return chars.count
    }

    /// `[:alpha:]` or `[^:alpha:]`: an ICU property set.
    private static func startsPOSIXSet(_ chars: [Character], at index: Int) -> Bool {
        var cursor = index + 1
        if cursor < chars.count, chars[cursor] == "^" { cursor += 1 }
        guard cursor < chars.count, chars[cursor] == ":" else { return false }
        cursor += 1
        var sawName = false
        while cursor + 1 < chars.count {
            let char = chars[cursor]
            if char == ":", chars[cursor + 1] == "]" { return sawName }
            guard char.isLetter || char == "_" || char == " " || char == "=" else { return false }
            sawName = true
            cursor += 1
        }
        return false
    }
}
