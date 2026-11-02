// Pure helpers of the Add Torrents sheet: the file list from
// torrents/parseMetadata or fetchMetadata and the save path whose free space
// it shows.

/** A file entry of `info.files` (5.3: `priority` 0 when excluded by name). */
export interface MetaFile {
  path: string;
  length: number;
  priority?: number;
}

/** Initially wanted files: everything except priority 0 (excluded names, magnet `so`). */
export function initialWanted(files: MetaFile[]): boolean[] {
  return files.map((f) => f.priority !== 0);
}

/**
 * `filePriorities` for torrents/add, or null when the choice equals the
 * server's own defaults (then the torrent is added as is and the core applies
 * the excluded names itself).
 */
export function prioritiesParam(initial: boolean[], wanted: boolean[]): string | null {
  if (initial.length === wanted.length && initial.every((w, i) => w === wanted[i])) return null;
  return wanted.map((w) => (w ? "1" : "0")).join(",");
}

export const isAbsolutePath = (p: string): boolean => p.startsWith("/") || /^[A-Za-z]:[\\/]/.test(p) || p.startsWith("\\\\");

/**
 * The folder the torrent would be saved in: the typed path (manual mode) or
 * the category's path (relative ones are inside the default save path), else
 * the default save path.
 */
export function effectiveSavePath(o: { auto: boolean; path: string; categoryPath?: string; defaultPath: string }): string {
  const def = o.defaultPath.trim();
  const join = (base: string, rel: string) => (base ? base.replace(/[\\/]+$/, "") + "/" + rel : rel);
  if (!o.auto && o.path.trim()) {
    const p = o.path.trim();
    return isAbsolutePath(p) ? p : join(def, p);
  }
  const c = (o.categoryPath ?? "").trim();
  if (c) return isAbsolutePath(c) ? c : join(def, c);
  return def;
}
