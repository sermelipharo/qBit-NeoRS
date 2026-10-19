// Settings sheet: appearance (this browser) and the core preferences in
// panes; "All Settings" lists every other WebAPI key, typed from its value.
// Only changed keys are sent to app/setPreferences.

import { get, post } from "./api";
import { app, emit, prefs } from "./app";
import { icon } from "./icons";
import { control, group, row, sheet, toast, valueOf } from "./ui";
import { h, load, put, save } from "./util";

type Kind = "bool" | "number" | "text" | "textarea" | "password" | "kib" | [string, string][];
type Spec = [key: string, label: string, kind?: Kind, hint?: string];
type Pane = [id: string, title: string, icon: string, groups: [string, Spec[]][]];

const ENCRYPTION: [string, string][] = [["0", "Allow encryption"], ["1", "Require encryption"], ["2", "Disable encryption"]];
const LAYOUT: [string, string][] = [["Original", "Original"], ["Subfolder", "Create subfolder"], ["NoSubfolder", "Don’t create subfolder"]];
const STOP: [string, string][] = [["None", "None"], ["MetadataReceived", "Metadata received"], ["FilesChecked", "Files checked"]];
const PROXY: [string, string][] = [["None", "None"], ["HTTP", "HTTP"], ["SOCKS5", "SOCKS5"], ["SOCKS4", "SOCKS4"]];
const SHARE_ACT: [string, string][] = [["0", "Stop torrent"], ["1", "Remove torrent"], ["3", "Remove torrent and files"], ["2", "Enable super seeding"]];
const DAYS: [string, string][] = [["0", "Every day"], ["1", "Weekdays"], ["2", "Weekends"], ["3", "Monday"], ["4", "Tuesday"], ["5", "Wednesday"], ["6", "Thursday"], ["7", "Friday"], ["8", "Saturday"], ["9", "Sunday"]];

