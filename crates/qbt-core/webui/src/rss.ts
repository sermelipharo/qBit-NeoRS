// RSS reader: feeds, articles and the article text. Descriptions are HTML
// from the feed; they are parsed in an inert document and shown only as
// plain text.

import { openAdd } from "./add";
import { get, post } from "./api";
import { app, emit, on } from "./app";
import { icon } from "./icons";
import { confirmSheet, formSheet, menu, toast } from "./ui";
import { collator, date, h, put, safeUrl, setText, shortDate, toggle } from "./util";
import { VList } from "./vlist";

interface Article {
  id: string;
  title: string;
  date: string;
  author?: string;
  description?: string;
  torrentURL?: string;
  link?: string;
  isRead?: boolean;
  _feed: string;
  _feedTitle: string;
  _ts: number;
  _key: string;
}

interface Feed {
  path: string;
  title: string;
  url: string;
  isLoading: boolean;
  hasError: boolean;
  articles: Article[];
  depth: number;
}

const UNREAD = "\0unread";

export function htmlToText(html: string): string {
  if (!/[<&]/.test(html)) return html.trim();
  const doc = new DOMParser().parseFromString(html, "text/html");
  doc.querySelectorAll("script,style,template").forEach((n) => n.remove());
  doc.querySelectorAll("br").forEach((n) => n.replaceWith("\n"));
  doc.querySelectorAll("p,div,li,tr,h1,h2,h3,h4,h5,h6,blockquote,pre").forEach((n) => n.append("\n"));
  return (doc.body?.textContent ?? "").replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
}

