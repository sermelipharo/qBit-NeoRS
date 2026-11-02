import XCTest

/// The Manage Torrent Content dialog's path edits (qBittorrent 5.3,
/// `TorrentContentLayoutModel` / `TorrentContentLayoutDialog`).
final class ContentLayoutEditsTests: XCTestCase {
    func testCleanFollowsQtPath() {
        XCTAssertEqual(ContentPath.clean("a//b/./c/"), "a/b/c")
        XCTAssertEqual(ContentPath.clean("a/../b"), "b")
        XCTAssertEqual(ContentPath.clean("a/.."), ".")
        XCTAssertEqual(ContentPath.clean("../a"), "../a")
        XCTAssertEqual(ContentPath.clean("a\\b"), "a/b")
        XCTAssertEqual(ContentPath.clean("name.mkv"), "name.mkv")
    }

    func testCommonPath() {
        XCTAssertNil(ContentPath.common([String]()))
        XCTAssertEqual(ContentPath.common(["r/a.mkv"]), "r/a.mkv")
        XCTAssertEqual(ContentPath.common(["r/s/a", "r/s/b", "r/t/c"]), "r")
        XCTAssertEqual(ContentPath.common(["r/ab", "r/abc"]), "r")
        XCTAssertEqual(ContentPath.common(["a", "b"]), "")
    }

    func testRelativeAndJoin() {
        XCTAssertEqual(ContentPath.relative("r/s/a", to: "r"), "s/a")
        XCTAssertEqual(ContentPath.relative("r/a", to: "r/a"), "")
        XCTAssertEqual(ContentPath.relative("r/a", to: ""), "r/a")
        XCTAssertEqual(ContentPath.relative("q/a", to: "r/s"), "../../q/a")
        XCTAssertEqual(ContentPath.join("x", "s/a"), "x/s/a")
        XCTAssertEqual(ContentPath.join("", "a"), "a")
        XCTAssertEqual(ContentPath.join("x/", ""), "x/")
    }

    func testSetPathTracksChanges() {
        var edits = ContentLayoutEdits(paths: ["r/a", "r/b"])
        XCTAssertTrue(edits.setPath("r//A", row: 0))
        XCTAssertEqual(edits.path(0), "r/A")
        XCTAssertTrue(edits.isChanged(0))
        XCTAssertFalse(edits.setPath("r/A", row: 0))
        XCTAssertTrue(edits.setPath("r/a", row: 0))
        XCTAssertFalse(edits.hasChanges)
        XCTAssertFalse(edits.setPath("x", row: 5))
    }

    func testCommonPathRewritesSelectedRows() {
        var edits = ContentLayoutEdits(paths: ["Show/S1/e1.mkv", "Show/S1/e2.mkv", "Other/x"])
        XCTAssertEqual(edits.commonPath(rows: [0, 1]), "Show/S1")
        edits.replaceCommonPath("Show/S1", with: "Show/Season 1", rows: [0, 1])
        XCTAssertEqual(edits.changes.map(\.path), ["Show/Season 1/e1.mkv", "Show/Season 1/e2.mkv"])
        // One file: its common path is the file itself, so the field renames it.
        edits.replaceCommonPath("Other/x", with: "Other/y", rows: [2])
        XCTAssertEqual(edits.path(2), "Other/y")
        // Clearing the field moves the files to the top.
        edits.replaceCommonPath("Show/Season 1", with: "", rows: [0])
        XCTAssertEqual(edits.path(0), "e1.mkv")
    }

    func testReloadKeepsFailedEdits() {
        var edits = ContentLayoutEdits(paths: ["a", "b"])
        edits.setPath("c", row: 0)
        edits.setPath("d", row: 1)
        edits.reload(paths: ["c", "b"], keeping: [1: "d", 0: "c"])
        XCTAssertEqual(edits.changes.map(\.row), [1])
        XCTAssertEqual(edits.path(1), "d")
    }
}
