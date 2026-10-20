import Foundation

/// The fields of Sort By (toolbar, View menu, Command Palette): what a card
/// shows plus the dates, in the list's column order.
extension TransferColumn {
    static let sortFields: [TransferColumn] = [
        .queuePosition, .name, .size, .progress, .status, .seeds, .peers, .downSpeed, .upSpeed,
        .eta, .ratio, .category, .addedOn, .completedOn, .lastActivity,
    ]

    /// The fields to offer now: the queue position only while queueing is
    /// on, plus the current sort column when a column header chose another one.
    static func sortFields(queueing: Bool, current: TransferColumn) -> [TransferColumn] {
        var fields = sortFields.filter { $0 != .queuePosition || queueing }
        if !fields.contains(current) { fields.append(current) }
        return fields
    }

    /// The field's name in Sort By ("#" says too little outside a header).
    var sortTitle: String {
        self == .queuePosition
            ? String(localized: "sort.field.queuePosition", defaultValue: "Queue Position", comment: "Sort By: the queue position field")
            : title
    }

    /// The Command Palette's command choosing this field.
    var sortCommand: CommandID? {
        switch self {
        case .queuePosition: .sortByQueue
        case .name: .sortByName
        case .size: .sortBySize
        case .progress: .sortByProgress
        case .status: .sortByStatus
        case .seeds: .sortBySeeds
        case .peers: .sortByPeers
        case .downSpeed: .sortByDownSpeed
        case .upSpeed: .sortByUpSpeed
        case .eta: .sortByETA
        case .ratio: .sortByRatio
        case .category: .sortByCategory
        case .addedOn: .sortByAddedOn
        case .completedOn: .sortByCompletedOn
        case .lastActivity: .sortByLastActivity
        default: nil
        }
    }
}
