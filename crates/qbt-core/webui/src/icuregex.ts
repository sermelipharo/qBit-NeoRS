// Runs the torrent name rules, written for NSRegularExpression (ICU), as
// JavaScript regular expressions with the same meaning:
// * `\w`, `\d`, `\s`, `\b` are Unicode-aware in ICU and become explicit
//   property classes (JS keeps them ASCII-only);
// * escapes JS rejects in `u` mode (`\-`, `\ `, `\Q…\E`, `\x{…}`) become
//   plain characters; POSIX-like `\p{Latin}` gets its `Script=`;
// * inline case flags (`(?-i:…)`, `(?i)`) use JS modifiers where the engine
//   has them; otherwise the case-insensitive parts are spelled out
//   (`a` -> `[aA]`) and the expression runs case-sensitively.
// ICU-only syntax (atomic groups, possessive quantifiers, nested or
// POSIX sets, other inline flags, `\G`, `\X`, `\N{…}`) yields null: the
// caller skips such a rule (the app warns about it when it is edited).

/** ICU's `\w`: alphabetic, marks, decimal digits, connectors, ZWNJ/ZWJ. */
const WORD = String.raw`\p{Alphabetic}\p{M}\p{Nd}\p{Pc}\u200C\u200D`;
const LINE_END = String.raw`\n\v\f\r\u0085\u2028\u2029`;
const SYNTAX = new Set([..."^$\\.*+?()[]{}|/"]);
const GENERAL_CATEGORIES = ["L", "LC", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc", "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "S", "Sm", "Sc", "Sk", "So", "Z", "Zs", "Zl", "Zp", "C", "Cc", "Cf", "Cs", "Co", "Cn"];
const CASED_CATEGORIES = new Set(["Lu", "Ll", "Lt", "Uppercase_Letter", "Lowercase_Letter", "Titlecase_Letter", "General_Category=Lu", "General_Category=Ll", "General_Category=Lt"]);

export interface IcuRegex {
  /** Flags `g`, `d` (match indices) and `u`, plus `i` when it applies. */
  re: RegExp;
  /** Number of capture groups. */
  groups: number;
}

/** Compiles an ICU pattern, or null when JavaScript cannot run it the same way. */
export function compileIcu(pattern: string, caseInsensitive: boolean, spellOutCase = false): IcuRegex | null {
  if (!spellOutCase) {
    const t = translate(pattern, caseInsensitive, true);
    if (!t) return null;
    const re = make(t.source, caseInsensitive ? "dgiu" : "dgu");
    // An engine without regular expression modifiers: spell the case out.
    if (re || !t.modifiers) return re;
  }
  const t = translate(pattern, caseInsensitive, false);
  return t ? make(t.source, "dgu") : null;
}

function make(source: string, flags: string): IcuRegex | null {
  try {
    const re = new RegExp(source, flags);
    const groups = new RegExp(source + "|", flags.replace("g", "")).exec("")!.length - 1;
    return { re, groups };
  } catch {
    return null;
  }
}

const propertyCache = new Map<string, string | null>();

function validProperty(name: string): boolean {
  try {
    new RegExp(`\\p{${name}}`, "u");
    return true;
  } catch {
    return false;
  }
}

const capitalized = (s: string) => s.split("_").map((w) => w.charAt(0).toUpperCase() + w.slice(1).toLowerCase()).join("_");

