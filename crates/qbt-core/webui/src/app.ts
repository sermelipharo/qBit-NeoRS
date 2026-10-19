// Shared UI state and a tiny event bus.

import { get } from "./api";
import { load } from "./util";

export type Section = "transfers" | "search" | "rss";

export const app = {
  section: "transfers" as Section,
  view: load<"list" | "cards">("view", "list"),
  selection: new Set<string>(),
  /** Anchor for shift-click ranges and keyboard moves. */
  anchor: null as string | null,
  /** Torrent shown in the details pane / sheet. */
  details: null as string | null,
  inspector: load("inspector", true),
  selectMode: false,
  interval: load("interval", 1500),
  prefs: null as Record<string, any> | null,
};

type Handler = () => void;
const handlers = new Map<string, Set<Handler>>();

export function on(name: string, f: Handler): void {
  let s = handlers.get(name);
  if (!s) handlers.set(name, (s = new Set()));
  s.add(f);
}

export function emit(name: string): void {
  handlers.get(name)?.forEach((f) => f());
}

export async function prefs(force = false): Promise<Record<string, any>> {
  if (!app.prefs || force) app.prefs = await get("app/preferences");
  return app.prefs!;
}

let poke: () => void = () => {};
export const setPoke = (f: () => void) => (poke = f);
/** Polls again soon (after an action), so the UI reflects it quickly. */
export const refreshSoon = () => poke();
