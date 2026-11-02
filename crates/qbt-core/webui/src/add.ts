// Add Torrent sheet: .torrent files and magnet / HTTP links with the main
// options (save path, category, tags, start, layout, checks), the free disk
// space, and the file list of a single torrent (excluded file names start
// unchecked, as qBittorrent 5.3).

import { effectiveSavePath, initialWanted, type MetaFile, prioritiesParam } from "./addfiles";
import { get, post } from "./api";
import { clientData, prefs, refreshSoon } from "./app";
import { icon } from "./icons";
import { categories, tags } from "./store";
import { control, group, row, sheet, toast } from "./ui";
import { bytes, collator, debounce, h, put, setText } from "./util";

const isTorrentFile = (f: File) => /\.torrent$/i.test(f.name) || f.type === "application/x-bittorrent";

/** Adds with the default options (the "Add new torrent" dialog is off). */
async function quickAdd(files: File[], urls: string): Promise<void> {
  const fd = new FormData();
  for (const f of files) fd.append("torrents", f, f.name);
  if (urls.trim()) fd.append("urls", urls.trim());
  try {
    await post("torrents/add", fd);
    const n = files.length + urls.split("\n").filter((u) => u.trim()).length;
    toast(`Added ${n} torrent${n === 1 ? "" : "s"}`);
  } catch (e) {
    toast((e as Error).message, true);
  }
  refreshSoon();
}

interface Meta {
  /** What it was loaded for (file identity or the link). */
  key: string;
  hash: string;
  files: MetaFile[];
  initial: boolean[];
  wanted: boolean[];
}

