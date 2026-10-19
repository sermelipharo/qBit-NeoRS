// Entry point of the signed-in UI: layout, polling of sync/maindata
// (incremental by rid), status bar, drag and drop, keyboard shortcuts.

import { openAdd } from "./add";
import { get, post } from "./api";
import { app, emit, on, setPoke, type Section } from "./app";
import { createDetails } from "./details";
import { icon } from "./icons";
import { createRss } from "./rss";
import { createSearch } from "./search";
import { applyAppearance, openSettings } from "./settings";
import { createSidebar } from "./sidebar";
import { apply, currentRid, server } from "./store";
import { createTransfers } from "./transfers";
import { closeMenu, formSheet, sheet, type Sheet, toast } from "./ui";
import { bytes, h, load, save, setText, speed, toggle } from "./util";

applyAppearance();

const narrowQuery = matchMedia("(max-width: 760px)");
const sidebar = createSidebar(() => setDrawer(false));
const transfers = createTransfers();
const search = createSearch();
const rss = createRss();
const details = createDetails();

// ---- status bar -------------------------------------------------------------------

const conn = h("span", { class: "sb-conn" });
const dht = h("span", null);
const free = h("span", null);
const dl = h("button", { class: "sb-speed", title: "Download speed and limit", onclick: () => speedLimits() }, icon("down"), h("span", null));
const ul = h("button", { class: "sb-speed", title: "Upload speed and limit", onclick: () => speedLimits() }, icon("up"), h("span", null));
const alt = h("button", { class: "sb-alt", title: "Alternative speed limits", "aria-label": "Alternative speed limits", onclick: toggleAlt }, icon("gauge"));
const statusbar = h("footer", { class: "statusbar" }, conn, dht, free, h("span", { class: "spacer" }), dl, ul, alt);
const mini = h("span", { class: "mini-speed narrow-only" });

async function toggleAlt(): Promise<void> {
  await post("transfer/toggleSpeedLimitsMode").catch((e) => toast(e.message, true));
  poke();
}

async function speedLimits(): Promise<void> {
  const kib = (v: number) => (v > 0 ? Math.round(v / 1024) : 0);
  const v = await formSheet("Speed limits", [
    { key: "dl", label: "Download", type: "number", value: kib(server.dl_rate_limit), min: 0, hint: "KiB/s, 0 = unlimited" },
    { key: "up", label: "Upload", type: "number", value: kib(server.up_rate_limit), min: 0, hint: "KiB/s, 0 = unlimited" },
  ], "Apply", h("p", { class: "muted pad" }, server.use_alt_speed_limits ? "Alternative limits are in effect." : "Global limits."));
  if (!v) return;
  await post("transfer/setDownloadLimit", { limit: Math.max(0, v.dl) * 1024 }).catch((e) => toast(e.message, true));
  await post("transfer/setUploadLimit", { limit: Math.max(0, v.up) * 1024 }).catch((e) => toast(e.message, true));
  poke();
}

function renderStatus(): void {
  const s = server;
  const c = s.connection_status ?? "disconnected";
  conn.dataset.s = c;
  setText(conn, c === "connected" ? "Connected" : c === "firewalled" ? "Firewalled" : "Disconnected");
  setText(dht, s.dht_nodes != null ? `DHT ${s.dht_nodes} nodes` : "");
  setText(free, s.free_space_on_disk > 0 ? `${bytes(s.free_space_on_disk)} free` : "");
  const lim = (v: number) => (v > 0 ? ` (${speed(v)})` : "");
  setText(dl.lastElementChild!, (speed(s.dl_info_speed) === "–" ? "0 B/s" : speed(s.dl_info_speed)) + lim(s.dl_rate_limit));
  setText(ul.lastElementChild!, (speed(s.up_info_speed) === "–" ? "0 B/s" : speed(s.up_info_speed)) + lim(s.up_rate_limit));
  toggle(alt, "on", !!s.use_alt_speed_limits);
  alt.setAttribute("aria-pressed", String(!!s.use_alt_speed_limits));
  setText(mini, `↓ ${speed(s.dl_info_speed)}   ↑ ${speed(s.up_info_speed)}`);
  toggle(mini, "alt", !!s.use_alt_speed_limits);
}

// ---- tab bar (phones) -------------------------------------------------------------

const tabs: [Section | "settings", string, string][] = [["transfers", "Transfers", "tray"], ["search", "Search", "search"], ["rss", "RSS", "rss"], ["settings", "Settings", "sliders"]];
const tabbar = h("nav", { class: "tabbar", "aria-label": "Sections" }, tabs.map(([id, label, ic]) =>
  h("button", { "data-id": id, onclick: () => (id === "settings" ? openSettings() : go(id)) }, icon(ic), h("span", null, label)),
));

function go(s: Section): void {
  app.section = s;
  emit("section");
}

// ---- layout -------------------------------------------------------------------------

const inspector = h("aside", { class: "inspector", "aria-label": "Torrent details" }, details.el);
const scrim = h("div", { class: "scrim", onclick: () => setDrawer(false) });
const settingsBtn = h("button", { class: "sb-item sb-settings", onclick: () => openSettings() }, icon("sliders"), h("span", { class: "sb-label" }, "Settings"));
sidebar.el.append(h("div", { class: "sb-foot" }, settingsBtn));
const mainEl = h("main", { class: "main" }, transfers.el, search.el, rss.el, statusbar);
const root = h("div", { id: "app" }, sidebar.el, scrim, mainEl, inspector, tabbar);
document.body.append(root);
transfers.el.querySelector(".subline")?.append(mini);

