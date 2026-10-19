// Sheets (native <dialog>), menus / action sheets, toasts and form controls.

import { icon } from "./icons";
import { type Child, h, isNarrow } from "./util";

const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

// ---- sheets ----------------------------------------------------------------------

export interface Action {
  label: string;
  primary?: boolean;
  danger?: boolean;
  /** Return false to keep the sheet open. */
  run?: () => unknown;
}

export interface Sheet {
  dlg: HTMLDialogElement;
  body: HTMLElement;
  close(): void;
}

export function sheet(o: { title: string; body: Child; actions?: Action[]; cls?: string; onclose?: () => void; header?: Child }): Sheet {
  const body = h("div", { class: "sheet-body" }, o.body);
  const form = h("form", { method: "dialog", novalidate: true });
  const dlg = h("dialog", { class: "sheet " + (o.cls ?? ""), "aria-label": o.title }, form);
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    const done = () => {
      dlg.close();
      dlg.remove();
      o.onclose?.();
    };
    if (reduced()) done();
    else {
      dlg.classList.add("out");
      setTimeout(done, 160);
    }
  };
  const actions = o.actions ?? [];
  const primary = actions.find((a) => a.primary);
  const run = async (a: Action) => {
    try {
      if ((await a.run?.()) !== false) close();
    } catch (e) {
      toast(String((e as Error).message ?? e), true);
    }
  };
  form.append(
    h("header", { class: "sheet-head" },
      h("h2", null, o.title),
      o.header,
      h("button", { type: "button", class: "icon-btn close", "aria-label": "Close", onclick: close }, icon("x")),
    ),
    body,
  );
  if (actions.length) {
    form.append(
      h("footer", { class: "sheet-foot" },
        actions.map((a) =>
          h("button", { type: a.primary ? "submit" : "button", class: "btn" + (a.primary ? " primary" : "") + (a.danger ? " danger" : ""), onclick: a.primary ? null : () => run(a) }, a.label),
        ),
      ),
    );
  }
  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (primary) run(primary);
  });
  dlg.addEventListener("cancel", (e) => {
    e.preventDefault();
    close();
  });
  dlg.addEventListener("click", (e) => {
    if (e.target === dlg) close();
  });
  document.body.appendChild(dlg);
  dlg.showModal();
  return { dlg, body, close };
}

export function confirmSheet(title: string, message: string, ok: string, danger = false, extra?: Child): Promise<boolean> {
  return new Promise((resolve) => {
    let result = false;
    sheet({
      title,
      cls: "small",
      body: [h("p", { class: "msg" }, message), extra],
      actions: [
        { label: "Cancel" },
        { label: ok, primary: true, danger, run: () => (result = true) },
      ],
      onclose: () => resolve(result),
    });
  });
}

// ---- form fields ------------------------------------------------------------------

export type FieldType = "text" | "number" | "bool" | "select" | "textarea" | "password";

export interface Field {
  key: string;
  label: string;
  type?: FieldType;
  value?: string | number | boolean;
  options?: [string, string][];
  placeholder?: string;
  hint?: string;
  min?: number;
  step?: number;
}

export function control(f: Field): HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement {
  const t = f.type ?? "text";
  if (t === "bool") return h("input", { type: "checkbox", class: "switch", checked: !!f.value, "aria-label": f.label });
  if (t === "select") {
    return h("select", { "aria-label": f.label }, (f.options ?? []).map(([v, l]) => h("option", { value: v, selected: String(f.value) === v }, l)));
  }
  if (t === "textarea") return h("textarea", { rows: 4, value: String(f.value ?? ""), placeholder: f.placeholder, spellcheck: false, "aria-label": f.label });
  return h("input", {
    type: t,
    value: f.value == null ? "" : String(f.value),
    placeholder: f.placeholder,
    min: f.min,
    step: f.step ?? (t === "number" ? "any" : undefined),
    autocomplete: t === "password" ? "new-password" : "off",
    spellcheck: false,
    "aria-label": f.label,
  });
}

export function valueOf(c: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement): string | number | boolean {
  if (c instanceof HTMLInputElement && c.type === "checkbox") return c.checked;
  if (c instanceof HTMLInputElement && c.type === "number") return c.value === "" ? 0 : Number(c.value);
  return c.value;
}

export function row(label: Child, ctl: Child, hint?: string, cls = ""): HTMLElement {
  return h("label", { class: "fr " + cls }, h("span", { class: "row-label" }, label, hint ? h("small", null, hint) : null), ctl);
}

export function group(title: string | null, ...rows: Child[]): HTMLElement {
  return h("section", { class: "group" }, title ? h("h3", null, title) : null, h("div", { class: "group-box" }, rows));
}

