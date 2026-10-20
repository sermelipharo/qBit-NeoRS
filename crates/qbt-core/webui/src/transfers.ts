// Transfer list: toolbar, sortable list or cards (virtualized), selection.

import { remove, selected, start, stop, torrentMenu } from "./actions";
import { app, emit, on } from "./app";
import { icon } from "./icons";
import { type Change, filter, kind, makePredicate, sort, sortList, STATUS, stateText, torrents, trackerHosts, type Torrent } from "./store";
import { type Chip, contentKind, emphasizesTitles, parsedName, showsCardTags } from "./names";
import { KIND_TITLES } from "./contentkind";
import { menu } from "./ui";
import { bytes, debounce, eta, h, isNarrow, pct, ratio, save, setText, shortDate, speed, toggle } from "./util";
import { VList } from "./vlist";

const COLS: [key: string, label: string, cls: string][] = [
  ["name", "Name", "name"],
  ["size", "Size", "num size"],
  ["progress", "Progress", "prog"],
  ["state", "Status", "state"],
  ["dlspeed", "Down", "num dl"],
  ["upspeed", "Up", "num ul"],
  ["eta", "ETA", "num eta"],
  ["ratio", "Ratio", "num ratio"],
  ["added_on", "Added", "num added"],
];

const SORTS: [string, string][] = [
  ["name", "Name"], ["added_on", "Date added"], ["size", "Size"], ["progress", "Progress"], ["state", "Status"],
  ["dlspeed", "Download speed"], ["upspeed", "Upload speed"], ["eta", "ETA"], ["ratio", "Ratio"], ["priority", "Queue position"],
];

const KIND_ICON: Record<string, string> = {
  dl: "dl", up: "ul", done: "done", stop: "stopped", check: "sync", queue: "clock", stall: "clock", meta: "magnet", move: "move", err: "warn",
};

interface Cells {
  badge: HTMLElement;
  kind: string;
  name: HTMLElement;
  size: HTMLElement;
  fill: HTMLElement;
  pct: HTMLElement;
  state: HTMLElement;
  dl: HTMLElement;
  ul: HTMLElement;
  eta: HTMLElement;
  ratio: HTMLElement;
  added?: HTMLElement;
  meta?: HTMLElement;
  chips?: HTMLElement;
  tags?: HTMLElement;
  labels?: HTMLElement;
  contentKind?: HTMLElement;
}
const cells = new WeakMap<HTMLElement, Cells>();
/** What an element shows, so unchanged content is not rebuilt on every poll. */
const shown = new WeakMap<Element, string>();

const CONTENT_ICON: Record<string, string> = {
  video: "film", music: "music", software: "app", game: "game", rom: "chip", audiobook: "headphones", book: "book", comic: "comic", adult: "adult", other: "doc",
};

/** The list's name: the parsed title semibold, the technical rest regular (like the app). */
function setListName(el: HTMLElement, name: string): void {
  const p = emphasizesTitles() ? parsedName(name) : null;
  const sig = p && p.titleEnd > p.titleStart ? `${p.titleStart}:${p.titleEnd}:${name}` : "\u0000" + name;
  if (shown.get(el) === sig) return;
  shown.set(el, sig);
  toggle(el, "parsed", !!p && p.titleEnd > p.titleStart);
  if (!p || p.titleEnd <= p.titleStart) el.textContent = name;
  else el.replaceChildren(name.slice(0, p.titleStart), h("span", { class: "tt" }, name.slice(p.titleStart, p.titleEnd)), name.slice(p.titleEnd));
}

function setContentKind(el: HTMLElement, kind: string): void {
  if (shown.get(el) === kind) return;
  shown.set(el, kind);
  el.dataset.kind = kind;
  el.title = KIND_TITLES[kind as keyof typeof KIND_TITLES] ?? "";
  el.setAttribute("aria-label", el.title);
  el.replaceChildren(icon(CONTENT_ICON[kind] ?? "doc"));
}

