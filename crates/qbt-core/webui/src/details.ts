// Torrent details: General, Trackers, Peers and Files (with priorities).
// Rendered in the inspector column on wide screens and in a sheet on
// narrow ones; only the visible tab is fetched, on the main poll timer.

import { remove, start, stop, torrentMenu } from "./actions";
import { get, post } from "./api";
import { refreshSoon } from "./app";
import { icon } from "./icons";
import { isStopped, kind, stateText, torrents, type Torrent } from "./store";
import { formSheet, menu, toast } from "./ui";
import { bytes, collator, date, duration, h, load, num, pct, put, ratio, save, setText, speed, toggle } from "./util";
import { VList } from "./vlist";

type Tab = "general" | "trackers" | "peers" | "files";
const TABS: [Tab, string][] = [["general", "General"], ["trackers", "Trackers"], ["peers", "Peers"], ["files", "Files"]];

const PRIOS: [string, string][] = [["0", "Skip"], ["1", "Normal"], ["6", "High"], ["7", "Maximum"]];
const TRACKER_STATUS = ["Disabled", "Not contacted", "Working", "Updating", "Not working"];

interface FNode {
  path: string;
  name: string;
  depth: number;
  dir: boolean;
  size: number;
  done: number;
  prio: number;
  idx: number[];
  children: FNode[];
}

