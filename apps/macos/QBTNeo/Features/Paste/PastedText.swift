import Foundation

/// What a pasted text holds, line by line: torrent links, tracker URLs and
/// peer addresses. A magnet link also lends its trackers (`tr`) and peers
/// (`x.pe`). Where the text is pasted decides which of them is used.
struct PastedText: Equatable {
    enum Context {
        /// The transfer list: http(s) URLs are torrent links unless they announce.
        case transfers
        /// A torrent's trackers: any http(s), udp or ws(s) URL is a tracker.
        case trackers
    }

    /// Magnets, URLs, `.torrent` paths and info hashes, in order.
    var torrentLinks: [String] = []
    /// Tracker URLs one per line; blank lines between them start a new tier
    /// (`TrackerTierParser`). The magnets' trackers come last.
    var trackerText = ""
    /// `IPv4:port` and `[IPv6]:port`, without duplicates.
    var peers: [String] = []

    var trackerCount: Int {
        trackerText.split(whereSeparator: \.isNewline).count
    }

    init(_ text: String, context: Context) {
        var trackerLines: [String] = []
        var magnetTrackers: [String] = []
        var seenPeers = Set<String>()
        func addPeer(_ peer: String) {
            if seenPeers.insert(peer).inserted { peers.append(peer) }
        }
        for raw in text.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces.union(.controlCharacters))
            if line.isEmpty {
                // Keep tier breaks only between trackers.
                if trackerLines.last?.isEmpty == false { trackerLines.append("") }
                continue
            }
            if line.lowercased().hasPrefix("magnet:") {
                torrentLinks.append(line)
                let parts = Self.magnetParts(line)
                magnetTrackers.append(contentsOf: parts.trackers)
                parts.peers.filter(Self.isPeerAddress).forEach(addPeer)
            } else if Self.isPeerAddress(line) {
                addPeer(line)
            } else if Self.isTracker(line, context: context) {
                trackerLines.append(line)
            } else if Self.isDownloadable(line) {
                torrentLinks.append(line)
            }
        }
        while trackerLines.last?.isEmpty == true { trackerLines.removeLast() }
        let known = Set(trackerLines)
        let extra = magnetTrackers.reduce(into: [String]()) { list, url in
            if !known.contains(url), !list.contains(url) { list.append(url) }
        }
        trackerText = (trackerLines + extra).joined(separator: "\n")
    }

    // MARK: Recognizers

    /// A tracker URL: udp and ws(s) always; http(s) when it announces, or
    /// anywhere a tracker is expected (but not a `.torrent` file).
    static func isTracker(_ line: String, context: Context) -> Bool {
        guard let url = URL(string: line), let scheme = url.scheme?.lowercased(), url.host() != nil else { return false }
        switch scheme {
        case "udp", "ws", "wss":
            return true
        case "http", "https":
            let path = url.path(percentEncoded: false).lowercased()
            if path.hasSuffix(".torrent") { return false }
            return context == .trackers || path.contains("announce")
        default:
            return false
        }
    }

    /// `PeerAddress::parse`: `[v6]:port` or `v4:port` with a non-zero port.
    static func isPeerAddress(_ line: String) -> Bool {
        let host: Substring
        let port: Substring
        if line.hasPrefix("[") {
            guard let close = line.firstIndex(of: "]"), line[line.index(after: close)...].hasPrefix(":") else { return false }
            host = line[line.index(after: line.startIndex)..<close]
            port = line[line.index(close, offsetBy: 2)...]
            guard host.contains(":"), host.allSatisfy({ $0.isHexDigit || $0 == ":" || $0 == "." }) else { return false }
        } else {
            let parts = line.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count == 2 else { return false }
            host = parts[0]
            port = parts[1]
            let octets = host.split(separator: ".", omittingEmptySubsequences: false)
            guard octets.count == 4, octets.allSatisfy({ !$0.isEmpty && $0.allSatisfy(\.isASCII) && UInt8($0) != nil }) else {
                return false
            }
        }
        guard port.allSatisfy(\.isASCII), let number = UInt16(port), number > 0 else { return false }
        return true
    }

    /// Lines the links dialog takes: URLs, magnets and bare v1 (hex/base32)
    /// or v2 info hashes.
    static func isDownloadable(_ line: String) -> Bool {
        let lower = line.lowercased()
        if lower.hasPrefix("magnet:") || lower.hasPrefix("http:") || lower.hasPrefix("https:")
            || lower.hasPrefix("ftp:") || lower.hasPrefix("file:") || lower.hasPrefix("data:") {
            return true
        }
        let hex = line.allSatisfy(\.isHexDigit)
        if hex, line.count == 40 || line.count == 64 { return true }
        let base32 = line.allSatisfy { ("a"..."z").contains($0.lowercased()) || ("2"..."7").contains($0) }
        return base32 && line.count == 32
    }

    /// A magnet link's trackers (`tr`, `tr.1`, …) and peers (`x.pe`).
    static func magnetParts(_ magnet: String) -> (trackers: [String], peers: [String]) {
        guard let query = magnet.firstIndex(of: "?").map({ magnet[magnet.index(after: $0)...] }) else { return ([], []) }
        var trackers: [String] = []
        var peers: [String] = []
        for pair in query.split(separator: "&") {
            guard let equals = pair.firstIndex(of: "=") else { continue }
            let key = pair[..<equals].lowercased()
            let raw = String(pair[pair.index(after: equals)...]).replacingOccurrences(of: "+", with: " ")
            guard let value = raw.removingPercentEncoding?.trimmingCharacters(in: .whitespaces), !value.isEmpty else { continue }
            if key == "tr" || key.hasPrefix("tr.") {
                if isTracker(value, context: .trackers), !trackers.contains(value) { trackers.append(value) }
            } else if key == "x.pe" {
                peers.append(value)
            }
        }
        return (trackers, peers)
    }
}
