// Splits a torrent name into a clean title and technical tags with the
// ordered rules of the macOS app (a port of its `NameParser`; the parity
// test runs the app's corpus through both). Pure: no DOM.
//
// Rules run in order. Each match claims its text: the text is masked before
// the next rule runs, so later rules cannot match across it and earlier
// rules win. Then the technical tokens at the start are skipped and the
// title runs to the first token after them; of several years before the
// first other token only the last is a year; a trailing release group
// counts only after another tag; the title's separators are normalized.

import { type Chip, ContentKindDetector, type ContentKind, isWholeToken, MAX_INPUT_LENGTH } from "./contentkind";
import { compileIcu } from "./icuregex";

/** Tag categories in display order (`NameRuleCategory`). */
export const NAME_CATEGORIES = [
  "seasonEpisode", "volume", "resolution", "hdr", "source", "videoCodec", "audio", "audioLanguage", "subtitles",
  "edition", "platform", "version", "region", "format", "year", "releaseGroup", "other", "hidden",
] as const;

const ORDER = new Map<string, number>(NAME_CATEGORIES.map((c, i) => [c, i]));

/** A rule as the server sends it (`neors/nameRules`). */
export interface NameRuleSpec {
  pattern: string;
  category: string;
  template: string;
  caseSensitive: boolean;
}

export interface ParsedName {
  /** The title with separators normalized and technical tokens removed. */
  title: string;
  /** Where the title is in the original name (UTF-16 offsets). */
  titleStart: number;
  titleEnd: number;
  /** Ordered by category, then by position; no duplicates. */
  chips: Chip[];
  kind: ContentKind;
}

export interface NameParserOptions {
  /** Off: the year is appended to the title instead ("Title (2021)"). */
  yearAsChip: boolean;
}

interface CompiledRule {
  re: RegExp;
  groups: number;
  category: string;
  template: string;
}

interface Token {
  start: number;
  end: number;
  category: string;
  text: string;
}

export const MAX_PATTERN_LENGTH = 300;
export const MAX_TEMPLATE_LENGTH = 80;
/** Backstop for one name: later rules are skipped once it is spent. */
export const TIME_BUDGET_MS = 25;

const now = () => performance.now();

/** The largest `$n` a template refers to (`\$` is a literal dollar sign). */
export function highestGroup(template: string): number | null {
  let highest: number | null = null;
  const chars = [...template];
  for (let i = 0; i < chars.length; i++) {
    if (chars[i] === "\\") {
      i++;
      continue;
    }
    if (chars[i] !== "$") continue;
    let digits = "";
    while (i + 1 < chars.length && /[0-9]/.test(chars[i + 1])) digits += chars[++i];
    if (digits) highest = Math.max(highest ?? 0, parseInt(digits, 10));
  }
  return highest;
}

/** The rule compiled, or null when the app would not use it or JS cannot run it. */
export function compileRule(spec: NameRuleSpec, spellOutCase = false): CompiledRule | null {
  const pattern = String(spec.pattern ?? "");
  const template = String(spec.template ?? "");
  if (!pattern.replace(/[ \t]/g, "") || pattern.length > MAX_PATTERN_LENGTH || template.length > MAX_TEMPLATE_LENGTH) return null;
  const c = compileIcu(pattern, !spec.caseSensitive, spellOutCase);
  if (!c) return null;
  const group = highestGroup(template);
  if (group != null && group > c.groups) return null;
  const category = ORDER.has(spec.category) ? spec.category : "other";
  return { re: c.re, groups: c.groups, category, template };
}

/** NSRegularExpression template: `$n` (as many digits as the group count has), `\` escapes. */
function expandTemplate(template: string, m: RegExpMatchArray, source: string, groups: number): string {
  const maxDigits = String(Math.max(groups, 1)).length;
  const indices = m.indices!;
  let out = "";
  for (let i = 0; i < template.length; i++) {
    const c = template[i];
    if (c === "\\" && i + 1 < template.length) {
      out += template[++i];
    } else if (c === "$" && /[0-9]/.test(template[i + 1] ?? "")) {
      let digits = "";
      while (digits.length < maxDigits && /[0-9]/.test(template[i + 1] ?? "")) digits += template[++i];
      const g = parseInt(digits, 10);
      const span = g <= groups ? indices[g] : undefined;
      if (span) out += source.slice(span[0], span[1]);
    } else out += c;
  }
  return out;
}

// ---- text helpers ------------------------------------------------------------------

const segmenter = typeof Intl !== "undefined" && "Segmenter" in Intl ? new Intl.Segmenter() : null;

/** User-perceived characters (Swift `Character`s). */
function graphemes(s: string): string[] {
  if (!segmenter) return [...s];
  const out: string[] = [];
  for (const g of segmenter.segment(s)) out.push(g.segment);
  return out;
}

