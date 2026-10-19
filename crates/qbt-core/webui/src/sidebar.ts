// Sidebar: status / category / tag / tracker filters with counts, and the
// Search and RSS sections. Items are keyed and updated in place.

import { newCategory, newTag } from "./actions";
import { post } from "./api";
import { app, emit, refreshSoon, type Section } from "./app";
import { icon } from "./icons";
import { categories, filter, saveFilter, STATUS, tagList, tags, torrents, trackerHosts } from "./store";
import { formSheet, menu, toast } from "./ui";
import { collator, h, load, save, setText, toggle } from "./util";

interface Item {
  id: string;
  label: string;
  icon: string;
  count?: number;
  active: boolean;
  onclick: () => void;
  oncontext?: (e: MouseEvent) => void;
}

const collapsed: Record<string, boolean> = load("collapsed", {});

function section(id: string, title: string, add?: () => void) {
  const list = h("div", { class: "sb-list", role: "list" });
  const head = h("div", { class: "sb-head" },
    h("button", {
      class: "sb-title",
      "aria-expanded": String(!collapsed[id]),
      onclick: () => {
        collapsed[id] = !collapsed[id];
        save("collapsed", collapsed);
        toggle(el, "collapsed", collapsed[id]);
        head.firstElementChild!.setAttribute("aria-expanded", String(!collapsed[id]));
      },
    }, title, icon("chevD", "sb-chev")),
    add ? h("button", { class: "sb-add", "aria-label": "New", title: "New", onclick: add }, icon("plus")) : null,
  );
  const el = h("section", { class: "sb-sec" + (collapsed[id] ? " collapsed" : "") }, head, list);
  return { el, list };
}

const nodes = new WeakMap<HTMLElement, Map<string, HTMLElement>>();

function sync(list: HTMLElement, items: Item[]): void {
  let map = nodes.get(list);
  if (!map) nodes.set(list, (map = new Map()));
  const seen = new Set<string>();
  items.forEach((it, i) => {
    seen.add(it.id);
    let el = map!.get(it.id);
    if (!el) {
      el = h("button", { class: "sb-item", role: "listitem" }, icon(it.icon), h("span", { class: "sb-label" }), h("span", { class: "sb-count" }));
      map!.set(it.id, el);
    }
    el.onclick = it.onclick;
    el.oncontextmenu = it.oncontext ?? null;
    el.title = it.label;
    setText(el.children[1], it.label);
    setText(el.children[2], it.count == null ? "" : String(it.count));
    toggle(el, "active", it.active);
    if (it.active) el.setAttribute("aria-current", "true");
    else el.removeAttribute("aria-current");
    if (list.children[i] !== el) list.insertBefore(el, list.children[i] ?? null);
  });
  for (const [id, el] of map) {
    if (!seen.has(id)) {
      el.remove();
      map.delete(id);
    }
  }
}