export async function openAdd(o: { files?: File[]; urls?: string } = {}): Promise<void> {
  let files: File[] = (o.files ?? []).filter(isTorrentFile);
  // qBittorrent 5.3's "skip the Add new torrent dialog": dropped files and
  // pasted or RSS links are added at once; the + button still opens the sheet.
  if ((files.length || o.urls?.trim()) && (await clientData()).add_new_torrent_dialog_enabled === false) {
    return quickAdd(files, o.urls ?? "");
  }
  const p = await prefs().catch(() => ({}) as Record<string, any>);
  let mode: "files" | "links" = o.urls ? "links" : "files";
  let closed = false;

  const urls = h("textarea", { rows: 4, placeholder: "magnet:?xt=…  or  https://…/file.torrent\nOne link per line", spellcheck: false, value: o.urls ?? "", "aria-label": "Links" });
  const picker = h("input", { type: "file", multiple: true, accept: ".torrent,application/x-bittorrent", hidden: true });
  const fileList = h("ul", { class: "file-list" });
  const drop = h("div", { class: "drop", tabIndex: 0, role: "button", "aria-label": "Choose .torrent files" },
    icon("doc"), h("p", null, "Drop .torrent files here"), h("small", null, "or click to choose"),
  );
  const showFiles = () => {
    put(fileList, files.map((f, i) => h("li", null, icon("doc"), h("span", null, f.name), h("small", null, bytes(f.size)),
      h("button", { type: "button", class: "icon-btn small", "aria-label": "Remove " + f.name, onclick: () => { files.splice(i, 1); showFiles(); } }, icon("x")))));
    void loadMeta();
  };
  picker.addEventListener("change", () => {
    files = files.concat([...(picker.files ?? [])].filter(isTorrentFile));
    picker.value = "";
    showFiles();
  });
  drop.addEventListener("click", () => picker.click());
  drop.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && picker.click());
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    e.stopPropagation();
    drop.classList.remove("over");
    files = files.concat([...(e.dataTransfer?.files ?? [])].filter((f) => /\.torrent$/i.test(f.name)));
    showFiles();
  });

  const filesPane = h("div", { class: "add-pane" }, drop, picker, fileList);
  const linksPane = h("div", { class: "add-pane" }, urls);
  const segFiles = h("button", { type: "button", onclick: () => setMode("files") }, "Torrent files");
  const segLinks = h("button", { type: "button", onclick: () => setMode("links") }, "Links");
  const setMode = (m: typeof mode) => {
    mode = m;
    filesPane.hidden = m !== "files";
    linksPane.hidden = m !== "links";
    segFiles.classList.toggle("on", m === "files");
    segLinks.classList.toggle("on", m === "links");
    if (m === "links") urls.focus();
    void loadMeta();
  };

  const auto = control({ key: "auto", label: "Automatic management", type: "bool", value: !!p.auto_tmm_enabled });
  const path = control({ key: "path", label: "Save path", value: p.save_path ?? "" }) as HTMLInputElement;
  const cat = control({ key: "cat", label: "Category", type: "select", value: "", options: [["", "None"], ...[...categories.keys()].sort(collator.compare).map((c) => [c, c] as [string, string])] }) as HTMLSelectElement;
  const tagIn = control({ key: "tags", label: "Tags", placeholder: [...tags].slice(0, 3).join(", ") || "comma, separated" }) as HTMLInputElement;
  const startIt = control({ key: "start", label: "Start torrent", type: "bool", value: !p.add_stopped_enabled }) as HTMLInputElement;
  const top = control({ key: "top", label: "Add to top of queue", type: "bool", value: !!p.add_to_top_of_queue }) as HTMLInputElement;
  const skip = control({ key: "skip", label: "Skip hash check", type: "bool", value: false }) as HTMLInputElement;
  const seq = control({ key: "seq", label: "Sequential download", type: "bool", value: false }) as HTMLInputElement;
  const fl = control({ key: "fl", label: "First and last pieces first", type: "bool", value: false }) as HTMLInputElement;
  const layout = control({ key: "layout", label: "Content layout", type: "select", value: p.torrent_content_layout ?? "Original", options: [["Original", "Original"], ["Subfolder", "Create subfolder"], ["NoSubfolder", "Don’t create subfolder"]] }) as HTMLSelectElement;
  const syncAuto = () => {
    path.disabled = (auto as HTMLInputElement).checked;
  };
  auto.addEventListener("change", syncAuto);
  cat.addEventListener("change", () => {
    const c = categories.get(cat.value);
    if (c?.savePath && !(auto as HTMLInputElement).checked) path.value = c.savePath;
  });
  syncAuto();

  // ---- free disk space (5.3) -------------------------------------------------------

  const diskInfo = h("span", { class: "row-ctl muted", "aria-live": "polite" }, "–");
  let free: number | null = null;
  let freeToken = 0;
  const showDisk = () => {
    const parts: string[] = [];
    if (meta) parts.push("Selected " + bytes(meta.files.reduce((n, f, i) => n + (meta!.wanted[i] ? f.length : 0), 0)));
    parts.push("Free " + (free == null ? "–" : bytes(free)));
    setText(diskInfo, parts.join(" · "));
  };
  const loadFree = debounce(async () => {
    const at = effectiveSavePath({ auto: (auto as HTMLInputElement).checked, path: path.value, categoryPath: categories.get(cat.value)?.savePath, defaultPath: p.save_path ?? "" });
    const token = ++freeToken;
    let v: number | null = null;
    if (at) {
      try {
        const r = Number(await get("app/getFreeSpaceAtPath", { path: at }));
        v = Number.isFinite(r) && r >= 0 ? r : null;
      } catch {
        v = null;
      }
    }
    if (token !== freeToken || closed) return;
    free = v;
    showDisk();
  }, 300);
  for (const c of [auto, cat]) c.addEventListener("change", loadFree);
  path.addEventListener("input", loadFree);

  // ---- file list of a single torrent (5.3: excluded names start unchecked) ----------

  let meta: Meta | null = null;
  let metaToken = 0;
  const filesBox = h("div", { class: "add-files" });
  const filesGroup = group("Files", filesBox);
  filesGroup.hidden = true;

  const renderMeta = () => {
    filesGroup.hidden = !meta && !linkPending();
    if (!meta) {
      put(filesBox, linkPending()
        ? h("div", { class: "fr" }, h("span", { class: "row-label muted" }, fetching() ? "Fetching the file list…" : "The file list needs the torrent’s metadata."),
          fetching() ? null : h("button", { type: "button", class: "btn small", onclick: fetchLinkMeta }, "Show files"))
        : null);
      showDisk();
      return;
    }
    const m = meta;
    const count = h("span", { class: "row-label" });
    const sync = () => {
      const n = m.wanted.filter(Boolean).length;
      setText(count, `${n} of ${m.files.length} file${m.files.length === 1 ? "" : "s"}`);
      showDisk();
    };
    const boxes = m.files.map((f, i) => {
      const cb = h("input", { type: "checkbox", checked: m.wanted[i], "aria-label": f.path });
      cb.addEventListener("change", () => {
        m.wanted[i] = cb.checked;
        sync();
      });
      return h("li", null, h("label", null, cb, h("span", { title: f.path }, f.path), h("small", null, bytes(f.length))));
    });
    const setAll = (v: boolean) => {
      m.wanted.fill(v);
      for (const li of boxes) (li.querySelector("input") as HTMLInputElement).checked = v;
      sync();
    };
    put(filesBox,
      h("div", { class: "fr" }, count, h("span", { class: "row-ctl" },
        h("button", { type: "button", class: "btn small", onclick: () => setAll(true) }, "All"),
        h("button", { type: "button", class: "btn small", onclick: () => setAll(false) }, "None"))),
      h("ul", { class: "file-list add-files-list" }, boxes),
      m.initial.some((w) => !w) ? h("p", { class: "note pad-x" }, "Files matching the excluded file names (or left out by the magnet link) start unchecked.") : null,
    );
    sync();
  };

  const setMeta = (key: string, json: any) => {
    const list: MetaFile[] = Array.isArray(json?.info?.files) ? json.info.files.map((f: any) => ({ path: String(f.path ?? ""), length: Number(f.length) || 0, priority: f.priority == null ? undefined : Number(f.priority) })) : [];
    if (!list.length || typeof json.hash !== "string") {
      meta = null;
      return;
    }
    const initial = initialWanted(list);
    meta = { key, hash: json.hash, files: list, initial, wanted: initial.slice() };
  };

  const fileKey = (f: File) => `${f.name}\n${f.size}\n${f.lastModified}`;
  const singleLink = () => {
    const l = urls.value.split("\n").map((x) => x.trim()).filter(Boolean);
    return l.length === 1 ? l[0] : null;
  };
  const linkPending = () => mode === "links" && !!singleLink() && meta?.key !== singleLink();
  /** The link whose metadata is being fetched. */
  let fetchingFor: string | null = null;
  const fetching = () => fetchingFor !== null && fetchingFor === singleLink();

  async function loadMeta(): Promise<void> {
    if (mode === "files") {
      if (fetchingFor !== null) {
        fetchingFor = null;
        ++metaToken;
      }
      if (files.length !== 1) {
        meta = null;
        renderMeta();
        return;
      }
      const f = files[0], key = fileKey(f);
      if (meta?.key === key) return renderMeta();
      const token = ++metaToken;
      const fd = new FormData();
      fd.append("torrents", f, f.name);
      try {
        const r = await post("torrents/parseMetadata", fd);
        if (token !== metaToken || closed) return;
        setMeta(key, Array.isArray(r) ? r[0] : null);
      } catch {
        if (token !== metaToken) return;
        meta = null;
      }
      renderMeta();
    } else {
      const l = singleLink();
      if (meta && meta.key !== l) meta = null;
      if (fetchingFor !== null && fetchingFor !== l) {
        fetchingFor = null;
        ++metaToken;
      }
      renderMeta();
    }
  }

  /** Asks the server for a link's metadata (magnets need peers) and polls. */
  async function fetchLinkMeta(): Promise<void> {
    const l = singleLink();
    if (!l) return;
    const token = ++metaToken;
    fetchingFor = l;
    renderMeta();
    for (let i = 0; i < 90 && !closed && token === metaToken; i++) {
      try {
        const r = await post("torrents/fetchMetadata", { source: l });
        if (token !== metaToken || closed) return;
        if (r && typeof r === "object" && r.info) {
          setMeta(l, r);
          break;
        }
      } catch (e) {
        if (token === metaToken) toast((e as Error).message, true);
        break;
      }
      await new Promise((res) => setTimeout(res, 2000));
    }
    if (token !== metaToken) return;
    fetchingFor = null;
    renderMeta();
  }
  urls.addEventListener("input", debounce(() => void loadMeta(), 250));

  sheet({
    title: "Add Torrents",
    cls: "add",
    onclose: () => {
      closed = true;
      metaToken++;
    },
    body: [
      h("div", { class: "seg wide" }, segFiles, segLinks),
      filesPane,
      linksPane,
      group("Saving",
        row("Automatic management", auto, "Save path follows the category"),
        row("Save path", path),
        row("Category", cat),
        row("Tags", tagIn),
        row("Disk space", diskInfo),
      ),
      filesGroup,
      group("Options",
        row("Start torrent", startIt),
        row("Add to top of queue", top),
        row("Content layout", layout),
        row("Skip hash check", skip),
        row("Sequential download", seq),
        row("First and last pieces first", fl),
      ),
    ],
    actions: [
      { label: "Cancel" },
      {
        label: "Add",
        primary: true,
        run: async () => {
          const fd = new FormData();
          // A changed file selection: the server adds its cached metadata
          // (from parseMetadata / fetchMetadata) with these priorities.
          const m = meta;
          const prios = m ? prioritiesParam(m.initial, m.wanted) : null;
          if (m && prios != null && !m.wanted.some(Boolean)) throw new Error("Select at least one file.");
          if (mode === "files") {
            if (!files.length) throw new Error("Choose at least one .torrent file.");
            if (m && prios != null && files.length === 1 && m.key === fileKey(files[0])) fd.append("urls", m.hash);
            else for (const f of files) fd.append("torrents", f, f.name);
          } else {
            const u = urls.value.trim();
            if (!u) throw new Error("Paste at least one magnet or HTTP link.");
            fd.append("urls", u);
          }
          if (m && prios != null && fd.has("urls") && (mode === "files" || m.key === singleLink())) fd.append("filePriorities", prios);
          const auto_ = (auto as HTMLInputElement).checked;
          fd.append("autoTMM", String(auto_));
          if (!auto_ && path.value.trim()) fd.append("savepath", path.value.trim());
          if (cat.value) fd.append("category", cat.value);
          if (tagIn.value.trim()) fd.append("tags", tagIn.value.split(",").map((x) => x.trim()).filter(Boolean).join(","));
          fd.append("stopped", String(!startIt.checked));
          fd.append("addToTopOfQueue", String(top.checked));
          fd.append("seedMode", String(skip.checked));
          fd.append("sequentialDownload", String(seq.checked));
          fd.append("firstLastPiecePrio", String(fl.checked));
          fd.append("contentLayout", layout.value);
          await post("torrents/add", fd);
          closed = true;
          toast(mode === "files" ? `Added ${files.length} torrent${files.length === 1 ? "" : "s"}` : "Links added");
          refreshSoon();
        },
      },
    ],
  });
  setMode(mode);
  showFiles();
  loadFree();
}
