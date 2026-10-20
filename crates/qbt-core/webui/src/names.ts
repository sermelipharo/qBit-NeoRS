// Torrent names as the macOS app shows them: a clean title, technical tags
// and a content kind, parsed with the user's own rules. The app hands its
// rules and switches to the core, which serves them at `neors/nameRules`
// (a qBit NeoRS extension). They are fetched at start, then at most once a
// minute while the UI polls and when the tab comes back; the version makes
// an unchanged answer tiny. Without them (older server, the daemon, parsing
// switched off in the app) names stay as they are.

import { ApiError, get } from "./api";
import { emit } from "./app";
import { type Chip, type ContentKind, kindForCategory, kindForExtension, pathExtension } from "./contentkind";
import { NameParseCache, NameParser, type NameRuleSpec, type ParsedName } from "./nameparser";

export type { Chip, ContentKind, ParsedName };

interface Options {
  enabled: boolean;
  semiboldTitles: boolean;
  cardTags: boolean;
  yearAsTag: boolean;
}

const OFF: Options = { enabled: false, semiboldTitles: false, cardTags: false, yearAsTag: true };
const REFETCH_MS = 60_000;
const MAX_RULES = 500;
const MAX_CATEGORY_KINDS = 256;

let options = OFF;
let cache: NameParseCache | null = null;
let version = "";
/** The server has no such endpoint (404): never asked again. */
let unsupported = false;
let inflight = false;
let fetchedAt = 0;
const categoryKinds = new Map<string, ContentKind>();

function rulesFrom(list: unknown): NameRuleSpec[] {
  if (!Array.isArray(list)) return [];
  const rules: NameRuleSpec[] = [];
  for (const r of list.slice(0, MAX_RULES)) {
    if (!r || typeof r.pattern !== "string" || typeof r.category !== "string") continue;
    rules.push({ pattern: r.pattern, category: r.category, template: typeof r.template === "string" ? r.template : "", caseSensitive: r.caseSensitive === true });
  }
  return rules;
}

function apply(r: Record<string, any>): void {
  const o = r.options && typeof r.options === "object" ? r.options : {};
  const next: Options = r.available === true
    ? { enabled: o.enabled === true, semiboldTitles: o.semiboldTitles === true, cardTags: o.cardTags === true, yearAsTag: o.yearAsTag !== false }
    : OFF;
  const rules = next.enabled ? rulesFrom(r.rules) : [];
  options = next;
  cache = rules.length ? new NameParseCache(new NameParser(rules, { yearAsChip: next.yearAsTag })) : null;
  version = typeof r.version === "string" ? r.version.slice(0, 64) : "";
  categoryKinds.clear();
  emit("names");
}

/** Fetches the rules when due (`force`: unless fetched in the last few seconds). */
export async function refreshNameRules(force = false): Promise<void> {
  const age = Date.now() - fetchedAt;
  if (unsupported || inflight || age < (force ? 5_000 : REFETCH_MS)) return;
  inflight = true;
  fetchedAt = Date.now();
  try {
    const r = await get("neors/nameRules", version ? { since: version } : undefined);
    if (r && typeof r === "object" && r.unchanged !== true) apply(r);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) unsupported = true;
  } finally {
    inflight = false;
  }
}

/** Title, tags and kind of a name, or null while names are not parsed. */
export function parsedName(name: string): ParsedName | null {
  return cache ? cache.parsed(name) : null;
}

/** Whether the list shows titles in semibold. */
export const emphasizesTitles = () => cache !== null && options.semiboldTitles;

/** Whether cards show the name's tags. */
export const showsCardTags = () => cache !== null && options.cardTags;

/** What a torrent most likely contains: from its name, else from its category's name. */
export function contentKind(name: string, category: string): ContentKind {
  const fromName = cache ? cache.parsed(name).kind : kindForExtension(pathExtension(name)) ?? "other";
  if (fromName !== "other" || !category) return fromName;
  let kind = categoryKinds.get(category);
  if (kind === undefined) {
    kind = kindForCategory(category) ?? "other";
    if (categoryKinds.size >= MAX_CATEGORY_KINDS) categoryKinds.clear();
    categoryKinds.set(category, kind);
  }
  return kind;
}
