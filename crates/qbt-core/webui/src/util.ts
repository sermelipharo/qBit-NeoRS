// DOM and formatting helpers. Every piece of text goes through text nodes
// (`textContent`); nothing in the UI is ever parsed as HTML.

export type Child = Node | string | number | null | undefined | false | Child[];
type Handler = (e: any) => void;
export type Props = Record<string, string | number | boolean | Handler | null | undefined>;

const PROPS = new Set(["value", "checked", "disabled", "hidden", "selected", "multiple", "title", "placeholder", "tabIndex", "type", "name", "min", "max", "step", "rows", "autocomplete", "spellcheck", "draggable", "accept"]);

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, props?: Props | null, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (props) {
    for (const k in props) {
      const v = props[k];
      if (v == null || v === false) continue;
      if (k === "class") el.className = String(v);
      else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v as Handler);
      else if (PROPS.has(k)) (el as any)[k] = v;
      else el.setAttribute(k, v === true ? "" : String(v));
    }
  }
  append(el, children);
  return el;
}

export function append(el: Node, children: Child[]): void {
  for (const c of children) {
    if (c == null || c === false) continue;
    if (Array.isArray(c)) append(el, c);
    else el.appendChild(typeof c === "object" ? c : document.createTextNode(String(c)));
  }
}

/** Replaces the children of `el`. */
export function put(el: Element, ...children: Child[]): void {
  el.replaceChildren();
  append(el, children);
}

/** Sets text only when it changed (no layout work for unchanged cells). */
export function setText(el: Element, s: string): void {
  if (el.textContent !== s) el.textContent = s;
}

export function toggle(el: Element, cls: string, on: boolean): void {
  if (el.classList.contains(cls) !== on) el.classList.toggle(cls, on);
}

export const $ = <T extends Element = HTMLElement>(sel: string, root: ParentNode = document) => root.querySelector(sel) as T | null;

// ---- storage (per-browser conveniences only) --------------------------------

export function load<T>(key: string, def: T): T {
  try {
    const v = localStorage.getItem("neors." + key);
    return v == null ? def : (JSON.parse(v) as T);
  } catch {
    return def;
  }
}

export function save(key: string, v: unknown): void {
  try {
    localStorage.setItem("neors." + key, JSON.stringify(v));
  } catch {
    /* private mode */
  }
}

// ---- formatting ---------------------------------------------------------------

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

export function bytes(n: number): string {
  if (!(n >= 0)) return "–";
  let i = 0;
  while (n >= 1000 && i < UNITS.length - 1) {
    n /= 1000;
    i++;
  }
  return (i === 0 ? n.toFixed(0) : n.toFixed(n < 10 ? 2 : n < 100 ? 1 : 0)) + " " + UNITS[i];
}

export const speed = (n: number): string => (n > 0 ? bytes(n) + "/s" : "–");

export function eta(s: number): string {
  if (!(s >= 0) || s >= 8640000) return "∞";
  if (s < 60) return s + "s";
  const m = Math.floor(s / 60), hr = Math.floor(m / 60), d = Math.floor(hr / 24);
  if (d > 0) return d + "d " + (hr % 24) + "h";
  if (hr > 0) return hr + "h " + (m % 60) + "m";
  return m + "m";
}

export const duration = eta;

export const pct = (p: number): string => (p >= 1 ? "100%" : (Math.floor(p * 1000) / 10).toFixed(1) + "%");

export const ratio = (r: number): string => (r < 0 ? "–" : r >= 9999 ? "∞" : r.toFixed(2));

export function date(ts: number): string {
  if (!(ts > 0)) return "–";
  return new Date(ts * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function shortDate(ts: number): string {
  if (!(ts > 0)) return "–";
  const d = new Date(ts * 1000), now = new Date();
  return d.toDateString() === now.toDateString()
    ? d.toLocaleTimeString(undefined, { timeStyle: "short" })
    : d.toLocaleDateString(undefined, { dateStyle: "medium" });
}

export const num = (n: number): string => (n >= 0 ? n.toLocaleString() : "–");

/** Only http(s) links may be opened (no `javascript:` or `data:`). */
export function safeUrl(u: string): string | null {
  try {
    const url = new URL(u);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

export function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number): (...a: A) => void {
  let t = 0;
  return (...a: A) => {
    clearTimeout(t);
    t = window.setTimeout(() => fn(...a), ms);
  };
}

export const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

export const isNarrow = () => matchMedia("(max-width: 760px)").matches;