export function createSidebar(onNavigate: () => void) {
  const status = section("status", "Status");
  const tools = section("tools", "Tools");
  const cats = section("cats", "Categories", () => newCategory());
  const tagSec = section("tags", "Tags", () => newTag());
  const trk = section("trackers", "Trackers");
  const el = h("nav", { class: "sidebar", "aria-label": "Filters" },
    h("div", { class: "sb-brand" }, h("img", { src: "icon.svg", alt: "", width: 22, height: 22 }), h("span", null, "qBit NeoRS")),
    h("div", { class: "sb-scroll" }, status.el, tools.el, cats.el, tagSec.el, trk.el),
  );

  const go = (s: Section) => {
    app.section = s;
    emit("section");
    onNavigate();
  };
  const setF = (k: "status" | "category" | "tag" | "tracker", v: string | null) => {
    (filter as any)[k] = v;
    saveFilter();
    if (app.section !== "transfers") app.section = "transfers";
    emit("section");
    emit("filter");
    onNavigate();
  };

  const catMenu = (name: string) => (e: MouseEvent) => {
    e.preventDefault();
    menu([
      {
        label: "Edit…", icon: "pencil", run: async () => {
          const v = await formSheet("Edit category", [{ key: "path", label: "Save path", value: categories.get(name)?.savePath ?? "" }], "Save");
          if (v) post("torrents/editCategory", { category: name, savePath: v.path }).then(refreshSoon, (er) => toast(er.message, true));
        },
      },
      { label: "Remove", icon: "trash", danger: true, run: () => post("torrents/removeCategories", { categories: name }).then(refreshSoon, (er) => toast(er.message, true)) },
    ], { x: e.clientX, y: e.clientY }, name);
  };
  const tagMenu = (name: string) => (e: MouseEvent) => {
    e.preventDefault();
    menu([{ label: "Delete tag", icon: "trash", danger: true, run: () => post("torrents/deleteTags", { tags: name }).then(refreshSoon, (er) => toast(er.message, true)) }], { x: e.clientX, y: e.clientY }, name);
  };

  function render(): void {
    const all = [...torrents.values()];
    const onT = app.section === "transfers";
    const statusItems: Item[] = STATUS.map(([id, label, ic, test]) => {
      let n = 0;
      for (const t of all) if (test(t)) n++;
      return { id, label, icon: ic, count: n, active: onT && filter.status === id, onclick: () => setF("status", id) };
    }).filter((it) => it.count! > 0 || !["checking", "moving", "errored", "stalled"].includes(it.id) || it.active);
    sync(status.list, statusItems);

    sync(tools.list, [
      { id: "search", label: "Search", icon: "search", active: app.section === "search", onclick: () => go("search") },
      { id: "rss", label: "RSS", icon: "rss", active: app.section === "rss", onclick: () => go("rss") },
    ]);

    const catCount = new Map<string, number>();
    let uncategorized = 0;
    for (const t of all) {
      if (!t.category) uncategorized++;
      else {
        // Parents count their subcategories' torrents.
        const parts = t.category.split("/");
        for (let i = 1; i <= parts.length; i++) {
          const k = parts.slice(0, i).join("/");
          catCount.set(k, (catCount.get(k) ?? 0) + 1);
        }
      }
    }
    sync(cats.list, [
      { id: "\0all", label: "All", icon: "tray", count: all.length, active: onT && filter.category === null, onclick: () => setF("category", null) },
      { id: "\0none", label: "Uncategorized", icon: "folder", count: uncategorized, active: onT && filter.category === "", onclick: () => setF("category", "") },
      ...[...categories.keys()].sort(collator.compare).map((c) => ({
        id: c, label: c, icon: "folder", count: catCount.get(c) ?? 0, active: onT && filter.category === c,
        onclick: () => setF("category", c), oncontext: catMenu(c),
      })),
    ]);

    const tagCount = new Map<string, number>();
    let untagged = 0;
    for (const t of all) {
      const tl = tagList(t);
      if (!tl.length) untagged++;
      for (const x of tl) tagCount.set(x, (tagCount.get(x) ?? 0) + 1);
    }
    sync(tagSec.list, [
      { id: "\0all", label: "All", icon: "tray", count: all.length, active: onT && filter.tag === null, onclick: () => setF("tag", null) },
      { id: "\0none", label: "Untagged", icon: "tag", count: untagged, active: onT && filter.tag === "", onclick: () => setF("tag", "") },
      ...[...tags].sort(collator.compare).map((x) => ({
        id: x, label: x, icon: "tag", count: tagCount.get(x) ?? 0, active: onT && filter.tag === x,
        onclick: () => setF("tag", x), oncontext: tagMenu(x),
      })),
    ]);

    const hosts = trackerHosts();
    const tracked = new Set<string>();
    for (const s of hosts.values()) for (const x of s) tracked.add(x);
    sync(trk.list, [
      { id: "\0all", label: "All", icon: "tray", count: all.length, active: onT && filter.tracker === null, onclick: () => setF("tracker", null) },
      { id: "\0none", label: "Trackerless", icon: "antenna", count: all.filter((t) => !tracked.has(t.hash)).length, active: onT && filter.tracker === "", onclick: () => setF("tracker", "") },
      ...[...hosts.keys()].sort(collator.compare).map((x) => ({
        id: x, label: x, icon: "globe", count: hosts.get(x)!.size, active: onT && filter.tracker === x, onclick: () => setF("tracker", x),
      })),
    ]);
  }

  return { el, render };
}