let detailSheet: Sheet | null = null;

function setDrawer(open: boolean): void {
  toggle(root, "drawer", open && narrowQuery.matches);
}

on("sidebar", () => {
  if (narrowQuery.matches) setDrawer(!root.classList.contains("drawer"));
  else {
    const hidden = !root.classList.contains("no-sidebar");
    toggle(root, "no-sidebar", hidden);
    save("noSidebar", hidden);
  }
});

function layout(): void {
  const narrow = narrowQuery.matches;
  toggle(root, "narrow", narrow);
  toggle(root, "with-inspector", !narrow && app.inspector && app.section === "transfers");
  if (!narrow) setDrawer(false);
  for (const b of tabbar.children) toggle(b, "on", (b as HTMLElement).dataset.id === app.section);
  transfers.el.hidden = app.section !== "transfers";
  syncDetails();
}

function syncDetails(): void {
  const one = app.selection.size === 1 ? [...app.selection][0] : null;
  if (detailSheet) {
    if (!one) detailSheet.close();
    else details.show(one);
    return;
  }
  details.setPlaceholder(app.selection.size > 1 ? `${app.selection.size} torrents selected` : "Select a torrent to see its details.");
  details.show(root.classList.contains("with-inspector") ? one : null);
}

on("selection", syncDetails);
on("inspector", layout);
on("section", () => {
  layout();
  sidebar.render();
  if (app.section === "transfers") transfers.refresh();
  setDrawer(false);
});
on("open-details", () => {
  if (!narrowQuery.matches) {
    if (!app.inspector) {
      app.inspector = true;
      save("inspector", true);
      layout();
    }
    return;
  }
  const one = app.selection.size === 1 ? [...app.selection][0] : null;
  if (!one || detailSheet) return;
  detailSheet = sheet({
    title: "Details",
    cls: "full details-sheet",
    body: details.el,
    onclose: () => {
      detailSheet = null;
      inspector.append(details.el);
      app.selection.clear();
      transfers.selectionChanged();
    },
  });
  details.show(one);
  requestAnimationFrame(() => details.relayout());
});
on("add", () => openAdd());
narrowQuery.addEventListener("change", () => {
  emit("narrow");
  layout();
});
if (load("noSidebar", false)) root.classList.add("no-sidebar");

// ---- polling ------------------------------------------------------------------------

let timer = 0;
let inflight = false;
let failures = 0;

async function poll(): Promise<void> {
  clearTimeout(timer);
  if (inflight) return;
  inflight = true;
  try {
    const d = await get("sync/maindata", { rid: currentRid() });
    const ch = apply(d);
    failures = 0;
    root.classList.remove("offline");
    if (ch.structure || ch.labels || ch.keys.has("state") || ch.keys.has("category") || ch.keys.has("tags") || ch.keys.has("upspeed")) sidebar.render();
    if (app.section === "transfers") transfers.refresh(ch);
    if (ch.server) renderStatus();
    details.onHead();
    details.tick();
  } catch {
    failures++;
    if (failures > 1) root.classList.add("offline");
  } finally {
    inflight = false;
    if (!document.hidden) timer = window.setTimeout(poll, Math.min(30000, app.interval * (failures > 2 ? 4 : 1)));
  }
}

function poke(): void {
  clearTimeout(timer);
  timer = window.setTimeout(poll, 150);
}
setPoke(poke);

// No polling while the tab is hidden: no CPU or network work in the background.
document.addEventListener("visibilitychange", () => {
  if (document.hidden) clearTimeout(timer);
  else poll();
});

// ---- input ---------------------------------------------------------------------------

const typing = (t: EventTarget | null) => t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));

document.addEventListener("keydown", (e) => {
  if (document.querySelector("dialog[open]") || typing(e.target)) return;
  if (e.key === "/") {
    if (app.section === "transfers") {
      e.preventDefault();
      transfers.focusSearch();
    }
  } else if (e.key === "Escape") {
    closeMenu();
    setDrawer(false);
  }
});

let dragDepth = 0;
const hasFiles = (e: DragEvent) => !!e.dataTransfer && [...e.dataTransfer.types].includes("Files");
document.addEventListener("dragenter", (e) => {
  if (!hasFiles(e) || document.querySelector("dialog[open]")) return;
  dragDepth++;
  root.classList.add("dropping");
});
document.addEventListener("dragleave", () => {
  if (--dragDepth <= 0) {
    dragDepth = 0;
    root.classList.remove("dropping");
  }
});
document.addEventListener("dragover", (e) => {
  if (hasFiles(e)) e.preventDefault();
});
document.addEventListener("drop", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth = 0;
  root.classList.remove("dropping");
  if (document.querySelector("dialog[open]")) return;
  const files = [...(e.dataTransfer?.files ?? [])].filter((f) => /\.torrent$/i.test(f.name));
  if (files.length) openAdd({ files });
});

document.addEventListener("paste", (e) => {
  if (typing(e.target) || document.querySelector("dialog[open]")) return;
  const text = e.clipboardData?.getData("text") ?? "";
  if (/^(magnet:|https?:\/\/)/i.test(text.trim())) {
    e.preventDefault();
    openAdd({ urls: text.trim() });
  }
});

// ---- start ---------------------------------------------------------------------------

layout();
sidebar.render();
transfers.refresh();
renderStatus();
poll();