const PANES: Pane[] = [
  ["downloads", "Downloads", "dl", [
    ["When adding a torrent", [
      ["torrent_content_layout", "Content layout", LAYOUT],
      ["add_to_top_of_queue", "Add to top of queue"],
      ["add_stopped_enabled", "Don’t start downloads automatically"],
      ["torrent_stop_condition", "Stop condition", STOP],
      ["merge_trackers", "Merge trackers of duplicate torrents"],
    ]],
    ["Saving", [
      ["auto_tmm_enabled", "Automatic torrent management by default"],
      ["save_path", "Default save path", "text"],
      ["temp_path_enabled", "Keep incomplete torrents in"],
      ["temp_path", "Incomplete path", "text"],
      ["export_dir", "Copy .torrent files to", "text"],
      ["export_dir_fin", "Copy .torrent files of finished downloads to", "text"],
      ["preallocate_all", "Pre-allocate disk space"],
      ["incomplete_files_ext", "Append .!qB to incomplete files"],
    ]],
    ["Excluded file names", [
      ["excluded_file_names_enabled", "Exclude files by name"],
      ["excluded_file_names", "Patterns", "textarea"],
    ]],
  ]],
  ["connection", "Connection", "antenna", [
    ["Listening", [
      ["listen_port", "Incoming port", "number"],
      ["upnp", "UPnP / NAT-PMP port forwarding"],
    ]],
    ["Limits", [
      ["max_connec", "Global connections", "number", "−1 = unlimited"],
      ["max_connec_per_torrent", "Connections per torrent", "number"],
      ["max_uploads", "Global upload slots", "number"],
      ["max_uploads_per_torrent", "Upload slots per torrent", "number"],
    ]],
    ["Proxy", [
      ["proxy_type", "Type", PROXY],
      ["proxy_ip", "Host", "text"],
      ["proxy_port", "Port", "number"],
      ["proxy_auth_enabled", "Authentication"],
      ["proxy_username", "Username", "text"],
      ["proxy_password", "Password", "password"],
      ["proxy_peer_connections", "Use for peer connections"],
      ["proxy_bittorrent", "Use for BitTorrent"],
      ["proxy_rss", "Use for RSS"],
      ["proxy_misc", "Use for other purposes"],
    ]],
    ["IP filtering", [
      ["ip_filter_enabled", "IP filter"],
      ["ip_filter_path", "Filter file", "text"],
      ["ip_filter_trackers", "Apply to trackers"],
      ["banned_IPs", "Banned IP addresses", "textarea"],
    ]],
  ]],
  ["speed", "Speed", "gauge", [
    ["Global limits", [
      ["dl_limit", "Download", "kib", "KiB/s, 0 = unlimited"],
      ["up_limit", "Upload", "kib", "KiB/s, 0 = unlimited"],
    ]],
    ["Alternative limits", [
      ["alt_dl_limit", "Download", "kib", "KiB/s, 0 = unlimited"],
      ["alt_up_limit", "Upload", "kib", "KiB/s, 0 = unlimited"],
      ["scheduler_enabled", "Use on a schedule"],
      ["schedule_from_hour", "From (hour)", "number"],
      ["schedule_from_min", "From (minute)", "number"],
      ["schedule_to_hour", "To (hour)", "number"],
      ["schedule_to_min", "To (minute)", "number"],
      ["scheduler_days", "Days", DAYS],
    ]],
    ["Rate limit settings", [
      ["limit_utp_rate", "Apply to µTP"],
      ["limit_tcp_overhead", "Apply to transport overhead"],
      ["limit_lan_peers", "Apply to LAN peers"],
    ]],
  ]],
  ["bittorrent", "BitTorrent", "magnet", [
    ["Privacy", [
      ["dht", "DHT (decentralized network)"],
      ["pex", "Peer exchange (PeX)"],
      ["lsd", "Local peer discovery"],
      ["encryption", "Encryption", ENCRYPTION],
      ["anonymous_mode", "Anonymous mode"],
    ]],
    ["Queueing", [
      ["queueing_enabled", "Torrent queueing"],
      ["max_active_downloads", "Active downloads", "number"],
      ["max_active_uploads", "Active uploads", "number"],
      ["max_active_torrents", "Active torrents", "number"],
      ["dont_count_slow_torrents", "Don’t count slow torrents"],
    ]],
    ["Seeding limits", [
      ["max_ratio_enabled", "Stop at ratio"],
      ["max_ratio", "Ratio", "number"],
      ["max_seeding_time_enabled", "Stop after seeding time"],
      ["max_seeding_time", "Minutes", "number"],
      ["max_ratio_act", "Then", SHARE_ACT],
    ]],
    ["Trackers", [
      ["add_trackers_enabled", "Add trackers to new downloads"],
      ["add_trackers", "Trackers", "textarea"],
    ]],
  ]],
  ["rss", "RSS", "rss", [
    ["Feeds", [
      ["rss_processing_enabled", "Fetch RSS feeds"],
      ["rss_refresh_interval", "Refresh interval", "number", "minutes"],
      ["rss_max_articles_per_feed", "Articles per feed", "number"],
      ["rss_auto_downloading_enabled", "Automatic downloading"],
      ["rss_download_repack_proper_episodes", "Download REPACK/PROPER episodes"],
    ]],
  ]],
  ["webui", "Web UI", "globe", [
    ["Server", [
      ["web_ui_address", "IP address", "text", "* = all addresses"],
      ["web_ui_port", "Port", "number"],
      ["web_ui_upnp", "Forward the port with UPnP"],
      ["use_https", "Use HTTPS"],
      ["web_ui_https_cert_path", "Certificate", "text"],
      ["web_ui_https_key_path", "Key", "text"],
    ]],
    ["Authentication", [
      ["web_ui_username", "Username", "text"],
      ["web_ui_password", "New password", "password", "leave empty to keep"],
      ["bypass_local_auth", "Bypass for clients on localhost"],
      ["bypass_auth_subnet_whitelist_enabled", "Bypass for whitelisted subnets"],
      ["bypass_auth_subnet_whitelist", "Subnets", "textarea"],
      ["web_ui_max_auth_fail_count", "Ban after failed attempts", "number"],
      ["web_ui_ban_duration", "Ban for", "number", "seconds"],
      ["web_ui_session_timeout", "Session timeout", "number", "seconds"],
    ]],
    ["Security", [
      ["web_ui_clickjacking_protection_enabled", "Clickjacking protection"],
      ["web_ui_csrf_protection_enabled", "CSRF protection"],
      ["web_ui_secure_cookie_enabled", "Secure cookie (HTTPS)"],
      ["web_ui_host_header_validation_enabled", "Host header validation"],
      ["web_ui_domain_list", "Server domains", "text"],
    ]],
    ["Interface", [
      ["alternative_webui_enabled", "Use an alternative Web UI"],
      ["alternative_webui_path", "Files location", "text"],
    ]],
  ]],
];

/** Keys never shown in "All Settings" (objects, secrets, read-only). */
const HIDDEN = new Set(["scan_dirs", "web_ui_api_key", "web_ui_password", "locale"]);

const ACCENTS = ["blue", "purple", "pink", "red", "orange", "yellow", "green", "graphite"];

export function applyAppearance(): void {
  const d = document.documentElement;
  const theme = load("theme", "auto");
  if (theme === "auto") delete d.dataset.theme;
  else d.dataset.theme = theme;
  d.dataset.accent = load("accent", "blue");
}

function appearancePane(): HTMLElement {
  const theme = control({ key: "theme", label: "Appearance", type: "select", value: load("theme", "auto"), options: [["auto", "Automatic"], ["light", "Light"], ["dark", "Dark"]] });
  theme.addEventListener("change", () => {
    save("theme", theme.value);
    applyAppearance();
  });
  const swatches = h("div", { class: "swatches", role: "radiogroup", "aria-label": "Accent color" }, ACCENTS.map((a) =>
    h("button", {
      type: "button", class: "swatch", "data-c": a, role: "radio", "aria-label": a, title: a[0].toUpperCase() + a.slice(1),
      "aria-checked": String(load("accent", "blue") === a),
      onclick: (e: MouseEvent) => {
        save("accent", a);
        applyAppearance();
        for (const s of swatches.children) s.setAttribute("aria-checked", String(s === e.currentTarget));
      },
    }),
  ));
  const interval = control({ key: "interval", label: "Refresh interval", type: "number", value: app.interval, min: 500 });
  interval.addEventListener("change", () => {
    app.interval = Math.max(500, Number(interval.value) || 1500);
    save("interval", app.interval);
  });
  return h("div", null,
    group("Appearance", row("Theme", theme), row("Accent color", swatches)),
    group("Updates", row("Refresh interval", interval, "milliseconds")),
  );
}

