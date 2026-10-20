// The TypeScript name parser against the macOS app's own results
// (names-fixture.json, written by the app's NameParserWebFixtureTests).
// Run with scripts/test-webui.sh.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { kindForCategory } from "../src/contentkind.ts";
import { compileIcu, translate } from "../src/icuregex.ts";
import { NameParseCache, NameParser } from "../src/nameparser.ts";

const fixture = JSON.parse(readFileSync(process.env.FIXTURE, "utf8"));

/** Results that differ from the app's, as readable lines. */
function differences(set, spellOutCase) {
  const parsers = new Map();
  const parser = (yearAsChip) => {
    if (!parsers.has(yearAsChip)) parsers.set(yearAsChip, new NameParser(set.rules, { yearAsChip }, spellOutCase));
    return parsers.get(yearAsChip);
  };
  const out = [];
  for (const want of set.results) {
    const p = parser(want.yearAsChip).parse(want.name);
    const got = { title: p.title, titleRange: [p.titleStart, p.titleEnd], chips: p.chips.map((c) => [c.category, c.text]), kind: p.kind };
    const expected = { title: want.title, titleRange: want.titleRange, chips: want.chips, kind: want.kind };
    if (JSON.stringify(got) !== JSON.stringify(expected)) {
      out.push(`${JSON.stringify(want.name)} (year as tag: ${want.yearAsChip})\n  app: ${JSON.stringify(expected)}\n  web: ${JSON.stringify(got)}`);
    }
  }
  return out;
}

for (const [mode, spell] of [["regex modifiers", false], ["spelled-out case", true]]) {
  test(`built-in rules, ${mode}: every rule runs in JavaScript`, () => {
    const parser = new NameParser(fixture.builtIn.rules, { yearAsChip: true }, spell);
    assert.equal(parser.rules.length, fixture.builtIn.rules.length);
  });

  test(`built-in rules, ${mode}: identical results`, () => {
    const diff = differences(fixture.builtIn, spell);
    console.log(`# ${mode}: ${fixture.builtIn.results.length - diff.length} of ${fixture.builtIn.results.length} built-in results identical`);
    assert.deepEqual(diff, []);
  });

  test(`custom rules, ${mode}: identical results`, () => {
    const diff = differences(fixture.custom, spell);
    console.log(`# ${mode}: ${fixture.custom.results.length - diff.length} of ${fixture.custom.results.length} custom results identical`);
    assert.deepEqual(diff, []);
  });
}

test("category names", () => {
  for (const [name, kind] of fixture.categoryKinds) assert.equal(kindForCategory(name) ?? "", kind, name);
});

test("ICU-only syntax is refused", () => {
  for (const p of ["(?>ab)c", "a*+b", "a++", "a?+", "a{2}+", String.raw`\Gx`, String.raw`\X`, "[a[b]]", "[a-z&&[^b]]", "[:alpha:]+",
    "(?x) a", "(?s:.)", String.raw`\p{InCyrillic}`, String.raw`[\W]`, String.raw`\N{LATIN SMALL LETTER A}`, "(unclosed", "a)", "x{", "a}"]) {
    assert.equal(translate(p, true, true), null, p);
    assert.equal(compileIcu(p, true), null, p);
  }
});

test("ICU syntax JavaScript lacks is translated", () => {
  const cases = [
    [String.raw`\w+`, "Привет_1", "Привет_1"],
    [String.raw`\bМир\b`, "Мир, мир", "Мир"],
    [String.raw`a\-b\ c`, "a-b c", "a-b c"],
    [String.raw`\QC++\E`, "C++", "C++"],
    [String.raw`\x{263A}`, "☺", "☺"],
    [String.raw`\p{Cyrillic}+`, "abcЖЖ", "ЖЖ"],
    [String.raw`\p{script=cyrillic}+`, "ЖЖ", "ЖЖ"],
    [String.raw`\d+`, "٣٤", "٣٤"],
  ];
  for (const [pattern, text, match] of cases) {
    for (const spell of [false, true]) {
      const c = compileIcu(pattern, true, spell);
      assert.ok(c, pattern);
      assert.equal(text.match(new RegExp(c.re.source, c.re.flags.replace("g", "")))?.[0], match, `${pattern} (${spell})`);
    }
  }
  // Inline flags: case-sensitive part inside a case-insensitive expression, and (?i) up to the group's end.
  for (const spell of [false, true]) {
    const c = compileIcu("(?-i:XXX)|abc", true, spell).re;
    assert.deepEqual(["xxx", "XXX", "ABC"].map((s) => new RegExp(c.source, c.flags.replace("g", "")).test(s)), [false, true, true], String(spell));
    const d = compileIcu("(x(?i)a|b)", false, spell).re;
    assert.deepEqual(["xA", "B", "X"].map((s) => new RegExp(d.source, d.flags.replace("g", "")).test(s)), [true, true, false], String(spell));
  }
});

test("cache is bounded", () => {
  const cache = new NameParseCache(new NameParser(fixture.builtIn.rules), 3);
  for (const n of ["a.1080p", "b.720p", "c.2160p", "d.480p"]) cache.parsed(n);
  assert.ok(cache.size <= 3);
  assert.equal(cache.parsed("d.480p").chips[0].text, "480p");
});

test("long names are cut and parse quickly", () => {
  const parser = new NameParser(fixture.builtIn.rules);
  const t0 = performance.now();
  for (const n of ["a".repeat(100_000), "1.".repeat(50_000), "[a]".repeat(30_000) + " 1080p"]) parser.parse(n);
  assert.ok(performance.now() - t0 < 2000);
});
