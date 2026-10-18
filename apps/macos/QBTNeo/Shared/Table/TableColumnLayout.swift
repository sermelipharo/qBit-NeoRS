import Foundation

/// The persisted column layout of a table: display order, widths and which
/// columns the user hid. Stored as JSON in the GUI defaults (`AppDefaults`)
/// under a per-table key. Unknown ids are dropped and columns added in a
/// later version are inserted at their default place, so a stored layout
/// never stops loading.
struct TableColumnLayout: Codable, Equatable, Sendable {
    struct Column: Codable, Equatable, Sendable {
        var id: String
        var width: Double
        var isHidden: Bool
    }

    static let currentVersion = 1

    var version = Self.currentVersion
    /// In display order.
    var columns: [Column]

    /// The default layout of `specs`.
    init(specs: [TableColumnSpec]) {
        columns = specs.map { Column(id: $0.id, width: $0.idealWidth, isHidden: !$0.isVisibleByDefault) }
    }

    init(columns: [Column]) {
        self.columns = columns
    }

    /// This layout reconciled with the current specs: unknown ids removed,
    /// missing specs inserted after their predecessor in spec order, widths
    /// clamped to the spec's range and non-hideable columns visible.
    func reconciled(with specs: [TableColumnSpec]) -> Self {
        let byID = Dictionary(specs.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        var seen = Set<String>()
        var result: [Column] = []
        for column in columns {
            guard let spec = byID[column.id], seen.insert(column.id).inserted else { continue }
            let width = min(max(column.width, spec.minWidth), spec.maxWidth)
            result.append(Column(id: column.id, width: width, isHidden: spec.isHideable && column.isHidden))
        }
        for (index, spec) in specs.enumerated() where !seen.contains(spec.id) {
            let fresh = Column(id: spec.id, width: spec.idealWidth, isHidden: !spec.isVisibleByDefault)
            let predecessor = specs[..<index].last { seen.contains($0.id) }?.id
            let position = predecessor.flatMap { id in result.firstIndex { $0.id == id } }.map { $0 + 1 } ?? 0
            result.insert(fresh, at: position)
            seen.insert(spec.id)
        }
        return Self(columns: result)
    }

    // MARK: Storage

    static func load(key: String, defaults: UserDefaults = AppDefaults.store) -> Self? {
        guard let data = defaults.data(forKey: key),
              let layout = try? JSONDecoder().decode(Self.self, from: data),
              layout.version <= currentVersion
        else { return nil }
        return layout
    }

    func save(key: String, defaults: UserDefaults = AppDefaults.store) {
        guard let data = try? JSONEncoder().encode(self) else { return }
        defaults.set(data, forKey: key)
    }
}