/** The JS name of an ICU property (`Latin` -> `Script=Latin`), or null. */
export function jsProperty(icu: string): string | null {
  const cached = propertyCache.get(icu);
  if (cached !== undefined) return cached;
  const name = icu.trim().replace(/\s+/g, "_");
  const candidates: string[] = [name];
  const eq = name.indexOf("=");
  if (eq > 0) {
    const key = name.slice(0, eq).toLowerCase();
    const value = name.slice(eq + 1);
    const keys: Record<string, string> = { script: "Script", sc: "Script", script_extensions: "Script_Extensions", scx: "Script_Extensions", general_category: "General_Category", gc: "General_Category" };
    if (keys[key]) candidates.push(`${keys[key]}=${value}`, `${keys[key]}=${capitalized(value)}`);
  } else if (!/^In/.test(name)) {
    const bare = /^Is[A-Z]/.test(name) ? name.slice(2) : name;
    const gc = GENERAL_CATEGORIES.find((c) => c.toLowerCase() === bare.toLowerCase());
    if (gc) candidates.push(gc);
    candidates.push(bare, capitalized(bare), `Script=${capitalized(bare)}`);
  }
  const found = candidates.find(validProperty) ?? null;
  propertyCache.set(icu, found);
  return found;
}

interface Frame {
  ci: boolean;
  /** Native mode: the modifier group opened by an inline flag (`(?i:`), closed at `|` and `)`. */
  implicit: string;
}

/** A class member: one character, a range, or ready-made class text. */
type Atom = { c: string } | { lo: string; hi: string } | { raw: string };

const hex = (cp: number) => `\\u{${cp.toString(16)}}`;

/** A character as JS class text. */
function classChar(c: string): string {
  const cp = c.codePointAt(0)!;
  if (cp < 0x20 || cp === 0x7f) return hex(cp);
  return "\\]^-[".includes(c) ? "\\" + c : c;
}

/** The other cases of a character (simple mappings only). */
function caseVariants(c: string): string[] {
  const out: string[] = [];
  for (const v of [c.toLowerCase(), c.toUpperCase()]) {
    if (v !== c && [...v].length === 1 && !out.includes(v)) out.push(v);
  }
  return out;
}

class Translator {
  private i = 0;
  private out = "";
  private frames: Frame[];
  modifiers = false;

  constructor(private readonly p: string[], private readonly native: boolean, ci: boolean) {
    this.frames = [{ ci, implicit: "" }];
  }

  private get top(): Frame {
    return this.frames[this.frames.length - 1];
  }

  /** Case-insensitive here and spelled out (not left to the engine). */
  private get spell(): boolean {
    return !this.native && this.top.ci;
  }

  run(): string | null {
    const p = this.p;
    while (this.i < p.length) {
      const c = p[this.i];
      let ok = true;
      switch (c) {
        case "\\": ok = this.escape(); break;
        case "[": ok = this.cls(); break;
        case "(": ok = this.open(); break;
        case ")": ok = this.close(); break;
        case "|":
          this.out += this.top.implicit ? ")|" + this.top.implicit : "|";
          this.i++;
          break;
        case "*": case "+": case "?":
          this.out += c;
          this.i++;
          ok = this.quantifierSuffix();
          break;
        case "{": {
          const m = /^\{\d+(?:,\d*)?\}/.exec(p.slice(this.i, this.i + 24).join(""));
          if (!m) return null;
          this.out += m[0];
          this.i += m[0].length;
          ok = this.quantifierSuffix();
          break;
        }
        case "}": return null;
        case "]":
          this.out += "\\]";
          this.i++;
          break;
        case "^": case "$": case ".":
          this.out += c;
          this.i++;
          break;
        default:
          this.literal(c);
          this.i++;
      }
      if (!ok) return null;
    }
    if (this.frames.length !== 1) return null;
    if (this.top.implicit) this.out += ")";
    return this.out;
  }

  /** Lazy is fine; possessive quantifiers have no JS equivalent. */
  private quantifierSuffix(): boolean {
    if (this.p[this.i] === "+") return false;
    if (this.p[this.i] === "?") {
      this.out += "?";
      this.i++;
    }
    return true;
  }

  private literal(c: string): void {
    const variants = this.spell ? caseVariants(c) : [];
    if (variants.length) this.out += "[" + [c, ...variants].map(classChar).join("") + "]";
    else if (SYNTAX.has(c)) this.out += "\\" + c;
    else {
      const cp = c.codePointAt(0)!;
      this.out += cp < 0x20 || cp === 0x7f ? hex(cp) : c;
    }
  }

