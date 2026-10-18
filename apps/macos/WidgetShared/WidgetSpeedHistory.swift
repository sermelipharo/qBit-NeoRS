import Foundation

/// The speed graph's samples: one every `interval` seconds, the newest
/// `capacity` kept (a value type the app updates from its refresh loop).
struct WidgetSpeedHistory: Sendable, Equatable {
    static let capacity = 60
    /// Seconds between samples (60 samples = the last 30 minutes).
    static let interval: Int64 = 30

    private(set) var samples: [WidgetSpeedSample] = []

    /// Records the current speeds if the previous sample is at least one
    /// interval old. Returns whether a sample was added.
    @discardableResult
    mutating func record(download: Int64, upload: Int64, at time: Int64) -> Bool {
        if let last = samples.last, time - last.time < Self.interval, time >= last.time { return false }
        samples.append(WidgetSpeedSample(time: time, download: max(download, 0), upload: max(upload, 0)))
        if samples.count > Self.capacity { samples.removeFirst(samples.count - Self.capacity) }
        return true
    }

    mutating func removeAll() {
        samples.removeAll()
    }
}
