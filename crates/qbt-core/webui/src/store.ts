// Client-side mirror of `sync/maindata`: diffs are merged into long-lived
// records (one object per torrent), and every record carries a version so
// views redraw only rows that changed.

import { collator, load, save } from "./util";

export interface Torrent {
  hash: string;
  name: string;
  size: number;
  total_size: number;
  progress: number;
  dlspeed: number;
  upspeed: number;
  state: string;
  category: string;
  tags: string;
  ratio: number;
  eta: number;
  num_seeds: number;
  num_leechs: number;
  num_complete: number;
  num_incomplete: number;
  added_on: number;
  completion_on: number;
  save_path: string;
  downloaded: number;
  uploaded: number;
  amount_left: number;
  dl_limit: number;
  up_limit: number;
  priority: number;
  seq_dl: boolean;
  f_l_piece_prio: boolean;
  force_start: boolean;
  auto_tmm: boolean;
  super_seeding: boolean;
  tracker: string;
  magnet_uri: string;
  ratio_limit: number;
  seeding_time_limit: number;
  inactive_seeding_time_limit: number;
  /** Bumped whenever a diff touches the record. */
  _v: number;
  [k: string]: any;
}

export interface Category {
  name: string;
  savePath: string;
}

export type ServerState = Record<string, any>;

export const torrents = new Map<string, Torrent>();
export const categories = new Map<string, Category>();
export const tags = new Set<string>();
/** tracker URL -> torrent hashes */
export const trackers = new Map<string, string[]>();
export let server: ServerState = {};
let rid = 0;

export interface Change {
  full: boolean;
  /** Torrents were added or removed. */
  structure: boolean;
  /** Torrent fields that changed (union). */
  keys: Set<string>;
  /** Categories, tags or trackers changed. */
  labels: boolean;
  server: boolean;
}

export function resetRid(): void {
  rid = 0;
}

export function currentRid(): number {
  return rid;
}

export function apply(d: any): Change {
  const ch: Change = { full: !!d.full_update, structure: false, keys: new Set(), labels: false, server: false };
  rid = d.rid ?? rid;
  if (d.full_update) {
    torrents.clear();
    categories.clear();
    tags.clear();
    trackers.clear();
    server = {};
    ch.structure = ch.labels = ch.server = true;
  }
  if (d.torrents) {
    for (const hash in d.torrents) {
      const diff = d.torrents[hash];
      const t = torrents.get(hash);
      if (t) {
        Object.assign(t, diff);
        t._v++;
        for (const k in diff) ch.keys.add(k);
      } else {
        torrents.set(hash, { hash, ...diff, _v: 1 } as Torrent);
        ch.structure = true;
      }
    }
  }
  for (const hash of d.torrents_removed ?? []) {
    torrents.delete(hash);
    ch.structure = true;
  }
  if (d.categories) {
    for (const name in d.categories) categories.set(name, { ...(categories.get(name) ?? { name, savePath: "" }), ...d.categories[name] });
    ch.labels = true;
  }
  for (const name of d.categories_removed ?? []) {
    categories.delete(name);
    ch.labels = true;
  }
  for (const t of d.tags ?? []) {
    tags.add(t);
    ch.labels = true;
  }
  for (const t of d.tags_removed ?? []) {
    tags.delete(t);
    ch.labels = true;
  }
  if (d.trackers) {
    for (const url in d.trackers) trackers.set(url, d.trackers[url]);
    ch.labels = true;
  }
  for (const url of d.trackers_removed ?? []) {
    trackers.delete(url);
    ch.labels = true;
  }
  if (d.server_state) {
    server = { ...server, ...d.server_state };
    ch.server = true;
  }
  return ch;
}

// ---- states ------------------------------------------------------------------------

export type Kind = "dl" | "up" | "done" | "stop" | "check" | "queue" | "stall" | "meta" | "move" | "err";

export function kind(state: string): Kind {
  switch (state) {
    case "downloading":
    case "forcedDL":
      return "dl";
    case "uploading":
    case "forcedUP":
      return "up";
    case "stalledUP":
      return "done";
    case "stoppedUP":
      return "done";
    case "stoppedDL":
      return "stop";
    case "checkingUP":
    case "checkingDL":
    case "checkingResumeData":
    case "allocating":
      return "check";
    case "queuedUP":
    case "queuedDL":
      return "queue";
    case "stalledDL":
      return "stall";
    case "metaDL":
    case "forcedMetaDL":
      return "meta";
    case "moving":
      return "move";
    default:
      return "err";
  }
}

const STATE_TEXT: Record<string, string> = {
  downloading: "Downloading",
  forcedDL: "[F] Downloading",
  metaDL: "Downloading metadata",
  forcedMetaDL: "[F] Downloading metadata",
  stalledDL: "Stalled",
  queuedDL: "Queued",
  stoppedDL: "Stopped",
  checkingDL: "Checking",
  uploading: "Seeding",
  forcedUP: "[F] Seeding",
  stalledUP: "Seeding",
  queuedUP: "Queued",
  stoppedUP: "Completed",
  checkingUP: "Checking",
  checkingResumeData: "Checking resume data",
  allocating: "Allocating",
  moving: "Moving",
  missingFiles: "Missing files",
  error: "Errored",
  unknown: "Unknown",
};

export const stateText = (s: string): string => STATE_TEXT[s] ?? s;