const WHITESPACE = /^[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;
const ANY_WHITESPACE = /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;
const NUMBER = /^\p{N}/u;
const WORD_CHARS = /[\p{L}\p{M}\p{N}]/u;

/** Runs of whitespace become one space; none at either end. */
function collapseWhitespace(s: string): string {
  if (!ANY_WHITESPACE.test(s)) return s;
  const words: string[] = [];
  let word = "";
  for (const g of graphemes(s)) {
    if (WHITESPACE.test(g)) {
      if (word) words.push(word);
      word = "";
    } else word += g;
  }
  if (word) words.push(word);
  return words.join(" ");
}

export const normalizedTag = collapseWhitespace;

const SEPARATORS = " \t._-–—|/\\:;,+~#*\u0001";
const JUNK = new Set([...SEPARATORS, ..."[](){}<>"]);
const LEADING_TRIM = new Set([...SEPARATORS, ...")]}>"]);
const TRAILING_TRIM = new Set([...SEPARATORS, ..."([{<"]);
const PAIRS: Record<string, string> = { "[": "]", "(": ")", "{": "}" };

const isSurrogate = (code: number) => code >= 0xd800 && code <= 0xdfff;

export function isJunk(text: string): boolean {
  for (const c of text) if (!JUNK.has(c)) return false;
  return true;
}

export const hasWordCharacters = (text: string) => WORD_CHARS.test(text);

/** The range without separators and dangling brackets at either end, and without brackets around all of it. */
export function trimmed(start: number, end: number, name: string): [number, number] {
  for (;;) {
    while (start < end && !isSurrogate(name.charCodeAt(start)) && LEADING_TRIM.has(name[start])) start++;
    while (end > start && !isSurrogate(name.charCodeAt(end - 1)) && TRAILING_TRIM.has(name[end - 1])) end--;
    if (end - start < 2) break;
    const open = name[start], close = name[end - 1];
    if (isSurrogate(name.charCodeAt(start)) || isSurrogate(name.charCodeAt(end - 1)) || PAIRS[open] !== close) break;
    const inner = name.slice(start + 1, end - 1);
    if (inner.includes(open) || inner.includes(close)) break;
    start++;
    end--;
  }
  return [start, end];
}

/** Dots and underscores as word separators become spaces; runs of spaces collapse. */
export function cleanTitle(raw: string): string {
  const chars = graphemes(raw.replaceAll("_", " "));
  const dotsSeparate = !chars.includes(" ");
  let result = "";
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i];
    if (c === "." && dotsSeparate) {
      const betweenDigits = i > 0 && i + 1 < chars.length && NUMBER.test(chars[i - 1]) && NUMBER.test(chars[i + 1]);
      result += betweenDigits ? "." : " ";
    } else result += c;
  }
  return collapseWhitespace(result);
}

/** Case and leading zeros don't make a tag different ("S1" and "S01"). */
export function dedupKey(text: string): string {
  let key = "";
  let previousIsDigit = false;
  let pendingZeros = "";
  for (const c of graphemes(text.toLowerCase())) {
    if (c === "0" && !previousIsDigit) {
      pendingZeros += c;
      continue;
    }
    if (NUMBER.test(c)) {
      pendingZeros = "";
      previousIsDigit = true;
    } else {
      key += pendingZeros;
      pendingZeros = "";
      previousIsDigit = false;
    }
    key += c;
  }
  return key + pendingZeros;
}

// ---- parser ----------------------------------------------------------------------------

export class NameParser {
  readonly rules: CompiledRule[];
  readonly detector: ContentKindDetector;
  /** The last parse ran out of time and skipped rules (not worth caching). */
  truncated = false;

  constructor(specs: NameRuleSpec[], readonly options: NameParserOptions = { yearAsChip: true }, spellOutCase = false) {
    this.rules = [];
    for (const s of specs) {
      const r = compileRule(s, spellOutCase);
      if (r) this.rules.push(r);
    }
    // Engines compile an expression on its first run, separately for Latin-1
    // and other text: do it now, so the per-name budget only counts matching.
    for (const r of this.rules) {
      r.re.test("a");
      r.re.lastIndex = 0;
      r.re.test("aĀ");
      r.re.lastIndex = 0;
    }
    this.detector = new ContentKindDetector(spellOutCase);
  }

  private plain(name: string): ParsedName {
    return { title: name, titleStart: 0, titleEnd: name.length, chips: [], kind: this.detector.kind(name, []) };
  }

  parse(name: string): ParsedName {
    this.truncated = false;
    const length = Math.min(name.length, MAX_INPUT_LENGTH);
    if (length === 0 || this.rules.length === 0) return this.plain(name);
    const source = length < name.length ? name.slice(0, length) : name;
    let tokens = this.match(source);
    if (!tokens.length) return this.plain(name);
    tokens.sort((a, b) => a.start - b.start);

    const titleStart = leadingEnd(tokens, source);
    tokens = disambiguateYears(tokens, titleStart);
    tokens = dropLoneTrailingGroups(tokens, titleStart, source);

    const titleEnd = tokens.find((t) => t.start >= titleStart)?.start ?? name.length;
    let range = trimmed(titleStart, titleStart + Math.max(titleEnd - titleStart, 0), name);
    if (!hasWordCharacters(name.slice(range[0], range[1]))) range = fallbackTitleRange(tokens, name);
    let title = cleanTitle(name.slice(range[0], range[1]));

    let chips = chipsFrom(tokens);
    const kind = this.detector.kind(name, chips);
    if (!this.options.yearAsChip) {
      const year = chips.find((c) => c.category === "year");
      if (year) {
        chips = chips.filter((c) => c.category !== "year");
        if (!title.endsWith(year.text)) title += ` (${year.text})`;
      }
    }
    return { title, titleStart: range[0], titleEnd: range[1], chips, kind };
  }