  private property(negated: boolean, inClass: boolean): string | null {
    const p = this.p;
    let name: string;
    if (p[this.i] === "{") {
      const end = p.indexOf("}", this.i);
      if (end < 0) return null;
      name = p.slice(this.i + 1, end).join("");
      this.i = end + 1;
    } else {
      name = p[this.i] ?? "";
      this.i++;
    }
    const js = jsProperty(name);
    if (!js) return null;
    if (this.spell && CASED_CATEGORIES.has(js)) {
      // Case-insensitive `\p{Lu}` matches every cased letter.
      if (inClass) return negated ? null : String.raw`\p{Lu}\p{Ll}\p{Lt}`;
      return negated ? String.raw`[^\p{Lu}\p{Ll}\p{Lt}]` : String.raw`[\p{Lu}\p{Ll}\p{Lt}]`;
    }
    return `\\${negated ? "P" : "p"}{${js}}`;
  }

  /** `\x..`, `\x{..}`, `\u....`, `\U........`, `\0ooo`: the character, or null. */
  private codePoint(kind: string): string | null {
    const rest = this.p.slice(this.i, this.i + 12).join("");
    const m = kind === "x" ? /^\{([0-9A-Fa-f]{1,6})\}|^([0-9A-Fa-f]{1,2})/.exec(rest)
      : kind === "u" ? /^([0-9A-Fa-f]{4})/.exec(rest)
      : kind === "U" ? /^([0-9A-Fa-f]{8})/.exec(rest)
      : /^([0-7]{0,3})/.exec(rest);
    if (!m) return null;
    const digits = m[1] ?? m[2] ?? "";
    const cp = digits === "" ? 0 : parseInt(digits, kind === "0" ? 8 : 16);
    if (cp > 0x10ffff) return null;
    this.i += m[0].length;
    return String.fromCodePoint(cp);
  }

  private static readonly SIMPLE: Record<string, string> = { t: "\t", n: "\n", r: "\r", f: "\f", e: "\x1b", a: "\x07" };

  private escape(): boolean {
    const p = this.p;
    const e = p[this.i + 1];
    this.i += 2;
    if (e === undefined) return false;
    switch (e) {
      case "w": this.out += `[${WORD}]`; return true;
      case "W": this.out += `[^${WORD}]`; return true;
      case "d": this.out += String.raw`\p{Nd}`; return true;
      case "D": this.out += String.raw`\P{Nd}`; return true;
      case "s": this.out += String.raw`\p{White_Space}`; return true;
      case "S": this.out += String.raw`\P{White_Space}`; return true;
      case "h": this.out += String.raw`[\t\p{Zs}]`; return true;
      case "H": this.out += String.raw`[^\t\p{Zs}]`; return true;
      case "v": this.out += `[${LINE_END}]`; return true;
      case "V": this.out += `[^${LINE_END}]`; return true;
      case "R": this.out += `(?:\\r\\n|[${LINE_END}])`; return true;
      case "b": this.out += `(?:(?<=[${WORD}])(?![${WORD}])|(?<![${WORD}])(?=[${WORD}]))`; return true;
      case "B": this.out += `(?:(?<=[${WORD}])(?=[${WORD}])|(?<![${WORD}])(?![${WORD}]))`; return true;
      case "A": this.out += "^"; return true;
      case "z": this.out += "$"; return true;
      case "Z": this.out += `(?=(?:\\r\\n|[${LINE_END}])?$)`; return true;
      case "p": case "P": {
        const prop = this.property(e === "P", false);
        if (prop == null) return false;
        this.out += prop;
        return true;
      }
      case "Q": {
        while (this.i < p.length && !(p[this.i] === "\\" && p[this.i + 1] === "E")) this.literal(p[this.i++]);
        if (this.i < p.length) this.i += 2;
        return true;
      }
      case "x": case "u": case "U": case "0": {
        const c = this.codePoint(e);
        if (c == null) return false;
        this.literal(c);
        return true;
      }
      case "c": {
        const x = p[this.i];
        if (!x || !/[A-Za-z]/.test(x)) return false;
        this.i++;
        this.literal(String.fromCharCode(x.toUpperCase().charCodeAt(0) ^ 0x40));
        return true;
      }
      case "k": {
        const m = /^<([A-Za-z][A-Za-z0-9]*)>/.exec(p.slice(this.i, this.i + 40).join(""));
        if (!m) return false;
        this.out += "\\k" + m[0];
        this.i += m[0].length;
        return true;
      }
    }
    if (Translator.SIMPLE[e]) {
      this.literal(Translator.SIMPLE[e]);
      return true;
    }
    if (/[1-9]/.test(e)) {
      let digits = e;
      while (/[0-9]/.test(p[this.i] ?? "")) digits += p[this.i++];
      this.out += "\\" + digits;
      return true;
    }
    // ICU rejects other ASCII letter and digit escapes (`\G`, `\X`, `\N{…}` are unsupported here).
    if (/[A-Za-z0-9]/.test(e)) return false;
    this.literal(e);
    return true;
  }

