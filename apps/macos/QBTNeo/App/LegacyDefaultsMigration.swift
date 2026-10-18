import Foundation

/// The app used to ship as "qBittorrent Neo" with the bundle id
/// `mk.lex.qbtneo`. Copies that domain's preferences into the current one,
/// once, and leaves the old domain untouched.
enum LegacyDefaultsMigration {
    private static let legacyDomain = "mk.lex.qbtneo"
    private static let doneKey = "LegacyDefaultsMigrated"

    static func runIfNeeded(defaults: UserDefaults = AppDefaults.store) {
        guard defaults.object(forKey: doneKey) == nil else { return }
        defaults.set(true, forKey: doneKey)
        guard let old = defaults.persistentDomain(forName: legacyDomain) else { return }
        for (key, value) in old where defaults.object(forKey: key) == nil {
            defaults.set(value, forKey: key)
        }
    }
}