/** The name's tags in their category's style, then the torrent's category and tags. */
function setCardChips(c: Cells, chips: Chip[], category: string, tags: string[]): void {
  const sig = JSON.stringify([chips, category, tags]);
  if (shown.get(c.chips!) === sig) return;
  shown.set(c.chips!, sig);
  c.tags!.replaceChildren(...chips.map((x) => h("span", { class: "chip", "data-c": x.category }, x.text)));
  c.labels!.replaceChildren(
    ...(category ? [h("span", { class: "chip lbl" }, icon("folder"), category)] : []),
    ...tags.map((t) => h("span", { class: "chip lbl" }, icon("tag"), t)),
  );
}

function setBadge(c: Cells, k: string): void {
  if (c.kind === k) return;
  c.kind = k;
  c.badge.className = "badge k-" + k;
  c.badge.replaceChildren(icon(KIND_ICON[k]));
}

function setFill(c: Cells, p: number, k: string): void {
  const v = String(Math.max(0, Math.min(1, p)));
  if (c.fill.dataset.p !== v) {
    c.fill.dataset.p = v;
    c.fill.style.transform = `scaleX(${v})`;
  }
  if (c.fill.dataset.k !== k) {
    c.fill.dataset.k = k;
    c.fill.className = "fill k-" + k;
  }
}

function createRow(): HTMLElement {
  const c = {} as Cells;
  const el = h("div", { class: "row", role: "option" },
    h("div", { class: "c name" }, (c.badge = h("span", { class: "badge" })), (c.name = h("span", { class: "title" }))),
    (c.size = h("div", { class: "c num size" })),
    h("div", { class: "c prog" }, h("div", { class: "bar" }, (c.fill = h("div", { class: "fill" }))), (c.pct = h("span", { class: "pct" }))),
    (c.state = h("div", { class: "c state" })),
    (c.dl = h("div", { class: "c num dl" })),
    (c.ul = h("div", { class: "c num ul" })),
    (c.eta = h("div", { class: "c num eta" })),
    (c.ratio = h("div", { class: "c num ratio" })),
    (c.added = h("div", { class: "c num added" })),
    (c.meta = h("div", { class: "c meta" })),
  );
  c.kind = "";
  cells.set(el, c);
  return el;
}

function createCard(): HTMLElement {
  const c = {} as Cells;
  const el = h("div", { class: "card", role: "option" },
    h("div", { class: "card-top" },
      (c.contentKind = h("span", { class: "content-kind", role: "img" })),
      (c.name = h("div", { class: "title" })),
      (c.badge = h("span", { class: "badge" })),
    ),
    (c.chips = h("div", { class: "chips" }, (c.tags = h("span", { class: "name-tags" })), (c.labels = h("span", { class: "labels" })))),
    h("div", { class: "bar" }, (c.fill = h("div", { class: "fill" }))),
    h("div", { class: "card-line" }, (c.size = h("span", null)), (c.pct = h("span", { class: "pct" }))),
    h("div", { class: "card-line" }, (c.state = h("span", { class: "state" })), h("span", { class: "speeds" }, icon("down"), (c.dl = h("span", null)), icon("up"), (c.ul = h("span", null)))),
    h("div", { class: "card-line dim" }, h("span", null, "Ratio ", (c.ratio = h("span", null))), h("span", null, "ETA ", (c.eta = h("span", null)))),
  );
  c.kind = "";
  cells.set(el, c);
  return el;
}

function fillCommon(el: HTMLElement, t: Torrent): Cells {
  const c = cells.get(el)!;
  const k = kind(t.state);
  setBadge(c, k);
  setFill(c, t.progress, k);
  setText(c.pct, pct(t.progress));
  setText(c.state, stateText(t.state));
  c.state.dataset.k = k;
  setText(c.dl, speed(t.dlspeed));
  setText(c.ul, speed(t.upspeed));
  setText(c.eta, t.progress >= 1 ? "–" : eta(t.eta));
  setText(c.ratio, ratio(t.ratio));
  toggle(el, "sel", app.selection.has(t.hash));
  el.setAttribute("aria-selected", String(app.selection.has(t.hash)));
  el.dataset.hash = t.hash;
  return c;
}