export function createDetails() {
  let tab: Tab = load("tab", "general");
  let hash: string | null = null;
  let busy = false;
  let peersRid = 0;
  const peers = new Map<string, any>();
  const expanded = new Set<string>();
  let filesHash: string | null = null;
  let fileRev = 0;

  const badge = h("span", { class: "badge" });
  const name = h("h2", { class: "d-name" });
  const sub = h("p", { class: "d-sub" });
  const bStart = h("button", { class: "icon-btn", title: "Start", "aria-label": "Start", onclick: () => cur() && start([cur()!]) }, icon("play"));
  const bStop = h("button", { class: "icon-btn", title: "Stop", "aria-label": "Stop", onclick: () => cur() && stop([cur()!]) }, icon("stop"));
  const bDel = h("button", { class: "icon-btn", title: "Remove", "aria-label": "Remove", onclick: () => cur() && remove([cur()!]) }, icon("trash"));
  const bMore = h("button", { class: "icon-btn", title: "More", "aria-label": "More actions", onclick: (e: MouseEvent) => cur() && menu(torrentMenu([cur()!]), e.currentTarget as HTMLElement) }, icon("more"));
  const tabBtns = TABS.map(([id, label]) => h("button", { role: "tab", "data-tab": id, onclick: () => setTab(id) }, label));
  const panels = Object.fromEntries(TABS.map(([id]) => [id, h("div", { class: "d-panel " + id, role: "tabpanel", tabIndex: -1 })])) as unknown as Record<Tab, HTMLElement>;
  const progress = h("div", { class: "bar big" }, h("div", { class: "fill" }));

  const el = h("div", { class: "details" },
    h("header", { class: "d-head" },
      h("div", { class: "d-title" }, badge, h("div", null, name, sub)),
      progress,
      h("div", { class: "d-actions" }, h("div", { class: "tb-group" }, bStart, bStop, bDel, bMore)),
      h("div", { class: "seg tabs", role: "tablist" }, tabBtns),
    ),
    h("div", { class: "d-panels" }, Object.values(panels)),
  );
  const placeholder = h("div", { class: "d-empty" }, icon("info"), h("p", null, "Select a torrent to see its details."));

  const cur = (): Torrent | undefined => (hash ? torrents.get(hash) : undefined);

  function setTab(t: Tab): void {
    tab = t;
    save("tab", t);
    for (const b of tabBtns) {
      const on = b.dataset.tab === t;
      toggle(b, "on", on);
      b.setAttribute("aria-selected", String(on));
    }
    for (const [id, p] of Object.entries(panels)) p.hidden = id !== t;
    if (t === "files") files.relayout();
    tick();
  }

  // ---- header ---------------------------------------------------------------------

  function head(t: Torrent): void {
    const k = kind(t.state);
    if (badge.dataset.k !== k) {
      badge.dataset.k = k;
      badge.className = "badge k-" + k;
      put(badge, icon(k === "dl" ? "dl" : k === "up" ? "ul" : k === "done" ? "done" : k === "stop" ? "stopped" : k === "err" ? "warn" : k === "check" ? "sync" : "clock"));
    }
    setText(name, t.name);
    setText(sub, `${stateText(t.state)} · ${pct(t.progress)} of ${bytes(t.size)}`);
    const fill = progress.firstElementChild as HTMLElement;
    fill.style.transform = `scaleX(${t.progress})`;
    fill.className = "fill k-" + k;
    bStart.disabled = !isStopped(t.state);
    bStop.disabled = isStopped(t.state);
  }

  // ---- General ----------------------------------------------------------------------

  function kv(rows: [string, string][]): HTMLElement {
    return h("dl", { class: "kv" }, rows.map(([k, v]) => [h("dt", null, k), h("dd", null, v)]));
  }

  async function general(t: Torrent): Promise<void> {
    const p = await get("torrents/properties", { hash: t.hash });
    const lim = (v: number) => (v > 0 ? speed(v) : "∞");
    put(panels.general,
      h("h3", null, "Transfer"),
      kv([
        ["Time active", duration(p.time_elapsed) + (p.seeding_time > 0 ? ` (seeded ${duration(p.seeding_time)})` : "")],
        ["ETA", t.progress >= 1 ? "–" : duration(p.eta)],
        ["Connections", `${p.nb_connections} (${p.nb_connections_limit} max)`],
        ["Downloaded", `${bytes(p.total_downloaded)} (${bytes(p.total_downloaded_session)} this session)`],
        ["Uploaded", `${bytes(p.total_uploaded)} (${bytes(p.total_uploaded_session)} this session)`],
        ["Seeds", `${p.seeds} (${p.seeds_total} total)`],
        ["Peers", `${p.peers} (${p.peers_total} total)`],
        ["Download speed", `${speed(p.dl_speed)} (${speed(p.dl_speed_avg)} avg.)`],
        ["Upload speed", `${speed(p.up_speed)} (${speed(p.up_speed_avg)} avg.)`],
        ["Limits", `↓ ${lim(p.dl_limit)} · ↑ ${lim(p.up_limit)}`],
        ["Share ratio", ratio(p.share_ratio)],
        ["Wasted", bytes(p.total_wasted)],
        ["Reannounce in", duration(p.reannounce)],
        ["Last seen complete", date(p.last_seen)],
      ]),
      h("h3", null, "Information"),
      kv([
        ["Total size", bytes(p.total_size)],
        ["Pieces", `${num(p.pieces_num)} × ${bytes(p.piece_size)} (have ${num(p.pieces_have)})`],
        ["Save path", p.save_path],
        ...(p.download_path ? [["Download path", p.download_path] as [string, string]] : []),
        ["Added on", date(p.addition_date)],
        ["Completed on", date(p.completion_date)],
        ["Created on", date(p.creation_date)],
        ["Created by", p.created_by || "–"],
        ["Private", p.is_private ? "Yes" : "No"],
        ["Info hash v1", p.infohash_v1 || "–"],
        ["Info hash v2", p.infohash_v2 || "–"],
        ["Comment", p.comment || "–"],
      ]),
    );
  }

  // ---- Trackers ---------------------------------------------------------------------

  async function trackers(t: Torrent): Promise<void> {
    const list: any[] = await get("torrents/trackers", { hash: t.hash });
    const addBtn = h("button", {
      class: "btn small", onclick: async () => {
        const v = await formSheet("Add trackers", [{ key: "urls", label: "Tracker URLs, one per line", type: "textarea" }], "Add");
        if (v?.urls) post("torrents/addTrackers", { hash: t.hash, urls: v.urls }).then(tick, (e) => toast(e.message, true));
      },
    }, icon("plus"), "Add trackers");
    put(panels.trackers,
      h("div", { class: "panel-tools" }, addBtn),
      h("div", { class: "cards-list" }, list.map((tr) => {
        const special = tr.url.startsWith("** [");
        const st = TRACKER_STATUS[tr.status] ?? "";
        return h("div", {
          class: "tcard",
          oncontextmenu: special ? null : (e: MouseEvent) => {
            e.preventDefault();
            trackerMenu(t, tr.url, { x: e.clientX, y: e.clientY });
          },
        },
          h("div", { class: "tcard-top" },
            h("span", { class: "tcard-url" }, special ? tr.url.replace(/\*/g, "").trim() : tr.url),
            h("span", { class: "pill s" + tr.status }, st),
            special ? null : h("button", { class: "icon-btn small", "aria-label": "Tracker actions", onclick: (e: MouseEvent) => trackerMenu(t, tr.url, e.currentTarget as HTMLElement) }, icon("more")),
          ),
          h("div", { class: "tcard-stats" },
            special ? null : h("span", null, "Tier ", String(tr.tier)),
            h("span", null, "Seeds ", num(tr.num_seeds)),
            h("span", null, "Peers ", num(tr.num_peers)),
            h("span", null, "Leeches ", num(tr.num_leeches)),
            special ? null : h("span", null, "Downloaded ", num(tr.num_downloaded)),
          ),
          tr.msg ? h("p", { class: "tcard-msg" }, tr.msg) : null,
        );
      })),
    );
  }

  function trackerMenu(t: Torrent, url: string, at: HTMLElement | { x: number; y: number }): void {
    menu([
      {
        label: "Edit URL…", icon: "pencil", run: async () => {
          const v = await formSheet("Edit tracker", [{ key: "url", label: "URL", value: url }], "Save");
          if (v?.url && v.url !== url) post("torrents/editTracker", { hash: t.hash, origUrl: url, newUrl: v.url }).then(tick, (e) => toast(e.message, true));
        },
      },
      { label: "Copy URL", icon: "doc", run: () => navigator.clipboard?.writeText(url).then(() => toast("Copied")) },
      { label: "Remove", icon: "trash", danger: true, run: () => post("torrents/removeTrackers", { hash: t.hash, urls: url }).then(tick, (e) => toast(e.message, true)) },
    ], at, url);
  }

  // ---- Peers ------------------------------------------------------------------------

  const peerRows = new Map<string, HTMLElement>();
  const peerBox = h("div", { class: "ptable", role: "table" });
  const peerTools = h("div", { class: "panel-tools" },
    h("button", {
      class: "btn small", onclick: async () => {
        const v = await formSheet("Add peers", [{ key: "peers", label: "IP:port, one per line", type: "textarea" }], "Add");
        if (v?.peers && hash) post("torrents/addPeers", { hashes: hash, peers: v.peers.split(/\s+/).filter(Boolean).join("|") }).then(tick, (e) => toast(e.message, true));
      },
    }, icon("plus"), "Add peers"),
  );
  panels.peers.append(peerTools, peerBox);

  async function peersTab(t: Torrent): Promise<void> {
    const d = await get("sync/torrentPeers", { hash: t.hash, rid: peersRid });
    if (d.full_update) peers.clear();
    peersRid = d.rid ?? 0;
    for (const id in d.peers ?? {}) peers.set(id, { ...(peers.get(id) ?? {}), ...d.peers[id] });
    for (const id of d.peers_removed ?? []) peers.delete(id);
    const sorted = [...peers.entries()].sort((a, b) => (b[1].dl_speed + b[1].up_speed) - (a[1].dl_speed + a[1].up_speed) || collator.compare(a[0], b[0]));
    if (!sorted.length) {
      put(peerBox, h("p", { class: "muted" }, "No peers connected."));
      peerRows.clear();
      return;
    }
    if (peerBox.firstElementChild?.tagName === "P") peerBox.replaceChildren();
    for (const [id, el] of peerRows) if (!peers.has(id)) {
      el.remove();
      peerRows.delete(id);
    }
    sorted.forEach(([id, p], i) => {
      let r = peerRows.get(id);
      if (!r) {
        r = h("div", {
          class: "prow", role: "row",
          oncontextmenu: (e: MouseEvent) => {
            e.preventDefault();
            menu([
              { label: "Copy IP:port", icon: "doc", run: () => navigator.clipboard?.writeText(id).then(() => toast("Copied")) },
              { label: "Ban peer", icon: "x", danger: true, run: () => post("transfer/banPeers", { peers: id }).then(tick, (er) => toast(er.message, true)) },
            ], { x: e.clientX, y: e.clientY }, id);
          },
        }, h("span", { class: "p-addr" }), h("span", { class: "p-client" }), h("span", { class: "p-prog" }), h("span", { class: "p-dl" }), h("span", { class: "p-ul" }));
        peerRows.set(id, r);
      }
      const c = r.children;
      setText(c[0], (p.country_code ? p.country_code.toUpperCase() + "  " : "") + id);
      setText(c[1], p.client || "–");
      setText(c[2], pct(p.progress ?? 0));
      setText(c[3], "↓ " + speed(p.dl_speed));
      setText(c[4], "↑ " + speed(p.up_speed));
      r.title = `${p.connection ?? ""} ${p.flags_desc ?? ""}`.trim();
      if (peerBox.children[i] !== r) peerBox.insertBefore(r, peerBox.children[i] ?? null);
    });
  }

  // ---- Files ------------------------------------------------------------------------

  let flat: FNode[] = [];
  let root: FNode | null = null;

  function buildTree(list: any[]): FNode {
    const top: FNode = { path: "", name: "", depth: -1, dir: true, size: 0, done: 0, prio: 1, idx: [], children: [] };
    const dirs = new Map<string, FNode>([["", top]]);
    for (const f of list) {
      const parts = String(f.name).split("/");
      let parent = top, path = "";
      for (let i = 0; i < parts.length - 1; i++) {
        path = path ? path + "/" + parts[i] : parts[i];
        let d = dirs.get(path);
        if (!d) {
          d = { path, name: parts[i], depth: i, dir: true, size: 0, done: 0, prio: -2, idx: [], children: [] };
          dirs.set(path, d);
          parent.children.push(d);
        }
        parent = d;
      }
      parent.children.push({ path: f.name, name: parts[parts.length - 1], depth: parts.length - 1, dir: false, size: f.size, done: f.size * f.progress, prio: f.priority, idx: [f.index], children: [] });
    }
    const roll = (n: FNode): void => {
      if (!n.dir) return;
      n.children.sort((a, b) => Number(b.dir) - Number(a.dir) || collator.compare(a.name, b.name));
      let prio = -2;
      for (const c of n.children) {
        roll(c);
        n.size += c.size;
        n.done += c.done;
        n.idx.push(...c.idx);
        prio = prio === -2 ? c.prio : prio === c.prio ? prio : -1;
      }
      n.prio = prio;
    };
    roll(top);
    return top;
  }

  function flatten(): void {
    flat = [];
    const walk = (n: FNode) => {
      for (const c of n.children) {
        flat.push(c);
        if (c.dir && expanded.has(c.path)) walk(c);
      }
    };
    if (root) walk(root);
    files.setItems(flat);
  }

  const fileBox = h("div", { class: "fscroll" });
  panels.files.append(fileBox);
  const files = new VList<FNode>(fileBox, {
    rowVar: "--file-h",
    key: (n) => n.path,
    version: () => fileRev,
    create: () => {
      const sel = h("select", { "aria-label": "Priority" }, PRIOS.map(([v, l]) => h("option", { value: v }, l)), h("option", { value: "-1", disabled: true }, "Mixed"));
      return h("div", { class: "frow" },
        h("button", { class: "f-chev", tabIndex: -1, "aria-label": "Expand" }, icon("chevR")),
        h("input", { type: "checkbox", class: "f-check", "aria-label": "Download" }),
        h("span", { class: "f-ic" }),
        h("span", { class: "f-text" }, h("span", { class: "f-name" }), h("span", { class: "f-meta" })),
        sel,
      );
    },
    update: (el, n) => {
      const [chev, check, ic, text, sel] = el.children as unknown as HTMLElement[];
      const [nm, meta] = text.children as unknown as HTMLElement[];
      el.dataset.path = n.path;
      el.style.setProperty("--depth", String(n.depth));
      chev.style.visibility = n.dir ? "" : "hidden";
      toggle(chev, "open", expanded.has(n.path));
      const cb = check as HTMLInputElement;
      cb.checked = n.prio !== 0;
      cb.indeterminate = n.prio === -1;
      if (ic.dataset.dir !== String(n.dir)) {
        ic.dataset.dir = String(n.dir);
        put(ic, icon(n.dir ? "folder" : "doc"));
      }
      setText(nm, n.name);
      nm.title = n.path;
      setText(meta, `${bytes(n.size)} · ${pct(n.size ? n.done / n.size : 1)}`);
      (sel as HTMLSelectElement).value = String(n.prio === -2 ? 1 : n.prio);
    },
  });

  const nodeOf = (e: Event) => {
    const p = ((e.target as HTMLElement).closest(".frow") as HTMLElement | null)?.dataset.path;
    return p == null ? undefined : flat.find((n) => n.path === p);
  };
  const setPrio = (n: FNode, prio: number) => {
    if (!hash) return;
    post("torrents/filePrio", { hash, id: n.idx.join("|"), priority: prio }).then(tick, (e) => toast(e.message, true));
  };
  fileBox.addEventListener("click", (e) => {
    const n = nodeOf(e);
    if (!n) return;
    const tgt = e.target as HTMLElement;
    if (tgt.closest(".f-chev") || (n.dir && tgt.closest(".f-text,.f-ic"))) {
      if (expanded.has(n.path)) expanded.delete(n.path);
      else expanded.add(n.path);
      fileRev++;
      flatten();
    }
  });
  fileBox.addEventListener("change", (e) => {
    const n = nodeOf(e);
    if (!n) return;
    const t = e.target as HTMLElement;
    if (t instanceof HTMLInputElement) setPrio(n, t.checked ? 1 : 0);
    else if (t instanceof HTMLSelectElement) setPrio(n, Number(t.value));
  });
  fileBox.addEventListener("contextmenu", (e) => {
    const n = nodeOf(e);
    if (!n || !hash) return;
    e.preventDefault();
    const h0 = hash;
    menu([
      ...PRIOS.map(([v, l]) => ({ label: l, checked: n.prio === Number(v), run: () => setPrio(n, Number(v)) })),
      "-",
      {
        label: "Rename…", icon: "pencil", run: async () => {
          const v = await formSheet("Rename", [{ key: "name", label: "Name", value: n.name }], "Rename");
          if (!v?.name || v.name === n.name) return;
          const parent = n.path.includes("/") ? n.path.slice(0, n.path.lastIndexOf("/") + 1) : "";
          post(n.dir ? "torrents/renameFolder" : "torrents/renameFile", { hash: h0, oldPath: n.path, newPath: parent + v.name }).then(tick, (er) => toast(er.message, true));
        },
      },
    ], { x: e.clientX, y: e.clientY }, n.name);
  });

  async function filesTab(t: Torrent): Promise<void> {
    const list: any[] = await get("torrents/files", { hash: t.hash });
    const first = filesHash !== t.hash;
    filesHash = t.hash;
    root = buildTree(list);
    if (first) {
      expanded.clear();
      // Expand everything for small torrents, only the top level otherwise.
      const walk = (n: FNode) => n.children.forEach((c) => c.dir && (expanded.add(c.path), list.length <= 300 && walk(c)));
      walk(root);
    }
    fileRev++;
    flatten();
  }

  // ---- refresh ----------------------------------------------------------------------

  async function tick(): Promise<void> {
    const t = cur();
    if (!t || busy || !el.isConnected) return;
    head(t);
    busy = true;
    try {
      if (tab === "general") await general(t);
      else if (tab === "trackers") await trackers(t);
      else if (tab === "peers") await peersTab(t);
      else await filesTab(t);
    } catch (e) {
      if ((e as any)?.status === 404) refreshSoon();
    } finally {
      busy = false;
    }
  }

  function show(x: string | null): void {
    if (x === hash) return;
    hash = x;
    peers.clear();
    peerRows.clear();
    peerBox.replaceChildren();
    peersRid = 0;
    filesHash = null;
    root = null;
    files.setItems([]);
    for (const id of ["general", "trackers"] as Tab[]) panels[id].replaceChildren();
    placeholder.hidden = !!x;
    el.hidden = !x;
    if (x) tick();
  }

  setTab(tab);
  el.hidden = true;
  const host = h("div", { class: "details-host" }, placeholder, el);

  return {
    el: host,
    show,
    tick,
    get hash() {
      return hash;
    },
    setPlaceholder(text: string) {
      setText(placeholder.lastElementChild!, text);
    },
    relayout: () => files.relayout(),
    onHead: () => {
      const t = cur();
      if (t) head(t);
    },
  };
}
