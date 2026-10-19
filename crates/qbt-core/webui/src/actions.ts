// Torrent commands shared by the toolbar, context menus and details.

import { post } from "./api";
import { app, prefs, refreshSoon } from "./app";
import { categories, isStopped, tagList, tags, torrents, type Torrent } from "./store";
import { confirmSheet, formSheet, type MenuEntry, toast } from "./ui";
import { collator, h } from "./util";

export const selected = (): Torrent[] => [...app.selection].map((x) => torrents.get(x)).filter((t): t is Torrent => !!t);
const hashes = (list = selected()) => list.map((t) => t.hash).join("|");

async function run(path: string, extra: Record<string, string | number | boolean> = {}, list = selected()): Promise<void> {
  if (!list.length) return;
  try {
    await post("torrents/" + path, { hashes: hashes(list), ...extra });
  } catch (e) {
    toast((e as Error).message, true);
  }
  refreshSoon();
}

export const start = (list?: Torrent[]) => run("start", {}, list);
export const stop = (list?: Torrent[]) => run("stop", {}, list);
export const reannounce = () => run("reannounce");

export async function remove(list = selected()): Promise<void> {
  if (!list.length) return;
  const p = await prefs().catch(() => ({}) as Record<string, any>);
  const files = h("input", { type: "checkbox", class: "switch", checked: !!p.delete_torrent_content_files });
  const what = list.length === 1 ? `“${list[0].name}”` : `${list.length} torrents`;
  const ok = await confirmSheet(
    "Remove torrents",
    `Remove ${what} from the transfer list?`,
    "Remove",
    true,
    h("label", { class: "fr plain" }, h("span", { class: "row-label" }, "Also delete the files on disk"), files),
  );
  if (ok) {
    await run("delete", { deleteFiles: files.checked }, list);
    for (const t of list) app.selection.delete(t.hash);
  }
}

export async function recheck(): Promise<void> {
  const list = selected();
  if (list.length && (await confirmSheet("Recheck torrents", `Recheck ${list.length === 1 ? "this torrent" : list.length + " torrents"}? Their data will be verified again.`, "Recheck"))) {
    run("recheck", {}, list);
  }
}

export async function setLocation(): Promise<void> {
  const list = selected();
  if (!list.length) return;
  const v = await formSheet("Set location", [{ key: "location", label: "Save path", value: list[0].save_path }], "Move");
  if (v?.location) run("setLocation", { location: v.location }, list);
}

export async function rename(t = selected()[0]): Promise<void> {
  if (!t) return;
  const v = await formSheet("Rename", [{ key: "name", label: "Name", value: t.name }], "Rename");
  if (v?.name && v.name !== t.name) {
    await post("torrents/rename", { hash: t.hash, name: v.name }).catch((e) => toast(e.message, true));
    refreshSoon();
  }
}

export async function newCategory(assign = false): Promise<void> {
  const v = await formSheet("New category", [
    { key: "name", label: "Name", placeholder: "Movies" },
    { key: "path", label: "Save path", placeholder: "Default" },
  ], "Create");
  if (!v?.name) return;
  try {
    await post("torrents/createCategory", { category: v.name, savePath: v.path });
    if (assign) await run("setCategory", { category: v.name });
  } catch (e) {
    toast((e as Error).message, true);
  }
  refreshSoon();
}

export async function newTag(assign = false): Promise<void> {
  const v = await formSheet("New tag", [{ key: "name", label: "Tag", placeholder: "Tag name" }], "Create");
  if (!v?.name) return;
  await post("torrents/createTags", { tags: v.name }).catch((e) => toast(e.message, true));
  if (assign) await run("addTags", { tags: v.name });
  refreshSoon();
}

export async function limits(): Promise<void> {
  const list = selected();
  if (!list.length) return;
  const t = list[0];
  const kib = (b: number) => (b > 0 ? Math.round(b / 1024) : 0);
  const v = await formSheet("Limits", [
    { key: "dl", label: "Download limit", type: "number", value: kib(t.dl_limit), min: 0, hint: "KB/s, 0 = unlimited" },
    { key: "up", label: "Upload limit", type: "number", value: kib(t.up_limit), min: 0, hint: "KB/s, 0 = unlimited" },
    { key: "ratio", label: "Ratio limit", type: "number", value: t.ratio_limit ?? -2, step: 0.01, hint: "−2 global, −1 none" },
    { key: "time", label: "Seeding time limit", type: "number", value: t.seeding_time_limit ?? -2, hint: "minutes; −2 global, −1 none" },
  ], "Apply");
  if (!v) return;
  await run("setDownloadLimit", { limit: Math.max(0, v.dl) * 1024 }, list);
  await run("setUploadLimit", { limit: Math.max(0, v.up) * 1024 }, list);
  await run("setShareLimits", {
    ratioLimit: v.ratio,
    seedingTimeLimit: v.time,
    inactiveSeedingTimeLimit: t.inactive_seeding_time_limit ?? -2,
    shareLimitAction: "Default",
  }, list);
}