export function createRss() {
  let feeds: Feed[] = [];
  let folders: { path: string; title: string; depth: number }[] = [];
  let current = UNREAD;
  let articles: Article[] = [];
  let open: Article | null = null;
  let timer = 0;
  let rev = 0;

  const feedList = h("div", { class: "rss-feeds", role: "list" });
  const artScroll = h("div", { class: "rss-articles scroller", tabIndex: 0, role: "list" });
  const reader = h("article", { class: "rss-reader" });
  const titleEl = h("h1", null, "RSS");
  const subEl = h("p", null);
  const back = h("button", { class: "icon-btn narrow-only back", "aria-label": "Back", onclick: () => setLevel(level - 1) }, icon("chevL"));
  let level = 0;

  const header = h("header", { class: "topbar" },
    h("div", { class: "toolbar" },
      h("button", { class: "icon-btn", "aria-label": "Sidebar", title: "Sidebar", onclick: () => emit("sidebar") }, icon("sidebar")),
      back,
      h("div", { class: "titles" }, titleEl, subEl),
      h("div", { class: "tb-actions" },
        h("div", { class: "tb-group" },
          h("button", { class: "icon-btn", title: "New feed", "aria-label": "New feed", onclick: addFeed }, icon("plus")),
          h("button", { class: "icon-btn", title: "Refresh all", "aria-label": "Refresh all feeds", onclick: () => refreshItem("") }, icon("sync")),
          h("button", { class: "icon-btn", title: "Mark all as read", "aria-label": "Mark all as read", onclick: markAllRead }, icon("check")),
        ),
      ),
    ),
  );
  const el = h("section", { class: "view rss", hidden: true, "data-level": "0" }, header,
    h("div", { class: "rss-cols" }, feedList, artScroll, reader));

  function setLevel(l: number): void {
    level = Math.max(0, l);
    el.dataset.level = String(level);
    back.hidden = level === 0;
  }

  const artList = new VList<Article>(artScroll, {
    rowVar: "--art-h",
    key: (a) => a._key,
    version: () => rev,
    create: () => h("div", { class: "art", role: "listitem" }, h("span", { class: "dot" }), h("div", { class: "art-title" }), h("div", { class: "art-meta" })),
    update: (row, a) => {
      toggle(row, "unread", !a.isRead);
      toggle(row, "sel", open?._key === a._key);
      setText(row.children[1], a.title);
      setText(row.children[2], `${a._feedTitle} · ${a._ts ? shortDate(a._ts) : a.date}`);
      row.dataset.key = a._key;
    },
  });

  function parse(tree: any): void {
    feeds = [];
    folders = [];
    const walk = (node: any, prefix: string, depth: number) => {
      const names = Object.keys(node).sort(collator.compare);
      for (const name of names) {
        const v = node[name];
        const path = prefix ? prefix + "\\" + name : name;
        if (v && typeof v === "object" && "url" in v) {
          const f: Feed = { path, title: v.title || name, url: v.url, isLoading: !!v.isLoading, hasError: !!v.hasError, articles: [], depth };
          f.articles = (v.articles ?? []).map((a: any) => {
            const ts = Date.parse(a.date);
            return { ...a, _feed: path, _feedTitle: f.title, _ts: Number.isFinite(ts) ? Math.floor(ts / 1000) : 0, _key: path + "\n" + a.id };
          });
          feeds.push(f);
        } else if (v && typeof v === "object") {
          folders.push({ path, title: name, depth });
          walk(v, path, depth + 1);
        }
      }
    };
    walk(tree ?? {}, "", 0);
  }

  function renderFeeds(): void {
    const unread = feeds.reduce((n, f) => n + f.articles.filter((a) => !a.isRead).length, 0);
    const item = (id: string, label: string, ic: string, count: number, depth = 0, cls = "") => {
      const b = h("button", {
        class: "sb-item " + cls + (current === id ? " active" : ""), role: "listitem",
        onclick: () => {
          current = id;
          open = null;
          showArticles();
          renderFeeds();
          setLevel(1);
        },
        oncontextmenu: id === UNREAD ? null : (e: MouseEvent) => {
          e.preventDefault();
          feedMenu(id, { x: e.clientX, y: e.clientY });
        },
      }, icon(ic), h("span", { class: "sb-label" }, label), h("span", { class: "sb-count" }, count ? String(count) : ""));
      b.style.paddingInlineStart = 8 + depth * 14 + "px";
      return b;
    };
    const rows: HTMLElement[] = [item(UNREAD, "Unread", "tray", unread)];
    const byFolder = new Map<string, Feed[]>();
    for (const f of feeds) {
      const parent = f.path.includes("\\") ? f.path.slice(0, f.path.lastIndexOf("\\")) : "";
      (byFolder.get(parent) ?? byFolder.set(parent, []).get(parent)!).push(f);
    }
    const feedItem = (f: Feed) => item(f.path, f.title, f.hasError ? "warn" : f.isLoading ? "sync" : "rss", f.articles.filter((a) => !a.isRead).length, f.depth, f.hasError ? "error" : "");
    for (const f of byFolder.get("") ?? []) rows.push(feedItem(f));
    for (const d of folders) {
      rows.push(item(d.path, d.title, "folder", 0, d.depth, "folder"));
      for (const f of byFolder.get(d.path) ?? []) rows.push(feedItem(f));
    }
    if (!feeds.length) rows.push(h("p", { class: "muted pad" }, "No feeds yet. Press + to subscribe to a feed."));
    put(feedList, rows);
    setText(subEl, feeds.length ? `${feeds.length} feeds · ${unread} unread` : "");
  }

  function showArticles(): void {
    if (current === UNREAD) articles = feeds.flatMap((f) => f.articles.filter((a) => !a.isRead || open?._key === a._key));
    else articles = feeds.filter((f) => f.path === current || f.path.startsWith(current + "\\")).flatMap((f) => f.articles);
    articles.sort((a, b) => b._ts - a._ts);
    rev++;
    artList.setItems(articles);
    setText(titleEl, current === UNREAD ? "Unread" : feeds.find((f) => f.path === current)?.title ?? current.split("\\").pop() ?? "RSS");
    renderReader();
  }

  function renderReader(): void {
    if (!open) {
      put(reader, h("div", { class: "empty static" }, icon("rss"), h("p", null, articles.length ? "Select an article" : "No articles")));
      return;
    }
    const a = open;
    const link = a.link ? safeUrl(a.link) : null;
    const torrent = a.torrentURL || (a.link?.startsWith("magnet:") ? a.link : "");
    put(reader,
      h("p", { class: "eyebrow" }, a._feedTitle),
      h("h2", null, a.title),
      h("p", { class: "muted" }, [a._ts ? date(a._ts) : a.date, a.author].filter(Boolean).join(" · ")),
      h("div", { class: "reader-actions" },
        torrent ? h("button", { class: "btn primary", onclick: () => openAdd({ urls: torrent }) }, icon("down"), "Download") : null,
        link ? h("button", { class: "btn", onclick: () => window.open(link, "_blank", "noopener,noreferrer") }, icon("link"), "Open link") : null,
      ),
      h("div", { class: "reader-text" }, htmlToText(a.description ?? "") || "No description."),
    );
  }

  async function load(): Promise<void> {
    clearTimeout(timer);
    try {
      parse(await get("rss/items", { withData: true }));
      if (current !== UNREAD && !feeds.some((f) => f.path === current || f.path.startsWith(current + "\\"))) current = UNREAD;
      if (open) open = feeds.flatMap((f) => f.articles).find((a) => a._key === open!._key) ?? null;
      renderFeeds();
      showArticles();
    } catch (e) {
      toast((e as Error).message, true);
    }
    if (app.section === "rss" && !document.hidden) timer = window.setTimeout(load, Math.max(10000, app.interval * 8));
  }

  artScroll.addEventListener("click", (e) => {
    const k = ((e.target as HTMLElement).closest(".art") as HTMLElement | null)?.dataset.key;
    const a = articles.find((x) => x._key === k);
    if (!a) return;
    open = a;
    if (!a.isRead) {
      a.isRead = true;
      post("rss/markAsRead", { itemPath: a._feed, articleId: a.id }).catch(() => {});
      renderFeeds();
    }
    rev++;
    artList.render();
    renderReader();
    setLevel(2);
  });

  async function refreshItem(path: string): Promise<void> {
    await post("rss/refreshItem", { itemPath: path }).catch((e) => toast(e.message, true));
    setTimeout(load, 1500);
  }

  async function markAllRead(): Promise<void> {
    const paths = current === UNREAD ? [""] : [current];
    for (const p of paths) await post("rss/markAsRead", { itemPath: p }).catch((e) => toast(e.message, true));
    load();
  }

  async function addFeed(): Promise<void> {
    const v = await formSheet("New feed", [
      { key: "url", label: "Feed URL", placeholder: "https://example.com/rss" },
      { key: "path", label: "Name", placeholder: "Optional" },
    ], "Subscribe");
    if (!v?.url) return;
    if (!safeUrl(v.url)) return toast("Enter an http:// or https:// URL", true);
    await post("rss/addFeed", { url: v.url, path: v.path || "" }).then(() => toast("Feed added"), (e) => toast(e.message, true));
    setTimeout(load, 1200);
  }

  function feedMenu(path: string, at: { x: number; y: number }): void {
    const f = feeds.find((x) => x.path === path);
    menu([
      { label: "Refresh", icon: "sync", run: () => refreshItem(path) },
      { label: "Mark as read", icon: "check", run: () => post("rss/markAsRead", { itemPath: path }).then(load) },
      {
        label: "Rename…", icon: "pencil", run: async () => {
          const name = path.split("\\").pop()!;
          const v = await formSheet("Rename", [{ key: "name", label: "Name", value: name }], "Rename");
          if (!v?.name || v.name === name) return;
          const parent = path.includes("\\") ? path.slice(0, path.lastIndexOf("\\") + 1) : "";
          post("rss/moveItem", { itemPath: path, destPath: parent + v.name }).then(() => {
            if (current === path) current = parent + v.name;
            load();
          }, (e) => toast(e.message, true));
        },
      },
      f ? {
        label: "Edit URL…", icon: "link", run: async () => {
          const v = await formSheet("Feed URL", [{ key: "url", label: "URL", value: f.url }], "Save");
          if (v?.url && v.url !== f.url) post("rss/setFeedURL", { path, url: v.url }).then(load, (e) => toast(e.message, true));
        },
      } : { label: "New feed in folder…", icon: "plus", run: addFeed },
      "-",
      {
        label: "Remove", icon: "trash", danger: true, run: async () => {
          if (await confirmSheet("Remove", `Remove “${path.split("\\").pop()}”?`, "Remove", true)) {
            post("rss/removeItem", { path }).then(load, (e) => toast(e.message, true));
          }
        },
      },
    ], at, f?.title ?? path);
  }

  on("section", () => {
    el.hidden = app.section !== "rss";
    if (!el.hidden) {
      artList.relayout();
      load();
    } else clearTimeout(timer);
  });
  setLevel(0);

  return { el };
}