  private open(): boolean {
    const p = this.p;
    const ci = this.top.ci;
    if (p[this.i + 1] !== "?") {
      this.frames.push({ ci, implicit: "" });
      this.out += "(";
      this.i++;
      return true;
    }
    const rest = p.slice(this.i + 2, this.i + 48).join("");
    const plain = /^(?::|=|!|<=|<!)/.exec(rest);
    if (plain) {
      this.frames.push({ ci, implicit: "" });
      this.out += "(?" + plain[0];
      this.i += 2 + plain[0].length;
      return true;
    }
    const named = /^<([A-Za-z][A-Za-z0-9]*)>/.exec(rest);
    if (named) {
      this.frames.push({ ci, implicit: "" });
      this.out += "(?" + named[0];
      this.i += 2 + named[0].length;
      return true;
    }
    if (rest.startsWith("#")) {
      const end = p.indexOf(")", this.i);
      if (end < 0) return false;
      this.i = end + 1;
      return true;
    }
    const flags = /^([A-Za-z]*)(?:-([A-Za-z]*))?([:)])/.exec(rest);
    if (!flags || (!flags[1] && !flags[2])) return false;
    const on = flags[1], off = flags[2] ?? "";
    if (/[^i]/.test(on + off)) return false;
    const next = on.includes("i") ? true : off.includes("i") ? false : ci;
    const modifier = `(?${on ? "i" : ""}${off ? "-i" : ""}:`;
    this.i += 2 + flags[0].length;
    if (flags[3] === ":") {
      this.frames.push({ ci: next, implicit: "" });
      if (this.native) {
        this.out += modifier;
        this.modifiers = true;
      } else this.out += "(?:";
      return true;
    }
    // `(?i)`: to the end of the enclosing group, in every later alternative.
    const top = this.top;
    top.ci = next;
    if (this.native) {
      if (top.implicit) this.out += ")";
      top.implicit = modifier;
      this.out += modifier;
      this.modifiers = true;
    }
    return true;
  }

  private close(): boolean {
    if (this.frames.length === 1) return false;
    const frame = this.frames.pop()!;
    this.out += frame.implicit ? "))" : ")";
    this.i++;
    return true;
  }

  // ---- character classes ----------------------------------------------------------

  private cls(): boolean {
    const p = this.p;
    if (/^\[\^?:[A-Za-z_ =]+:\]/.test(p.slice(this.i, this.i + 40).join(""))) return false;
    this.i++;
    let negated = false;
    if (p[this.i] === "^") {
      negated = true;
      this.i++;
    }
    const atoms: Atom[] = [];
    while (true) {
      const c = p[this.i];
      if (c === undefined || c === "[") return false;
      if (c === "]") {
        this.i++;
        break;
      }
      if ((c === "&" && p[this.i + 1] === "&") || (c === "-" && p[this.i + 1] === "-")) return false;
      const atom = this.classAtom();
      if (!atom) return false;
      if ("c" in atom && p[this.i] === "-" && p[this.i + 1] !== "]" && p[this.i + 1] !== undefined) {
        this.i++;
        const hi = this.classAtom();
        if (!hi || !("c" in hi) || hi.c.codePointAt(0)! < atom.c.codePointAt(0)!) return false;
        atoms.push({ lo: atom.c, hi: hi.c });
      } else atoms.push(atom);
    }
    let body = "";
    for (const a of atoms) {
      if ("raw" in a) body += a.raw;
      else if ("c" in a) body += [a.c, ...(this.spell ? caseVariants(a.c) : [])].map(classChar).join("");
      else body += classChar(a.lo) + "-" + classChar(a.hi) + (this.spell ? rangeVariants(a.lo, a.hi) : "");
    }
    this.out += "[" + (negated ? "^" : "") + body + "]";
    return true;
  }

  private classAtom(): Atom | null {
    const p = this.p;
    const c = p[this.i];
    if (c !== "\\") {
      this.i++;
      return { c };
    }
    const e = p[this.i + 1];
    this.i += 2;
    if (e === undefined) return null;
    switch (e) {
      case "d": return { raw: String.raw`\p{Nd}` };
      case "D": return { raw: String.raw`\P{Nd}` };
      case "w": return { raw: WORD };
      case "s": return { raw: String.raw`\p{White_Space}` };
      case "S": return { raw: String.raw`\P{White_Space}` };
      case "h": return { raw: String.raw`\t\p{Zs}` };
      case "v": return { raw: LINE_END };
      case "b": return { c: "\b" };
      case "p": case "P": {
        const prop = this.property(e === "P", true);
        return prop == null ? null : { raw: prop };
      }
      case "Q": {
        let raw = "";
        while (this.i < p.length && !(p[this.i] === "\\" && p[this.i + 1] === "E")) {
          const q = p[this.i++];
          raw += [q, ...(this.spell ? caseVariants(q) : [])].map(classChar).join("");
        }
        if (this.i < p.length) this.i += 2;
        return { raw };
      }
      case "x": case "u": case "U": case "0": {
        const ch = this.codePoint(e);
        return ch == null ? null : { c: ch };
      }
      case "c": {
        const x = p[this.i];
        if (!x || !/[A-Za-z]/.test(x)) return null;
        this.i++;
        return { c: String.fromCharCode(x.toUpperCase().charCodeAt(0) ^ 0x40) };
      }
    }
    if (Translator.SIMPLE[e]) return { c: Translator.SIMPLE[e] };
    if (/[A-Za-z0-9]/.test(e)) return null;
    return { c: e };
  }
}