async function copy(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Plain-HTTP origins have no async clipboard.
    const ta = h("textarea", { value: text });
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  toast("Copied");
}

function categoryMenu(list: Torrent[]): MenuEntry[] {
  const cur = list.length === 1 ? list[0].category : null;
  const names = [...categories.keys()].sort(collator.compare);
  return [
    { label: "New…", icon: "plus", run: () => newCategory(true) },
    "-",
    { label: "None", checked: cur === "", run: () => run("setCategory", { category: "" }, list) },
    ...names.map((n) => ({ label: n, checked: cur === n, run: () => run("setCategory", { category: n }, list) })),
  ];
}

function tagMenu(list: Torrent[]): MenuEntry[] {
  const names = [...tags].sort(collator.compare);
  return [
    { label: "New…", icon: "plus", run: () => newTag(true) },
    { label: "Remove all", disabled: !list.some((t) => t.tags), run: () => run("removeTags", { tags: "" }, list) },
    "-",
    ...names.map((n) => {
      const all = list.every((t) => tagList(t).includes(n));
      return { label: n, checked: all, run: () => run(all ? "removeTags" : "addTags", { tags: n }, list) };
    }),
  ];
}

export function torrentMenu(list = selected()): MenuEntry[] {
  if (!list.length) return [];
  const one = list.length === 1 ? list[0] : null;
  const anyStopped = list.some((t) => isStopped(t.state));
  const anyRunning = list.some((t) => !isStopped(t.state));
  const all = (k: keyof Torrent) => list.every((t) => !!t[k]);
  return [
    { label: "Start", icon: "play", disabled: !anyStopped, run: () => start(list) },
    { label: "Stop", icon: "stop", disabled: !anyRunning, run: () => stop(list) },
    { label: "Force start", icon: "bolt", checked: all("force_start"), run: () => run("setForceStart", { value: !all("force_start") }, list) },
    "-",
    { label: "Remove…", icon: "trash", danger: true, run: () => remove(list) },
    "-",
    { label: "Set location…", icon: "folder", run: setLocation },
    { label: "Rename…", icon: "pencil", disabled: !one, run: () => rename(one!) },
    { label: "Category", icon: "folder", sub: () => categoryMenu(list) },
    { label: "Tags", icon: "tag", sub: () => tagMenu(list) },
    { label: "Limits…", icon: "gauge", run: limits },
    "-",
    {
      label: "Queue",
      icon: "queue",
      sub: () => [
        { label: "Move to top", run: () => run("topPrio", {}, list) },
        { label: "Move up", run: () => run("increasePrio", {}, list) },
        { label: "Move down", run: () => run("decreasePrio", {}, list) },
        { label: "Move to bottom", run: () => run("bottomPrio", {}, list) },
      ],
    },
    {
      label: "Options",
      icon: "sliders",
      sub: () => [
        { label: "Automatic management", checked: all("auto_tmm"), run: () => run("setAutoManagement", { enable: !all("auto_tmm") }, list) },
        { label: "Sequential download", checked: all("seq_dl"), run: () => run("toggleSequentialDownload", {}, list) },
        { label: "First and last pieces first", checked: all("f_l_piece_prio"), run: () => run("toggleFirstLastPiecePrio", {}, list) },
        { label: "Super seeding", checked: all("super_seeding"), run: () => run("setSuperSeeding", { value: !all("super_seeding") }, list) },
      ],
    },
    { label: "Force recheck", icon: "sync", run: recheck },
    { label: "Force reannounce", icon: "antenna", run: reannounce },
    "-",
    {
      label: "Copy",
      icon: "doc",
      sub: () => [
        { label: "Name", run: () => copy(list.map((t) => t.name).join("\n")) },
        { label: "Info hash", run: () => copy(list.map((t) => t.hash).join("\n")) },
        { label: "Magnet link", run: () => copy(list.map((t) => t.magnet_uri).join("\n")) },
      ],
    },
  ];
}
