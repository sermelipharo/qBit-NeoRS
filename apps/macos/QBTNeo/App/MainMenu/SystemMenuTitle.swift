import Foundation

/// Titles of the standard menus and items (File, Edit, Undo, Hide …), read
/// from the same table SwiftUI builds its default main menu from, so they
/// are worded and localized exactly as before.
enum SystemMenuTitle {
    private static let bundle = Bundle(identifier: "com.apple.SwiftUI")

    /// The app name in "Hide %@" and "Quit %@".
    static let appName: String = {
        let info = Bundle.main.localizedInfoDictionary ?? Bundle.main.infoDictionary ?? [:]
        return info["CFBundleDisplayName"] as? String ?? info["CFBundleName"] as? String ?? ProcessInfo.processInfo.processName
    }()

    static func text(_ key: String) -> String {
        bundle?.localizedString(forKey: key, value: nil, table: "MainMenu") ?? key
    }

    /// A title with the app name in place of "%@".
    static func withAppName(_ key: String) -> String {
        text(key).replacingOccurrences(of: "%@", with: appName)
    }
}
