// Unit tests of the Web UI's pure logic: RSS link policy, the Add sheet's
// file priorities and save path, the transfer list's secondary sort and
// "Copy path". Run with scripts/test-webui.sh.

import assert from "node:assert/strict";
import { test } from "node:test";

import { effectiveSavePath, initialWanted, prioritiesParam } from "../src/addfiles.ts";
import { articleLinks, isSupportedTorrentUrl } from "../src/links.ts";
import { comparator, sortBy } from "../src/store.ts";
import { joinPath } from "../src/util.ts";

test("RSS: only http(s), magnet links and info hashes are downloadable", () => {
  assert.ok(isSupportedTorrentUrl("https://example.com/a.torrent"));
  assert.ok(isSupportedTorrentUrl("HTTP://example.com/page"));
  assert.ok(isSupportedTorrentUrl("magnet:?xt=urn:btih:abc"));
  assert.ok(isSupportedTorrentUrl("0123456789abcdef0123456789abcdef01234567"));
  assert.ok(isSupportedTorrentUrl("a".repeat(64)));
  assert.ok(!isSupportedTorrentUrl("a".repeat(41)));
  assert.ok(!isSupportedTorrentUrl("javascript:alert(1)"));
  assert.ok(!isSupportedTorrentUrl("data:application/x-bittorrent;base64,AAAA"));
  assert.ok(!isSupportedTorrentUrl("file:///etc/passwd"));
  assert.ok(!isSupportedTorrentUrl("ftp://example.com/a.torrent"));
});

test("RSS: article links", () => {
  assert.deepEqual(articleLinks({ torrentURL: "https://x/a.torrent", link: "https://x/page" }), { torrent: "https://x/a.torrent", page: "https://x/page" });
  // A magnet in `link` downloads and is not opened as a page.
  assert.deepEqual(articleLinks({ link: "magnet:?xt=urn:btih:abc" }), { torrent: "magnet:?xt=urn:btih:abc", page: null });
  // Unsupported torrent URL: nothing to download; the page still opens.
  assert.deepEqual(articleLinks({ torrentURL: "javascript:alert(1)", link: "https://x/page" }), { torrent: null, page: "https://x/page" });
  // Never opens javascript:, data: or file: links.
  assert.deepEqual(articleLinks({ link: "javascript:alert(1)" }), { torrent: null, page: null });
  assert.deepEqual(articleLinks({ link: "data:text/html,<b>x</b>" }), { torrent: null, page: null });
  assert.deepEqual(articleLinks({ torrentURL: "", link: "" }), { torrent: null, page: null });
});

test("Add sheet: excluded files start unchecked; priorities only when changed", () => {
  const files = [{ path: "a/movie.mkv", length: 10, priority: 1 }, { path: "a/sample.mkv", length: 2, priority: 0 }, { path: "a/x.nfo", length: 1 }];
  const initial = initialWanted(files);
  assert.deepEqual(initial, [true, false, true]);
  assert.equal(prioritiesParam(initial, initial.slice()), null);
  assert.equal(prioritiesParam(initial, [true, true, true]), "1,1,1");
  assert.equal(prioritiesParam(initial, [false, false, true]), "0,0,1");
});

test("Add sheet: save path for the free space", () => {
  const def = "/Users/me/Downloads";
  assert.equal(effectiveSavePath({ auto: false, path: "/Volumes/x", defaultPath: def }), "/Volumes/x");
  assert.equal(effectiveSavePath({ auto: false, path: "", defaultPath: def }), def);
  assert.equal(effectiveSavePath({ auto: false, path: "sub", defaultPath: def + "/" }), def + "/sub");
  assert.equal(effectiveSavePath({ auto: true, path: "/ignored", categoryPath: "Movies", defaultPath: def }), def + "/Movies");
  assert.equal(effectiveSavePath({ auto: true, path: "", categoryPath: "/Volumes/m", defaultPath: def }), "/Volumes/m");
  assert.equal(effectiveSavePath({ auto: true, path: "", defaultPath: def }), def);
});

test("Transfer list: the previous column becomes the secondary sort", () => {
  const t = (name, size, ratio) => ({ name, size, ratio });
  const list = [t("b", 1, 2), t("a", 2, 1), t("c", 1, 1), t("d", 2, 3)];
  const s = { key: "ratio", asc: false };
  sortBy(s, "size", true);
  assert.deepEqual(s.then, { key: "ratio", asc: false });
  assert.deepEqual(list.slice().sort(comparator(s)).map((x) => x.name), ["b", "c", "d", "a"]);
  // Same column again: only the direction changes.
  sortBy(s, "size", false);
  assert.deepEqual(s.then, { key: "ratio", asc: false });
  assert.deepEqual(list.slice().sort(comparator(s)).map((x) => x.name), ["d", "a", "b", "c"]);
  // Back to the secondary column: no secondary on itself.
  sortBy(s, "ratio", true);
  assert.deepEqual(s.then, { key: "size", asc: false });
  // Without a secondary, the name breaks ties.
  assert.deepEqual(list.slice().sort(comparator({ key: "size", asc: true })).map((x) => x.name), ["b", "c", "a", "d"]);
});

test("Copy path joins the save path and the file's path", () => {
  assert.equal(joinPath("/Users/me/Downloads", "Show/S01/e1.mkv"), "/Users/me/Downloads/Show/S01/e1.mkv");
  assert.equal(joinPath("/Users/me/Downloads/", "a.iso"), "/Users/me/Downloads/a.iso");
  assert.equal(joinPath("C:\\Downloads", "Show/e1.mkv"), "C:\\Downloads\\Show\\e1.mkv");
  assert.equal(joinPath("/", "a.iso"), "/a.iso");
});
