// Add Torrent sheet: .torrent files and magnet / HTTP links with the main
// options (save path, category, tags, start, layout, checks).

import { post } from "./api";
import { prefs, refreshSoon } from "./app";
import { icon } from "./icons";
import { categories, tags } from "./store";
import { control, group, row, sheet, toast } from "./ui";
import { bytes, collator, h, put } from "./util";

export async function openAdd(o: { files?: File[]; urls?: string } = {}): Promise<void> {
  const p = await prefs().catch(() => ({}) as Record<string, any>);
  let files: File[] = (o.files ?? []).filter((f) => /\.torrent$/i.test(f.name) || f.type === "application/x-bittorrent");
  let mode: "files" | "links" = o.urls ? "links" : "files";

  const urls = h("textarea", { rows: 4, placeholder: "magnet:?xt=…  or  https://…/file.torrent\nOne link per line", spellcheck: false, value: o.urls ?? "", "aria-label": "Links" });
  const picker = h("input", { type: "file", multiple: true, accept: ".torrent,application/x-bittorrent", hidden: true });
  const fileList = h("ul", { class: "file-list" });
  const drop = h("div", { class: "drop", tabIndex: 0, role: "button", "aria-label": "Choose .torrent files" },
    icon("doc"), h("p", null, "Drop .torrent files here"), h("small", null, "or click to choose"),
  );
  const showFiles = () => {
    put(fileList, files.map((f, i) => h("li", null, icon("doc"), h("span", null, f.name), h("small", null, bytes(f.size)),
      h("button", { type: "button", class: "icon-btn small", "aria-label": "Remove " + f.name, onclick: () => { files.splice(i, 1); showFiles(); } }, icon("x")))));
  };
  picker.addEventListener("change", () => {
    files = files.concat([...(picker.files ?? [])]);
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

  sheet({
    title: "Add Torrents",
    cls: "add",
    body: [
      h("div", { class: "seg wide" }, segFiles, segLinks),
      filesPane,
      linksPane,
      group("Saving",
        row("Automatic management", auto, "Save path follows the category"),
        row("Save path", path),
        row("Category", cat),
        row("Tags", tagIn),
      ),
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
          if (mode === "files") {
            if (!files.length) throw new Error("Choose at least one .torrent file.");
            for (const f of files) fd.append("torrents", f, f.name);
          } else {
            const u = urls.value.trim();
            if (!u) throw new Error("Paste at least one magnet or HTTP link.");
            fd.append("urls", u);
          }
          const auto_ = (auto as HTMLInputElement).checked;
          fd.append("autoTMM", String(auto_));
          if (!auto_ && path.value.trim()) fd.append("savepath", path.value.trim());
          if (cat.value) fd.append("category", cat.value);
          if (tagIn.value.trim()) fd.append("tags", tagIn.value.split(",").map((x) => x.trim()).filter(Boolean).join(","));
          fd.append("stopped", String(!startIt.checked));
          fd.append("addToTopOfQueue", String(top.checked));
          fd.append("skip_checking", String(skip.checked));
          fd.append("sequentialDownload", String(seq.checked));
          fd.append("firstLastPiecePrio", String(fl.checked));
          fd.append("contentLayout", layout.value);
          await post("torrents/add", fd);
          toast(mode === "files" ? `Added ${files.length} torrent${files.length === 1 ? "" : "s"}` : "Links added");
          refreshSoon();
        },
      },
    ],
  });
  setMode(mode);
  showFiles();
}