function updateRow(el: HTMLElement, t: Torrent, i: number): void {
  const c = fillCommon(el, t);
  setListName(c.name, t.name);
  setText(c.size, bytes(t.size));
  setText(c.added!, shortDate(t.added_on));
  const parts = [stateText(t.state), t.progress >= 1 ? bytes(t.size) : `${bytes(t.size - t.amount_left)} of ${bytes(t.size)}`];
  if (t.dlspeed > 0) parts.push("↓ " + speed(t.dlspeed));
  if (t.upspeed > 0) parts.push("↑ " + speed(t.upspeed));
  setText(c.meta!, parts.join(" · "));
  toggle(el, "odd", i % 2 === 1);
  toggle(el, "check", app.selectMode);
}

function updateCard(el: HTMLElement, t: Torrent): void {
  const c = fillCommon(el, t);
  const parsed = parsedName(t.name);
  setText(c.name, parsed ? parsed.title : t.name);
  if (c.name.title !== t.name) c.name.title = t.name;
  setContentKind(c.contentKind!, contentKind(t.name, t.category));
  setText(c.size, t.progress >= 1 ? bytes(t.size) : `${bytes(t.size - t.amount_left)} of ${bytes(t.size)}`);
  const tags = t.tags ? t.tags.split(",").map((s) => s.trim()).filter(Boolean) : [];
  setCardChips(c, parsed && showsCardTags() ? parsed.chips : [], t.category, tags);
}

