// Search: run a query through the search plugins, stream results into a
// virtualized table, download or open results; manage plugins.

import { ApiError, get, post } from "./api";
import { app, emit, on, refreshSoon } from "./app";
import { icon } from "./icons";
import { control, formSheet, group, menu, sheet, toast } from "./ui";
import { bytes, collator, h, num, put, safeUrl, setText, shortDate, toggle } from "./util";
import { VList } from "./vlist";

interface Result {
  fileName: string;
  fileUrl: string;
  fileSize: number;
  nbSeeders: number;
  nbLeechers: number;
  engineName: string;
  siteUrl: string;
  descrLink: string;
  pubDate: number;
  _i: number;
}

interface Plugin {
  name: string;
  fullName: string;
  enabled: boolean;
  version: string;
  url: string;
  supportedCategories: { id: string; name: string }[];
}

const COLS: [string, string, string][] = [
  ["fileName", "Name", "name"], ["fileSize", "Size", "num size"], ["nbSeeders", "Seeds", "num seeds"],
  ["nbLeechers", "Leechers", "num leech"], ["engineName", "Engine", "engine"], ["pubDate", "Published", "num added"],
];

export function createSearch() {
  let plugins: Plugin[] = [];
  let id: number | null = null;
  let running = false;
  let results: Result[] = [];
  let shown: Result[] = [];
  let sortKey = "nbSeeders", asc = false;
  let timer = 0;

  const pattern = h("input", { type: "search", placeholder: "Search torrents", "aria-label": "Search for", spellcheck: false, autocomplete: "off", enterkeyhint: "search" });
  const cat = h("select", { "aria-label": "Category" }, h("option", { value: "all" }, "All categories"));
  const plug = h("select", { "aria-label": "Plugins" }, h("option", { value: "enabled" }, "Enabled plugins"), h("option", { value: "all" }, "All plugins"));
  const go = h("button", { type: "submit", class: "btn primary" }, "Search");
  const narrowFilter = h("input", { type: "search", placeholder: "Filter results", "aria-label": "Filter results", spellcheck: false });
  const status = h("p", null, "Search the web with your search plugins");
  const form = h("form", { class: "search-form" }, h("label", { class: "searchbox big" }, icon("search"), pattern), cat, plug, go);

  const thead = h("div", { class: "thead", role: "row" }, COLS.map(([k, l, cls]) =>
    h("button", { class: "c " + cls, "data-k": k, onclick: () => { asc = sortKey === k ? !asc : k === "fileName" || k === "engineName"; sortKey = k; apply(); } }, h("span", null, l), icon("chevD", "sort-ind")),
  ));
  const header = h("header", { class: "topbar" },
    h("div", { class: "toolbar" },
      h("button", { class: "icon-btn", "aria-label": "Sidebar", title: "Sidebar", onclick: () => emit("sidebar") }, icon("sidebar")),
      h("div", { class: "titles" }, h("h1", null, "Search"), status),
      h("label", { class: "searchbox wide-only" }, icon("search"), narrowFilter),
      h("button", { class: "btn small", onclick: () => pluginsSheet() }, "Plugins"),
    ),
    form,
    thead,
  );
  const scroller = h("div", { class: "scroller", tabIndex: 0, role: "grid", "aria-label": "Search results" });
  const empty = h("div", { class: "empty" }, icon("search"), h("p", null, "No results yet"), h("small", null, "Type a query and press Search."));
  const el = h("section", { class: "view search", hidden: true }, header, scroller, empty);

  const list = new VList<Result>(scroller, {
    rowVar: "--row-h",
    key: (r) => String(r._i),
    version: () => 0,
    create: () => h("div", { class: "row srow", role: "row" },
      h("div", { class: "c name" }, h("span", { class: "title" })),
      h("div", { class: "c num size" }), h("div", { class: "c num seeds" }), h("div", { class: "c num leech" }),
      h("div", { class: "c engine" }), h("div", { class: "c num added" }),
      h("div", { class: "c meta" }),
    ),
    update: (row, r, i) => {
      const c = row.children;
      setText(c[0].firstElementChild!, r.fileName);
      (c[0] as HTMLElement).title = r.fileName;
      setText(c[1], bytes(r.fileSize));
      setText(c[2], num(r.nbSeeders));
      setText(c[3], num(r.nbLeechers));
      setText(c[4], r.engineName);
      setText(c[5], r.pubDate > 0 ? shortDate(r.pubDate) : "–");
      setText(c[6], `${bytes(r.fileSize)} · ${num(r.nbSeeders)} seeds · ${num(r.nbLeechers)} leechers · ${r.engineName}`);
      row.dataset.i = String(r._i);
      toggle(row, "odd", i % 2 === 1);
    },
  });

  function apply(): void {
    const words = narrowFilter.value.toLowerCase().split(/\s+/).filter(Boolean);
    shown = words.length ? results.filter((r) => words.every((w) => r.fileName.toLowerCase().includes(w))) : results.slice();
    const dir = asc ? 1 : -1;
    shown.sort((a, b) => {
      const x = (a as any)[sortKey], y = (b as any)[sortKey];
      return (typeof x === "string" ? collator.compare(x, y) : x - y) * dir;
    });
    list.setItems(shown);
    empty.hidden = shown.length > 0;
    for (const b of thead.children) {
      const onK = (b as HTMLElement).dataset.k === sortKey;
      toggle(b, "sorted", onK);
      toggle(b, "asc", onK && asc);
    }
    setText(status, id == null ? "Search the web with your search plugins" : `${running ? "Searching… " : ""}${results.length} results${shown.length !== results.length ? `, ${shown.length} shown` : ""}`);
    go.textContent = running ? "Stop" : "Search";
  }

  async function loadPlugins(): Promise<void> {
    try {
      plugins = await get("search/plugins");
    } catch {
      plugins = [];
    }
    const cats = new Map<string, string>();
    for (const p of plugins) if (p.enabled) for (const c of p.supportedCategories ?? []) cats.set(c.id, c.name);
    const keep = cat.value;
    put(cat, h("option", { value: "all" }, "All categories"), [...cats].filter(([k]) => k !== "all").map(([k, v]) => h("option", { value: k }, v)));
    cat.value = keep && [...cat.options].some((o) => o.value === keep) ? keep : "all";
    const keepP = plug.value;
    put(plug, h("option", { value: "enabled" }, "Enabled plugins"), h("option", { value: "all" }, "All plugins"),
      plugins.filter((p) => p.enabled).map((p) => h("option", { value: p.name }, p.fullName || p.name)));
    plug.value = [...plug.options].some((o) => o.value === keepP) ? keepP : "enabled";
    if (!plugins.length) {
      setText(empty.children[1], "No search plugins");
      setText(empty.children[2], "Install plugins with the Plugins button. Search needs Python 3 on the server.");
    }
  }

  async function poll(): Promise<void> {
    clearTimeout(timer);
    if (id == null) return;
    try {
      const r = await get("search/results", { id, offset: results.length, limit: 500 });
      for (const x of r.results ?? []) results.push({ ...x, _i: results.length });
      running = r.status === "Running";
      apply();
    } catch (e) {
      running = false;
      apply();
      if ((e as ApiError).status !== 404) toast((e as Error).message, true);
      return;
    }
    if (running && app.section === "search") timer = window.setTimeout(poll, Math.max(1000, app.interval));
  }

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    if (running && id != null) {
      await post("search/stop", { id }).catch(() => {});
      running = false;
      apply();
      return;
    }
    const q = pattern.value.trim();
    if (!q) return pattern.focus();
    if (id != null) post("search/delete", { id }).catch(() => {});
    results = [];
    try {
      const r = await post("search/start", { pattern: q, category: cat.value, plugins: plug.value });
      id = r.id;
      running = true;
      apply();
      timer = window.setTimeout(poll, 800);
    } catch (er) {
      toast((er as Error).message, true);
    }
  });
  narrowFilter.addEventListener("input", apply);

  const resultAt = (e: Event) => {
    const i = ((e.target as HTMLElement).closest(".srow") as HTMLElement | null)?.dataset.i;
    return i == null ? undefined : results[Number(i)];
  };
  const download = (r: Result) =>
    post("search/downloadTorrent", { torrentUrl: r.fileUrl, pluginName: r.engineName })
      .then(() => (toast("Download started"), refreshSoon()), (er) => toast(er.message, true));
  const resultMenu = (r: Result, at: { x: number; y: number }) => {
    const link = safeUrl(r.descrLink);
    menu([
      { label: "Download", icon: "down", run: () => download(r) },
      { label: "Open description page", icon: "link", disabled: !link, run: () => link && window.open(link, "_blank", "noopener,noreferrer") },
      { label: "Copy link", icon: "doc", run: () => navigator.clipboard?.writeText(r.fileUrl).then(() => toast("Copied")) },
    ], at, r.fileName);
  };
  scroller.addEventListener("click", (e) => {
    const r = resultAt(e);
    if (r) resultMenu(r, { x: (e as MouseEvent).clientX, y: (e as MouseEvent).clientY });
  });
  scroller.addEventListener("contextmenu", (e) => {
    const r = resultAt(e);
    if (!r) return;
    e.preventDefault();
    resultMenu(r, { x: e.clientX, y: e.clientY });
  });

  async function pluginsSheet(): Promise<void> {
    await loadPlugins();
    const body = h("div", null);
    const render = () =>
      put(body, plugins.length
        ? group("Installed", plugins.map((p) => {
            const sw = control({ key: p.name, label: p.fullName, type: "bool", value: p.enabled }) as HTMLInputElement;
            sw.addEventListener("change", () => post("search/enablePlugin", { names: p.name, enable: sw.checked }).then(loadPlugins, (e) => toast(e.message, true)));
            const link = safeUrl(p.url);
            return h("div", { class: "fr" },
              h("span", { class: "row-label" }, p.fullName || p.name, h("small", null, `${p.name} ${p.version}${link ? " · " + new URL(link).host : ""}`)),
              h("span", { class: "row-ctl" },
                h("button", {
                  type: "button", class: "icon-btn small", "aria-label": "Uninstall " + p.name, title: "Uninstall",
                  onclick: (e: MouseEvent) => {
                    e.preventDefault();
                    post("search/uninstallPlugin", { names: p.name }).then(async () => { await loadPlugins(); render(); }, (er) => toast(er.message, true));
                  },
                }, icon("trash")),
                sw,
              ));
          }))
        : h("p", { class: "muted pad" }, "No plugins installed. Search plugins are Python scripts; install one from a URL or a path on the server."));
    render();
    sheet({
      title: "Search Plugins",
      cls: "small",
      body,
      actions: [
        {
          label: "Install…",
          run: async () => {
            const v = await formSheet("Install plugin", [{ key: "src", label: "URL or server path", placeholder: "https://…/plugin.py" }], "Install");
            if (v?.src) await post("search/installPlugin", { sources: v.src }).then(() => toast("Installing plugin…"), (e) => toast(e.message, true));
            setTimeout(loadPlugins, 2000);
          },
        },
        { label: "Update all", run: () => post("search/updatePlugins").then(() => toast("Checking for updates…"), (e) => toast(e.message, true)) },
        { label: "Done", primary: true },
      ],
    });
  }

  on("section", () => {
    el.hidden = app.section !== "search";
    if (!el.hidden) {
      loadPlugins();
      list.relayout();
      apply();
      if (running) poll();
    }
  });

  return { el };
}