/** Class text for the other cases of the characters in lo…hi (small ranges only). */
function rangeVariants(lo: string, hi: string): string {
  const a = lo.codePointAt(0)!, b = hi.codePointAt(0)!;
  if (b - a > 4096) return "";
  const extra: number[] = [];
  for (let cp = a; cp <= b; cp++) {
    for (const v of caseVariants(String.fromCodePoint(cp))) {
      const vc = v.codePointAt(0)!;
      if (vc < a || vc > b) extra.push(vc);
    }
  }
  extra.sort((x, y) => x - y);
  let out = "";
  for (let k = 0; k < extra.length; ) {
    let end = k;
    while (end + 1 < extra.length && extra[end + 1] <= extra[end] + 1) end++;
    const s = String.fromCodePoint(extra[k]), e = String.fromCodePoint(extra[end]);
    out += extra[k] === extra[end] ? classChar(s) : classChar(s) + "-" + classChar(e);
    k = end + 1;
  }
  return out;
}

/** JS source for an ICU pattern (`native`: modifiers for inline case flags). */
export function translate(pattern: string, caseInsensitive: boolean, native: boolean): { source: string; modifiers: boolean } | null {
  const t = new Translator([...pattern], native, caseInsensitive);
  const source = t.run();
  return source == null ? null : { source, modifiers: t.modifiers };
}