export async function openSettings(): Promise<void> {
  let p: Record<string, any>;
  try {
    p = await prefs(true);
  } catch (e) {
    toast((e as Error).message, true);
    return;
  }
  const ctls = new Map<string, [Kind | undefined, HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement]>();
  const field = ([key, label, kind, hint]: Spec): HTMLElement | null => {
    if (!(key in p) && key !== "web_ui_password") return null;
    const v = p[key];
    const k: Kind = kind ?? (typeof v === "boolean" ? "bool" : typeof v === "number" ? "number" : "text");
    const c = Array.isArray(k)
      ? control({ key, label, type: "select", value: String(v), options: k })
      : control({
          key, label,
          type: k === "kib" ? "number" : k,
          value: k === "kib" ? Math.round((v ?? 0) / 1024) : k === "password" ? "" : Array.isArray(v) ? v.join("\n") : v,
          min: k === "kib" ? 0 : undefined,
        });
    ctls.set(key, [k, c]);
    return row(label, c, hint, k === "textarea" ? "col" : "");
  };

  const known = new Set<string>();
  const panes: [string, string, string, () => HTMLElement][] = [
    ["appearance", "Appearance", "sliders", appearancePane],
    ...PANES.map(([id, title, ic, groups]) => {
      for (const [, specs] of groups) for (const s of specs) known.add(s[0]);
      return [id, title, ic, () => h("div", null, groups.map(([gt, specs]) => group(gt, specs.map(field))))] as [string, string, string, () => HTMLElement];
    }),
  ];
  panes.push(["all", "All Settings", "list", () => {
    const keys = Object.keys(p).filter((k) => !known.has(k) && !HIDDEN.has(k) && (typeof p[k] !== "object" || Array.isArray(p[k]))).sort();
    return h("div", null,
      h("p", { class: "muted pad" }, "Every other preference, by its WebAPI name. Values are sent exactly as entered."),
      group(null, keys.map((k) => field([k, k, typeof p[k] === "string" && p[k].includes("\n") ? "textarea" : undefined]))),
    );
  }]);

  const content = h("div", { class: "set-content" });
  const built = new Map<string, HTMLElement>();
  const nav = h("nav", { class: "set-nav", "aria-label": "Settings sections" });
  let current = load("pane", "appearance");
  const show = (id: string) => {
    current = id;
    save("pane", id);
    let pane = built.get(id);
    if (!pane) {
      pane = panes.find((x) => x[0] === id)![3]();
      built.set(id, pane);
    }
    put(content, pane);
    content.scrollTop = 0;
    for (const b of nav.children) {
      b.classList.toggle("active", (b as HTMLElement).dataset.id === id);
      b.setAttribute("aria-current", String((b as HTMLElement).dataset.id === id));
    }
  };
  for (const [id, title, ic] of panes) nav.append(h("button", { type: "button", class: "sb-item", "data-id": id, onclick: () => show(id) }, icon(ic), h("span", { class: "sb-label" }, title)));
  if (!panes.some((x) => x[0] === current)) current = "appearance";

  const version = h("span", null);
  get("app/version").then((v) => (version.textContent = "WebAPI of qBittorrent " + v), () => {});

  sheet({
    title: "Settings",
    cls: "settings",
    body: h("div", { class: "set-wrap" }, nav, content),
    actions: [
      {
        label: "Log out", danger: true,
        run: async () => {
          await post("auth/logout").catch(() => {});
          location.reload();
        },
      },
      { label: "Cancel" },
      {
        label: "Save",
        primary: true,
        run: async () => {
          const out: Record<string, unknown> = {};
          for (const [key, [k, c]] of ctls) {
            let v: unknown = valueOf(c);
            if (k === "kib") v = Math.max(0, Number(v)) * 1024;
            else if (k === "password") {
              if (!v) continue;
            } else if (Array.isArray(k)) v = typeof p[key] === "number" ? Number(v) : v;
            else if (Array.isArray(p[key])) v = String(v).split("\n").map((s) => s.trim()).filter(Boolean);
            else if (typeof p[key] === "number" && typeof v === "string") v = Number(v);
            const before = Array.isArray(p[key]) ? JSON.stringify(p[key]) : p[key];
            if ((Array.isArray(v) ? JSON.stringify(v) : v) !== before) out[key] = v;
          }
          if (!Object.keys(out).length) return;
          await post("app/setPreferences", { json: JSON.stringify(out) });
          await prefs(true).catch(() => {});
          toast("Settings saved");
          emit("prefs");
        },
      },
    ],
    header: version,
  });
  show(current);
}
