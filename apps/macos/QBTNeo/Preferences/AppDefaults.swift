import Foundation

/// The defaults store of the app's GUI-only settings. With `--profile <dir>`
/// they live in that profile (`<dir>/config/gui-preferences.plist`), next to
/// the core's own settings, so a second instance on another profile never
/// changes the regular settings (lock password, window layout, search options).
/// Demo and snapshot runs without a profile use a store in the temporary
/// folder. Only the default profile uses the app's standard defaults domain;
/// every other instance also routes AppKit's own autosaving there
/// (`StandardDefaultsRedirect`).
enum AppDefaults {
    /// `UserDefaults` is thread-safe (documented), just not marked `Sendable`.
    nonisolated(unsafe) static let store: UserDefaults = {
        guard let config = configDirectory(LaunchOptions.current) else { return .standard }
        try? FileManager.default.createDirectory(at: config, withIntermediateDirectories: true)
        return UserDefaults(suiteName: config.appending(path: "gui-preferences").path(percentEncoded: false)) ?? .standard
    }()

    private static func configDirectory(_ launch: LaunchOptions) -> URL? {
        if !launch.profileDirectory.isEmpty {
            return URL(filePath: launch.profileDirectory, directoryHint: .isDirectory)
                .appending(path: "config", directoryHint: .isDirectory)
        }
        guard launch.usesDemoData else { return nil }
        return URL.temporaryDirectory.appending(path: "qBit NeoRS demo", directoryHint: .isDirectory)
    }
}
