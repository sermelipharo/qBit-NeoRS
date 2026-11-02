// Which links may be downloaded as torrents or opened, as the macOS app's
// RSSLinkPolicy: torrents only from http(s), magnet links or info hashes;
// everything else is a web page that opens only over http(s) (never
// `javascript:`, `data:` or `file:`).

import { safeUrl } from "./util";

/** An http(s) URL, a magnet link or a bare v1/v2 info hash. */
export function isSupportedTorrentUrl(text: string): boolean {
  const s = text.trim();
  if (/^(https?:\/\/|magnet:)/i.test(s)) return true;
  return /^[0-9a-f]+$/i.test(s) && (s.length === 40 || s.length === 64);
}

export interface ArticleLinks {
  /** What "Download" adds, or null when the article has no torrent link. */
  torrent: string | null;
  /** What "Open link" opens (http(s) only), or null. */
  page: string | null;
}

/**
 * An article's torrent and web links. The torrent URL is used only when it
 * is a supported torrent source; a magnet link in `link` also counts. The
 * article link opens only when it is http(s).
 */
export function articleLinks(a: { torrentURL?: string; link?: string }): ArticleLinks {
  const t = (a.torrentURL ?? "").trim();
  const l = (a.link ?? "").trim();
  let torrent: string | null = null;
  if (t && isSupportedTorrentUrl(t)) torrent = t;
  else if (/^magnet:/i.test(l)) torrent = l;
  const page = l && !/^magnet:/i.test(l) ? safeUrl(l) : null;
  return { torrent, page };
}