  private match(source: string): Token[] {
    const deadline = now() + TIME_BUDGET_MS;
    this.truncated = false;
    const tokens: Token[] = [];
    const claimed: [number, number][] = [];
    let work = source;
    for (const rule of this.rules) {
      if (now() >= deadline) {
        this.truncated = true;
        break;
      }
      const found: [number, number][] = [];
      for (const m of work.matchAll(rule.re)) {
        const start = m.index!, end = start + m[0].length;
        if (end === start || !isWholeToken(start, end, work) || claimed.some(([s, e]) => s < end && start < e)) continue;
        const raw = rule.template === "" ? source.slice(start, end) : expandTemplate(rule.template, m, source, rule.groups);
        tokens.push({ start, end, category: rule.category, text: normalizedTag(raw) });
        found.push([start, end]);
      }
      // Masked text separates tokens like punctuation and matches nothing.
      for (const [s, e] of found) {
        claimed.push([s, e]);
        work = work.slice(0, s) + "\u0001".repeat(e - s) + work.slice(e);
      }
    }
    return tokens;
  }
}

/** End of the technical tokens at the very start ("[Group] ", site prefixes). */
function leadingEnd(tokens: Token[], source: string): number {
  let end = 0;
  for (const t of tokens) {
    if (t.start < end) continue;
    if (!isJunk(source.slice(end, t.start))) break;
    end = t.end;
  }
  return end;
}

/** Keeps only the last year before the first other token; earlier ones are part of the title. */
function disambiguateYears(tokens: Token[], start: number): Token[] {
  const firstOther = tokens.find((t) => t.start >= start && t.category !== "year")?.start ?? Infinity;
  const early = tokens.filter((t) => t.category === "year" && t.start >= start && t.start < firstOther);
  if (early.length < 2) return tokens;
  const kept = early[early.length - 1].start;
  const dropped = new Set(early.map((t) => t.start).filter((s) => s !== kept));
  return tokens.filter((t) => !(t.category === "year" && dropped.has(t.start)));
}

/** A release group after the title ("-GROUP", "[Group]", "от Group") needs another tag before it. */
function dropLoneTrailingGroups(tokens: Token[], start: number, source: string): Token[] {
  return tokens.filter((t) => {
    if (t.category !== "releaseGroup" || t.start < start || t.start === 0 || !"-[оО".includes(source[t.start])) return true;
    return tokens.some((o) => o.start < t.start && o.category !== "releaseGroup" && o.category !== "hidden");
  });
}

/** The first stretch between tokens that has letters or digits, else the whole name. */
function fallbackTitleRange(tokens: Token[], name: string): [number, number] {
  let cursor = 0;
  for (const t of [...tokens, { start: name.length, end: name.length }]) {
    if (t.start > cursor) {
      const r = trimmed(cursor, t.start, name);
      if (hasWordCharacters(name.slice(r[0], r[1]))) return r;
    }
    cursor = Math.max(cursor, t.end);
  }
  return [0, name.length];
}

/** Parse results per name; bounded: when full it starts over (like the app's `NameParseCache`). */
export class NameParseCache {
  static readonly LIMIT = 4096;
  private readonly entries = new Map<string, ParsedName>();

  constructor(readonly parser: NameParser, private readonly limit = NameParseCache.LIMIT) {}

  get size(): number {
    return this.entries.size;
  }

  parsed(name: string): ParsedName {
    const hit = this.entries.get(name);
    if (hit) return hit;
    const result = this.parser.parse(name);
    if (this.parser.truncated) return result;
    if (this.entries.size >= this.limit) this.entries.clear();
    this.entries.set(name, result);
    return result;
  }
}

/** Ordered by category, then position; the same text in a category appears once. */
function chipsFrom(tokens: Token[]): Chip[] {
  const candidates = tokens
    .map((t, i) => ({ t, i }))
    .filter(({ t }) => t.category !== "hidden" && t.text !== "")
    .sort((a, b) => ORDER.get(a.t.category)! - ORDER.get(b.t.category)! || a.i - b.i);
  const seen = new Set<string>();
  const chips: Chip[] = [];
  for (const { t } of candidates) {
    const key = t.category + "\u0000" + dedupKey(t.text);
    if (seen.has(key)) continue;
    seen.add(key);
    chips.push({ category: t.category, text: t.text });
  }
  return chips;
}
