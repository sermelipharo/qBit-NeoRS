import Foundation

/// One point of the widgets' speed graph, encoded as `[time, download, upload]`.
struct WidgetSpeedSample: Codable, Hashable, Sendable {
    /// Unix seconds.
    var time: Int64
    var download: Int64
    var upload: Int64

    var date: Date { Date(timeIntervalSince1970: TimeInterval(time)) }

    init(time: Int64, download: Int64, upload: Int64) {
        self.time = time
        self.download = download
        self.upload = upload
    }

    init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        time = try container.decode(Int64.self)
        download = try container.decode(Int64.self)
        upload = try container.decode(Int64.self)
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.unkeyedContainer()
        try container.encode(time)
        try container.encode(download)
        try container.encode(upload)
    }
}
