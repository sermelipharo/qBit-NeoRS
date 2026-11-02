import Foundation
import Testing

struct RSSLinkTests {
    @Test func sanitizerKeepsMagnetLinksAndDropsScripts() {
        let html = #"<a href="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567">m</a>"#
            + #"<a href="javascript:alert(1)">j</a><a href="data:text/html,x">d</a>"#
        let clean = RSSHTMLSanitizer.sanitize(html)
        #expect(clean.contains(#"href="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567""#))
        #expect(!clean.contains("javascript"))
        #expect(!clean.contains("data:"))
    }

    @Test func torrentLinksFollowQBittorrent53() throws {
        let link = { (s: String) in try #require(URL(string: s)) }
        #expect(RSSLinkPolicy.isTorrentLink(try link("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567")))
        #expect(RSSLinkPolicy.isTorrentLink(try link("https://example.org/files/show.torrent")))
        #expect(RSSLinkPolicy.isTorrentLink(try link("http://example.org/a%20b.torrent?key=1")))
        #expect(!RSSLinkPolicy.isTorrentLink(try link("https://example.org/page.html")))
        #expect(!RSSLinkPolicy.isTorrentLink(try link("ftp://example.org/x.torrent")))
        #expect(!RSSLinkPolicy.isTorrentLink(try link("file:///etc/x.torrent")))
    }
}
