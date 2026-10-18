import Foundation

/// The snapshot file in the App Group container shared by the app and the
/// widget extension (`~/Library/Group Containers/<group>/`). The app is not
/// sandboxed; it reaches the same folder through its application-groups
/// entitlement, which macOS honours without a provisioning profile because
/// the group name starts with the team ID.
enum WidgetSnapshotStore {
    /// Fallback when the bundle has no `QBTAppGroup` key (unit tests).
    static let defaultAppGroup = "YWA2X8G6VY.mk.lex.qbitneors"
    static let fileName = "widget-snapshot.json"

    /// The group from Info.plist (`$(QBT_APP_GROUP)` in project.yml).
    static var appGroup: String {
        let value = Bundle.main.object(forInfoDictionaryKey: "QBTAppGroup") as? String ?? ""
        return value.isEmpty ? defaultAppGroup : value
    }

    static var containerURL: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup)
    }

    static var fileURL: URL? {
        containerURL?.appending(path: fileName)
    }

    static func load(from url: URL? = fileURL) -> WidgetSnapshot? {
        guard let url,
              let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size <= WidgetSnapshot.maxEncodedSize,
              let data = try? Data(contentsOf: url) else { return nil }
        return WidgetSnapshot.decode(data)
    }

    /// Writes atomically (readers never see a partial file).
    static func save(_ snapshot: WidgetSnapshot, to url: URL? = fileURL) throws {
        guard let url else { throw CocoaError(.fileNoSuchFile) }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try snapshot.encoded().write(to: url, options: [.atomic])
    }
}
