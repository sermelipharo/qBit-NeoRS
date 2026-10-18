#!/usr/bin/env python3
"""Fill an Xcode String Catalog with qBittorrent's existing translations.

The SwiftUI app uses qBittorrent's English UI strings, so most keys of
Localizable.xcstrings have an exact counterpart in qBittorrent's Qt
translation files (src/lang/qbittorrent_<lang>.ts). For every catalog key the
script looks up the qBittorrent source string (after normalising mnemonics,
ellipses and format specifiers) and copies each language's translation,
converting Qt's %1..%n placeholders to positional Swift specifiers.

Existing translations in the catalog are never overwritten unless --force.

Usage:
  scripts/i18n/import_qbt_translations.py \
      --catalog apps/macos/QBTNeo/Resources/Localizable.xcstrings \
      --ts-dir reference/qBittorrent/src/lang [--force] [--report]
"""

import argparse
import glob
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

SPEC = re.compile(r"%(?:(\d+)\$)?(@|lld|ld|d|lf|f|llu|lu|u|%)")
QT_ARG = re.compile(r"%(\d+)")


def lang_id(code: str) -> str:
    special = {
        "zh_CN": "zh-Hans",
        "zh_TW": "zh-Hant",
        "zh_HK": "zh-HK",
        "sr@latin": "sr-Latn",
        "az@latin": "az-Latn",
        "uz@Latn": "uz-Latn",
    }
    return special.get(code, code.replace("_", "-"))


def normalize(text: str) -> str:
    """Strip Qt mnemonics (&File -> File, keep &&) and unify ellipses/space."""
    text = text.replace("&&", "\0")
    text = re.sub(r"&(?=\S)", "", text)
    text = text.replace("\0", "&")
    text = text.replace("…", "...")
    return " ".join(text.split())


def strip_mnemonic(text: str) -> str:
    text = text.replace("&&", "\0")
    text = re.sub(r"&(?=\S)", "", text)
    return text.replace("\0", "&")


def load_ts(path: str) -> dict:
    """source(normalized) -> translation, first non-empty finished one wins."""
    out = {}
    tree = ET.parse(path)  # Qt .ts files: trusted reference sources, no DTD entities.
    for msg in tree.iter("message"):
        if msg.get("numerus") == "yes":
            continue
        src = msg.findtext("source")
        tr = msg.find("translation")
        if src is None or tr is None:
            continue
        if tr.get("type") in ("unfinished", "obsolete", "vanished"):
            continue
        value = (tr.text or "").strip()
        if not value:
            continue
        key = normalize(src)
        out.setdefault(key, strip_mnemonic(value))
    return out


def catalog_key_to_qt(key: str):
    """Returns (qt_source_normalized, [swift specifier per argument]) or None."""
    specs = []
    pos = 0

    def repl(m):
        nonlocal pos
        if m.group(2) == "%":
            return "%"
        pos += 1
        index = int(m.group(1)) if m.group(1) else pos
        specs.append((index, m.group(2)))
        return f"%{index}"

    qt = SPEC.sub(repl, key)
    types = {i: t for i, t in specs}
    return normalize(qt), types


SUFFIXES = ("...", "…", ":", ".")
CJK_SUFFIX = {":": "：", ".": "。"}


def split_suffix(text: str):
    """Separates one trailing ellipsis, colon or period from a string."""
    for suffix in SUFFIXES:
        if text.endswith(suffix) and len(text) > len(suffix):
            return text[: -len(suffix)].rstrip(), suffix
    return text, ""


def loose_key(text: str) -> str:
    """Case- and trailing-punctuation-insensitive form used for fallback matches."""
    base, _ = split_suffix(text)
    return " ".join(base.lower().split())


def loose_index(table: dict) -> dict:
    out = {}
    for source, value in table.items():
        key = loose_key(source)
        # Single words are too ambiguous for fallback matching ("Engine" is a
        # search engine in qBittorrent but a BitTorrent engine elsewhere).
        if len(key.split()) >= 2:
            out.setdefault(key, value)
    return out


def loose_translation(translation: str, qt_key: str):
    """Adapts a fallback translation to the catalog key's capitalisation and
    trailing punctuation. Returns None when the placeholders differ."""
    base, _ = split_suffix(translation)
    key_base, key_suffix = split_suffix(qt_key)
    if sorted(set(QT_ARG.findall(base))) != sorted(set(QT_ARG.findall(key_base))):
        return None
    if key_base[:1].isupper() and base[:1].islower():
        base = base[:1].upper() + base[1:]
    if key_suffix in CJK_SUFFIX and base and ord(base[-1]) > 0x2E80:
        key_suffix = CJK_SUFFIX[key_suffix]
    return base + key_suffix


def qt_to_swift(translation: str, types: dict) -> str:
    def repl(m):
        i = int(m.group(1))
        t = types.get(i, "@")
        return f"%{i}${t}" if len(types) > 1 else f"%{t}"

    out = translation.replace("%%", "\0")
    out = QT_ARG.sub(repl, out)
    return out.replace("\0", "%%")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalog", required=True)
    ap.add_argument("--ts-dir", required=True)
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--report", action="store_true")
    args = ap.parse_args()

    with open(args.catalog, encoding="utf-8") as f:
        catalog = json.load(f)
    strings = catalog.setdefault("strings", {})

    langs = {}
    for path in sorted(glob.glob(os.path.join(args.ts_dir, "qbittorrent_*.ts"))):
        code = os.path.basename(path)[len("qbittorrent_"):-3]
        if code in ("en",):
            continue
        langs[lang_id(code)] = load_ts(path)

    loose = {lang: loose_index(table) for lang, table in langs.items()}

    filled = 0
    exact_keys = []
    loose_keys = []
    missing = []
    for key, entry in strings.items():
        if entry.get("shouldTranslate") is False:
            continue
        qt_key, types = catalog_key_to_qt(key)
        locs = entry.setdefault("localizations", {})
        match = None
        for lang, table in langs.items():
            how = "exact"
            tr = table.get(qt_key)
            if tr is None:
                candidate = loose[lang].get(loose_key(qt_key))
                tr = loose_translation(candidate, qt_key) if candidate is not None else None
                how = "loose"
            if tr is None:
                continue
            if how == "exact" or match is None:
                match = how
            if lang in locs and not args.force:
                continue
            locs[lang] = {"stringUnit": {"state": "translated", "value": qt_to_swift(tr, types)}}
            filled += 1
        if match == "exact":
            exact_keys.append(key)
        elif match == "loose":
            loose_keys.append(key)
        else:
            missing.append(key)
        if not locs:
            del entry["localizations"]

    with open(args.catalog, "w", encoding="utf-8") as f:
        json.dump(catalog, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")

    total = len(strings)
    print(f"{total} keys, {len(langs)} languages, {filled} translations filled")
    print(f"matched exactly: {len(exact_keys)}, matched by normalisation: {len(loose_keys)}, "
          f"no qBittorrent match: {len(missing)} "
          f"(match rate {100 * (len(exact_keys) + len(loose_keys)) / max(total, 1):.1f}%)")
    if args.report:
        for k in loose_keys:
            print("  normalised:", k)
        for k in missing:
            print("  missing:", k)
    return 0


if __name__ == "__main__":
    sys.exit(main())
