import Testing

struct PastedTextTests {
    @Test func trackerListKeepsItsTiers() {
        let text = """
        udp://tracker.opentrackr.org:1337/announce
        https://tracker.example.org:443/announce

        wss://tracker.webtorrent.dev

        """
        let pasted = PastedText(text, context: .transfers)
        #expect(pasted.trackerText == """
        udp://tracker.opentrackr.org:1337/announce
        https://tracker.example.org:443/announce

        wss://tracker.webtorrent.dev
        """)
        #expect(pasted.trackerCount == 3)
        #expect(pasted.torrentLinks.isEmpty)
        #expect(pasted.peers.isEmpty)
    }

    @Test func plainHTTPIsATrackerOnlyWhereTrackersGo() {
        let line = "http://bt.example.org:2710/"
        #expect(PastedText(line, context: .transfers).torrentLinks == [line])
        #expect(PastedText(line, context: .transfers).trackerCount == 0)
        #expect(PastedText(line, context: .trackers).trackerText == line)
        #expect(PastedText("https://example.org/file.torrent", context: .trackers).trackerCount == 0)
    }

    @Test func rutrackerStyleAnnIsATrackerEverywhere() {
        let line = "http://bt4.t-ru.org/ann"
        #expect(PastedText(line, context: .transfers).trackerText == line)
        #expect(PastedText(line, context: .transfers).torrentLinks.isEmpty)
        #expect(PastedText.isTracker(line, context: .transfers))
        #expect(!PastedText.isTracker("http://example.org/announcements.torrent", context: .transfers))
    }

    @Test func peers() {
        let text = "1.2.3.4:6881\n[2001:db8::1]:51413\n1.2.3.4:6881\n300.1.1.1:1\n1.2.3.4:0\nlocalhost:80"
        #expect(PastedText(text, context: .transfers).peers == ["1.2.3.4:6881", "[2001:db8::1]:51413"])
    }

    @Test func magnetLendsItsTrackersAndPeers() {
        let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Name"
            + "&tr=udp%3A%2F%2Ftracker.example.org%3A6969%2Fannounce&tr.1=http%3A%2F%2Fbt.example.org%2Fannounce"
            + "&x.pe=10.0.0.1%3A6881&x.pe=bogus"
        let pasted = PastedText("udp://tracker.example.org:6969/announce\n" + magnet, context: .trackers)
        #expect(pasted.torrentLinks == [magnet])
        #expect(pasted.trackerText == "udp://tracker.example.org:6969/announce\nhttp://bt.example.org/announce")
        #expect(pasted.peers == ["10.0.0.1:6881"])
    }

    @Test func torrentLinksAndHashes() {
        let text = """
        https://example.org/download.php?id=1
        0123456789abcdef0123456789abcdef01234567
        some words
        """
        let pasted = PastedText(text, context: .transfers)
        #expect(pasted.torrentLinks == ["https://example.org/download.php?id=1", "0123456789abcdef0123456789abcdef01234567"])
        #expect(pasted.trackerCount == 0)
    }

    @Test func windowsLineEndingsAndIndentation() {
        let pasted = PastedText("  udp://a.example:1/announce\r\n\r\n\tudp://b.example:2/announce\r\n", context: .trackers)
        #expect(pasted.trackerText == "udp://a.example:1/announce\n\nudp://b.example:2/announce")
    }
}