/** A sheet with simple fields; resolves to the values or null when cancelled. */
export function formSheet(title: string, fields: Field[], ok = "OK", note?: Child): Promise<Record<string, any> | null> {
  return new Promise((resolve) => {
    let result: Record<string, any> | null = null;
    const ctls = fields.map((f) => [f, control(f)] as const);
    const s = sheet({
      title,
      cls: "small",
      body: [note, group(null, ctls.map(([f, c]) => row(f.label, c, f.hint, f.type === "textarea" ? "col" : "")))],
      actions: [
        { label: "Cancel" },
        {
          label: ok,
          primary: true,
          run: () => {
            result = {};
            for (const [f, c] of ctls) result[f.key] = valueOf(c);
          },
        },
      ],
      onclose: () => resolve(result),
    });
    (s.body.querySelector("input:not([type=checkbox]),textarea,select") as HTMLElement | null)?.focus();
  });
}

// ---- menus ------------------------------------------------------------------------

export interface MenuItem {
  label: string;
  icon?: string;
  danger?: boolean;
  disabled?: boolean;
  checked?: boolean;
  run?: () => void;
  sub?: () => (MenuItem | "-")[];
}

export type MenuEntry = MenuItem | "-";

let openMenu: HTMLElement | null = null;

export function closeMenu(): void {
  if (!openMenu) return;
  const m = openMenu;
  openMenu = null;
  m.remove();
  document.removeEventListener("pointerdown", outside, true);
  document.removeEventListener("keydown", menuKeys, true);
}

function outside(e: Event): void {
  if (openMenu && !openMenu.contains(e.target as Node)) closeMenu();
}

function menuKeys(e: KeyboardEvent): void {
  if (!openMenu) return;
  const items = [...openMenu.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
  const i = items.indexOf(document.activeElement as HTMLButtonElement);
  if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    closeMenu();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    const n = items.length;
    if (n) items[e.key === "ArrowDown" ? (i + 1) % n : i <= 0 ? n - 1 : i - 1].focus();
  }
}

export function menu(entries: MenuEntry[], at: { x: number; y: number } | HTMLElement, title?: string): void {
  closeMenu();
  const sheetMode = isNarrow();
  const m = h("div", { class: "menu" + (sheetMode ? " action-sheet" : ""), role: "menu" });
  const fill = (list: MenuEntry[], back?: () => void, heading?: string) => {
    m.replaceChildren();
    if (heading && sheetMode) m.append(h("div", { class: "menu-title" }, heading));
    if (back) m.append(h("button", { class: "mi back", onclick: back }, icon("chevL"), h("span", null, "Back")), h("hr"));
    for (const e of list) {
      if (e === "-") {
        m.append(h("hr"));
        continue;
      }
      m.append(
        h("button", {
          class: "mi" + (e.danger ? " danger" : ""),
          role: e.checked === undefined ? "menuitem" : "menuitemcheckbox",
          "aria-checked": e.checked === undefined ? null : String(e.checked),
          disabled: e.disabled,
          onclick: () => {
            if (e.sub) fill(e.sub(), () => fill(entries, undefined, title), e.label);
            else {
              closeMenu();
              e.run?.();
            }
          },
        },
          h("span", { class: "mi-check" }, e.checked ? icon("check") : null),
          e.icon ? icon(e.icon) : null,
          h("span", { class: "mi-label" }, e.label),
          e.sub ? icon("chevR", "mi-sub") : null,
        ),
      );
    }
    if (sheetMode) m.append(h("button", { class: "mi cancel", onclick: closeMenu }, h("span", { class: "mi-label" }, "Cancel")));
    if (!sheetMode) place();
  };
  const place = () => {
    const r = at instanceof HTMLElement ? at.getBoundingClientRect() : { left: at.x, bottom: at.y, top: at.y, right: at.x };
    const mw = m.offsetWidth, mh = m.offsetHeight;
    let x = r.left, y = r.bottom + (at instanceof HTMLElement ? 6 : 0);
    if (x + mw > innerWidth - 8) x = Math.max(8, (at instanceof HTMLElement ? r.right : innerWidth) - mw - 8);
    if (y + mh > innerHeight - 8) y = Math.max(8, (at instanceof HTMLElement ? r.top - 6 : y) - mh);
    m.style.left = x + "px";
    m.style.top = y + "px";
  };
  document.body.appendChild(m);
  openMenu = m;
  fill(entries, undefined, title);
  document.addEventListener("pointerdown", outside, true);
  document.addEventListener("keydown", menuKeys, true);
  if (!sheetMode) (m.querySelector("button:not(:disabled)") as HTMLElement | null)?.focus({ preventScroll: true });
}

// ---- toasts -----------------------------------------------------------------------

let toastBox: HTMLElement | null = null;

export function toast(msg: string, error = false): void {
  if (!toastBox) document.body.appendChild((toastBox = h("div", { class: "toasts", role: "status", "aria-live": "polite" })));
  const t = h("div", { class: "toast" + (error ? " err" : "") }, error ? icon("warn") : icon("check"), h("span", null, msg));
  toastBox.appendChild(t);
  setTimeout(() => {
    t.classList.add("out");
    setTimeout(() => t.remove(), 300);
  }, error ? 5000 : 2600);
}
