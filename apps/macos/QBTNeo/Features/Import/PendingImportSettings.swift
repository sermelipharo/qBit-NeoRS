import Foundation

/// Applies the GUI settings of a pending full import (an empty profile that
/// takes over the source's settings) before `GUIPreferences` reads them, so
/// they apply on this launch. The previous values of those keys are saved to
/// `<profile>/migration/gui-defaults-before-import.plist` first, which the
/// profile backup then includes.
enum PendingImportSettings {
    static func applyIfNeeded(_ launch: LaunchOptions) {
        guard !launch.usesDemoData,
              let request = importPending(profileDir: launch.profileDirectory),
              !request.keepSettings,
              importProfileIsEmpty(profileDir: launch.profileDirectory)
        else { return }
        let xml = importGuiSettings(source: request.source)
        guard let data = xml.data(using: .utf8),
              let values = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
              !values.isEmpty
        else { return }
        let store = AppDefaults.store
        var previous: [String: Any] = [:]
        var missing: [String] = []
        for key in values.keys.sorted() {
            if let old = store.object(forKey: key) { previous[key] = old } else { missing.append(key) }
        }
        let backup: [String: Any] = ["values": previous, "missing": missing]
        let url = profileURL(launch).appending(path: "migration/gui-defaults-before-import.plist")
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        guard let encoded = try? PropertyListSerialization.data(fromPropertyList: backup, format: .xml, options: 0),
              (try? encoded.write(to: url, options: .atomic)) != nil
        else { return }
        for (key, value) in values { store.set(value, forKey: key) }
    }

    /// The profile folder (`--profile`, or the default one).
    static func profileURL(_ launch: LaunchOptions) -> URL {
        if !launch.profileDirectory.isEmpty {
            return URL(filePath: launch.profileDirectory, directoryHint: .isDirectory)
        }
        return URL.applicationSupportDirectory.appending(path: "qBit NeoRS", directoryHint: .isDirectory)
    }
}