export function createTransfers() {
  const titleEl = h("h1", null);
  const subEl = h("p", null);
  const search = h("input", { type: "search", placeholder: "Search", "aria-label": "Filter torrents by name", spellcheck: false, autocomplete: "off" });
  const btn = (ic: string, label: string, onclick: (e: MouseEvent) => void, cls = "") =>
    h("button", { class: "icon-btn " + cls, title: label, "aria-label": label, onclick }, icon(ic));

  const bStart = btn("play", "Start", () => start());
  const bStop = btn("stop", "Stop", () => stop());
  const bDel = btn("trash", "Remove", () => remove());
  const bMore = btn("more", "More actions", (e) => menu(torrentMenu(), e.currentTarget as HTMLElement));
  const bList = btn("list", "List", () => setView("list"));
  const bCards = btn("cards", "Cards", () => setView("cards"));
  const bSort = btn("arrows", "Sort", (e) =>
    menu([
      ...SORTS.map(([k, l]) => ({ label: l, checked: sort.key === k, run: () => setSort(k, sort.key === k ? sort.asc : k === "name") })),
      "-",
      { label: "Ascending", checked: sort.asc, run: () => setSort(sort.key, true) },
      { label: "Descending", checked: !sort.asc, run: () => setSort(sort.key, false) },
    ], e.currentTarget as HTMLElement, "Sort by"),
  );
  const bInfo = btn("info", "Show details", () => {
    app.inspector = !app.inspector;
    save("inspector", app.inspector);
    emit("inspector");
    syncToolbar();
  }, "wide-only");
  const bSelect = h("button", { class: "text-btn narrow-only", onclick: () => setSelectMode(!app.selectMode) }, "Select");
  const bAdd = btn("plus", "Add torrents", () => emit("add"), "accent");
  const bSidebar = btn("sidebar", "Sidebar", () => emit("sidebar"));

  const thead = h("div", { class: "thead" });
  for (const [k, l, cls] of COLS) {
    thead.append(h("button", { class: "c " + cls, "data-k": k, onclick: () => setSort(k, sort.key === k ? !sort.asc : k === "name") }, h("span", null, l), icon("chevD", "sort-ind")));
  }

  const header = h("header", { class: "topbar" },
    h("div", { class: "toolbar" },
      bSidebar,
      h("div", { class: "titles" }, titleEl, h("div", { class: "subline" }, subEl)),
      h("div", { class: "tb-actions" },
        h("div", { class: "tb-group" }, bAdd),
        h("div", { class: "tb-group sel-actions" }, bStart, bStop, bDel, bMore),
        h("div", { class: "tb-group seg" }, bList, bCards),
        h("div", { class: "tb-group" }, bSort),
      ),
      h("label", { class: "searchbox" }, icon("search"), search),
      h("div", { class: "tb-group wide-only" }, bInfo),
      bSelect,
    ),
    thead,
  );

  const scroller = h("div", { class: "scroller", tabIndex: 0, role: "listbox", "aria-label": "Torrents", "aria-multiselectable": "true" });
  const empty = h("div", { class: "empty", hidden: true }, icon("tray"), h("p", null, "No torrents"), h("small", null, "Drop .torrent files here or press + to add."));
  const selbar = h("div", { class: "selbar" },
    btn("play", "Start", () => start()), btn("stop", "Stop", () => stop()), btn("trash", "Remove", () => remove()),
    btn("more", "More actions", (e) => menu(torrentMenu(), e.currentTarget as HTMLElement)),
  );
  const el = h("section", { class: "view transfers" }, header, scroller, empty, selbar);

  const key = (t: Torrent) => t.hash;
  const version = (t: Torrent) => t._v;
  const list = new VList<Torrent>(scroller, { rowVar: "--row-h", create: createRow, update: updateRow, key, version });
  const cards = new VList<Torrent>(scroller, {
    rowVar: "--card-h", create: createCard, update: (e, t) => updateCard(e, t), key, version,
    minWidth: () => (isNarrow() ? 0 : 290), gap: () => (isNarrow() ? 0 : 14),
  });
  let items: Torrent[] = [];
  const active = () => (app.view === "cards" ? cards : list);

  function setView(v: "list" | "cards"): void {
    app.view = v;
    save("view", v);
    el.dataset.view = v;
    const off = v === "cards" ? list : cards;
    off.setItems([]);
    off.canvas.hidden = true;
    active().canvas.hidden = false;
    active().relayout();
    active().setItems(items);
    syncToolbar();
  }

  function setSort(k: string, asc: boolean): void {
    sort.key = k;
    sort.asc = asc;
    save("sort", sort);
    refresh();
  }

  function setSelectMode(on: boolean): void {
    app.selectMode = on;
    if (!on) app.selection.clear();
    setText(bSelect, on ? "Done" : "Select");
    selectionChanged();
  }

  function syncToolbar(): void {
    const n = app.selection.size;
    for (const b of [bStart, bStop, bDel, bMore]) b.disabled = n === 0;
    toggle(bList, "on", app.view === "list");
    toggle(bCards, "on", app.view === "cards");
    bList.setAttribute("aria-pressed", String(app.view === "list"));
    bCards.setAttribute("aria-pressed", String(app.view === "cards"));
    toggle(bInfo, "on", app.inspector);
    toggle(el, "selecting", app.selectMode);
    toggle(el, "has-sel", n > 0);
    for (const b of thead.children) {
      const on = (b as HTMLElement).dataset.k === sort.key;
      toggle(b, "sorted", on);
      toggle(b, "asc", on && sort.asc);
      b.setAttribute("aria-sort", on ? (sort.asc ? "ascending" : "descending") : "none");
    }
    const st = STATUS.find((s) => s[0] === filter.status)?.[1] ?? "All";
    const scope = [filter.category === "" ? "Uncategorized" : filter.category, filter.tag === "" ? "Untagged" : filter.tag, filter.tracker === "" ? "Trackerless" : filter.tracker].filter((x) => x != null);
    setText(titleEl, filter.status === "all" && scope.length ? String(scope[0]) : st === "All" ? "All Transfers" : st);
    const count = `${items.length} ${items.length === 1 ? "torrent" : "torrents"}`;
    const extra = filter.status !== "all" && scope.length ? scope.join(" · ") + " · " : scope.length > 1 ? scope.slice(1).join(" · ") + " · " : "";
    setText(subEl, extra + count + (n ? ` · ${n} selected` : ""));
  }

  function selectionChanged(): void {
    list.invalidate();
    cards.invalidate();
    active().render();
    syncToolbar();
    emit("selection");
  }

  let lastFilterSig = "";
  function refresh(ch?: Change): void {
    const sig = JSON.stringify(filter) + sort.key + sort.asc;
    const keys = ch?.keys;
    const needs = !ch || ch.structure || sig !== lastFilterSig || (ch.labels && filter.tracker !== null) ||
      !!keys && (keys.has(sort.key) || keys.has("state") || keys.has("category") || keys.has("tags") || keys.has("name") ||
        (keys.has("upspeed") && (filter.status === "active" || filter.status === "inactive")));
    lastFilterSig = sig;
    if (needs) {
      const pred = makePredicate(filter, trackerHosts());
      items = [];
      for (const t of torrents.values()) if (pred(t)) items.push(t);
      sortList(items);
      let dropped = false;
      for (const x of app.selection) {
        if (!torrents.has(x)) {
          app.selection.delete(x);
          dropped = true;
        }
      }
      if (dropped) emit("selection");
      active().setItems(items);
      empty.hidden = items.length > 0;
      setText(empty.children[1], torrents.size ? "No matching torrents" : "No torrents");
    } else active().render();
    syncToolbar();
  }

  // ---- pointer / keyboard -------------------------------------------------------

  const hashAt = (e: Event) => ((e.target as HTMLElement).closest(".row,.card") as HTMLElement | null)?.dataset.hash;

  function selectOnly(x: string): void {
    app.selection.clear();
    app.selection.add(x);
    app.anchor = x;
  }

  scroller.addEventListener("click", (e) => {
    const x = hashAt(e);
    if (!x) {
      if (!app.selectMode && e.target === scroller) {
        app.selection.clear();
        selectionChanged();
      }
      return;
    }
    if (app.selectMode) {
      if (app.selection.has(x)) app.selection.delete(x);
      else app.selection.add(x);
    } else if (e.shiftKey && app.anchor) {
      const a = items.findIndex((t) => t.hash === app.anchor), b = items.findIndex((t) => t.hash === x);
      if (a >= 0 && b >= 0) {
        if (!(e.metaKey || e.ctrlKey)) app.selection.clear();
        for (let i = Math.min(a, b); i <= Math.max(a, b); i++) app.selection.add(items[i].hash);
      }
    } else if (e.metaKey || e.ctrlKey) {
      if (app.selection.has(x)) app.selection.delete(x);
      else app.selection.add(x);
      app.anchor = x;
    } else {
      selectOnly(x);
      if (isNarrow()) emit("open-details");
    }
    selectionChanged();
  });

  scroller.addEventListener("dblclick", (e) => {
    const x = hashAt(e);
    if (!x || isNarrow()) return;
    selectOnly(x);
    selectionChanged();
    emit("open-details");
  });

  scroller.addEventListener("contextmenu", (e) => {
    const x = hashAt(e);
    if (!x) return;
    e.preventDefault();
    if (!app.selection.has(x)) {
      selectOnly(x);
      selectionChanged();
    }
    menu(torrentMenu(), { x: e.clientX, y: e.clientY }, selected().length === 1 ? selected()[0].name : `${app.selection.size} torrents`);
  });

  scroller.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!items.length) return;
      const cur = items.findIndex((t) => t.hash === app.anchor);
      const next = Math.max(0, Math.min(items.length - 1, cur < 0 ? 0 : cur + (e.key === "ArrowDown" ? 1 : -1)));
      const x = items[next].hash;
      if (e.shiftKey) {
        app.selection.add(x);
        app.anchor = x;
      } else selectOnly(x);
      active().scrollToIndex(next);
      selectionChanged();
    } else if ((e.key === "a" || e.key === "A") && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      for (const t of items) app.selection.add(t.hash);
      selectionChanged();
    } else if (e.key === "Delete" || (e.key === "Backspace" && (e.metaKey || !e.ctrlKey))) {
      if (app.selection.size) {
        e.preventDefault();
        remove();
      }
    } else if (e.key === "Enter" && app.selection.size === 1) {
      emit("open-details");
    } else if (e.key === "Escape" && app.selection.size) {
      app.selection.clear();
      selectionChanged();
    }
  });

  search.addEventListener("input", debounce(() => {
    filter.text = search.value;
    refresh();
  }, 120));

  on("filter", () => {
    scroller.scrollTop = 0;
    refresh();
  });
  // Name rules arrived or changed: redraw every name.
  on("names", () => {
    list.invalidate();
    cards.invalidate();
    active().render();
  });
  on("narrow", () => {
    active().relayout();
    syncToolbar();
  });

  el.dataset.view = app.view;
  (app.view === "cards" ? list : cards).canvas.hidden = true;

  return {
    el,
    refresh,
    focusSearch: () => search.focus(),
    selectionChanged,
    setSelectMode,
  };
}