export const isStopped = (s: string): boolean => s === "stoppedDL" || s === "stoppedUP";
const DOWNLOADING = new Set(["downloading", "metaDL", "forcedMetaDL", "stalledDL", "checkingDL", "stoppedDL", "queuedDL", "forcedDL"]);
const UPLOADING = new Set(["uploading", "stalledUP", "checkingUP", "queuedUP", "forcedUP"]);
const COMPLETED = new Set(["uploading", "stalledUP", "checkingUP", "stoppedUP", "queuedUP", "forcedUP"]);
const ACTIVE = new Set(["metaDL", "forcedMetaDL", "downloading", "forcedDL", "uploading", "forcedUP"]);

const isActive = (t: Torrent) => (t.state === "stalledDL" ? t.upspeed > 0 : ACTIVE.has(t.state));

export const STATUS: [id: string, label: string, icon: string, test: (t: Torrent) => boolean][] = [
  ["all", "All", "tray", () => true],
  ["downloading", "Downloading", "dl", (t) => DOWNLOADING.has(t.state)],
  ["seeding", "Seeding", "ul", (t) => UPLOADING.has(t.state)],
  ["completed", "Completed", "done", (t) => COMPLETED.has(t.state)],
  ["running", "Running", "run", (t) => !isStopped(t.state)],
  ["stopped", "Stopped", "stopped", (t) => isStopped(t.state)],
  ["active", "Active", "bolt", isActive],
  ["inactive", "Inactive", "moon", (t) => !isActive(t)],
  ["stalled", "Stalled", "clock", (t) => t.state === "stalledUP" || t.state === "stalledDL"],
  ["checking", "Checking", "sync", (t) => t.state === "checkingUP" || t.state === "checkingDL" || t.state === "checkingResumeData"],
  ["moving", "Moving", "move", (t) => t.state === "moving"],
  ["errored", "Errored", "warn", (t) => t.state === "error" || t.state === "unknown" || t.state === "missingFiles"],
];

// ---- filtering ----------------------------------------------------------------------

export interface Filter {
  status: string;
  /** null = any, "" = uncategorized */
  category: string | null;
  /** null = any, "" = untagged */
  tag: string | null;
  /** null = any, "" = trackerless, else host */
  tracker: string | null;
  text: string;
}

export const filter: Filter = { status: load("status", "all"), category: load("category", null), tag: load("tag", null), tracker: null, text: "" };

export function saveFilter(): void {
  save("status", filter.status);
  save("category", filter.category);
  save("tag", filter.tag);
}

export const tagList = (t: Torrent): string[] => (t.tags ? t.tags.split(",").map((s) => s.trim()).filter(Boolean) : []);

export function hostOf(url: string): string {
  try {
    return new URL(url).hostname || url;
  } catch {
    return url;
  }
}

/** host -> hashes */
export function trackerHosts(): Map<string, Set<string>> {
  const m = new Map<string, Set<string>>();
  for (const [url, hashes] of trackers) {
    const host = hostOf(url);
    let s = m.get(host);
    if (!s) m.set(host, (s = new Set()));
    for (const h of hashes) s.add(h);
  }
  return m;
}

function matchesCategory(t: Torrent, c: string): boolean {
  // Subcategories ("a/b") are part of their parent.
  return t.category === c || (c !== "" && t.category.startsWith(c + "/"));
}

export function makePredicate(f: Filter, hosts: Map<string, Set<string>>, skip?: keyof Filter): (t: Torrent) => boolean {
  const st = STATUS.find((s) => s[0] === f.status)?.[3] ?? (() => true);
  const words = f.text.toLowerCase().split(/\s+/).filter(Boolean);
  const trackerSet = f.tracker ? hosts.get(f.tracker) : null;
  let anyTracker: Set<string> | null = null;
  if (f.tracker === "" && skip !== "tracker") {
    anyTracker = new Set();
    for (const s of hosts.values()) for (const h of s) anyTracker.add(h);
  }
  return (t) => {
    if (skip !== "status" && !st(t)) return false;
    if (skip !== "category" && f.category !== null && !matchesCategory(t, f.category)) return false;
    if (skip !== "tag" && f.tag !== null) {
      const tl = tagList(t);
      if (f.tag === "" ? tl.length > 0 : !tl.includes(f.tag)) return false;
    }
    if (skip !== "tracker" && f.tracker !== null) {
      if (f.tracker === "" ? anyTracker!.has(t.hash) : !trackerSet?.has(t.hash)) return false;
    }
    if (words.length) {
      const n = t.name.toLowerCase();
      for (const w of words) if (!n.includes(w)) return false;
    }
    return true;
  };
}

// ---- sorting -----------------------------------------------------------------------

export interface Sort {
  key: string;
  asc: boolean;
}

export const sort: Sort = load("sort", { key: "added_on", asc: false });

export function compare(key: string): (a: Torrent, b: Torrent) => number {
  if (key === "name" || key === "category" || key === "tags" || key === "tracker" || key === "save_path") {
    return (a, b) => collator.compare(a[key] ?? "", b[key] ?? "");
  }
  if (key === "state") return (a, b) => collator.compare(stateText(a.state), stateText(b.state));
  if (key === "eta") {
    const v = (t: Torrent) => (t.eta >= 8640000 || t.eta < 0 ? Infinity : t.eta);
    return (a, b) => v(a) - v(b);
  }
  if (key === "priority") {
    const v = (t: Torrent) => (t.priority > 0 ? t.priority : Infinity);
    return (a, b) => v(a) - v(b);
  }
  return (a, b) => (a[key] ?? 0) - (b[key] ?? 0);
}

export function sortList(list: Torrent[]): void {
  const c = compare(sort.key), name = compare("name"), dir = sort.asc ? 1 : -1;
  list.sort((a, b) => c(a, b) * dir || name(a, b));
}
