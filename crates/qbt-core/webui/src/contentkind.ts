// What a torrent most likely contains, guessed from its name and the tags
// the name rules found: a port of the macOS app's `ContentKindDetector`
// (fixed markers, checked in order; the first that applies wins), plus the
// file extension and category name fallbacks. Pure: no DOM.

import { compileIcu } from "./icuregex";

export type ContentKind = "video" | "music" | "software" | "game" | "rom" | "audiobook" | "book" | "comic" | "adult" | "other";

export const KIND_TITLES: Record<ContentKind, string> = {
  video: "Video", music: "Music", software: "Software", game: "Game", rom: "ROM Collection",
  audiobook: "Audiobook", book: "Book", comic: "Comic", adult: "Adult", other: "Other",
};

/** One tag of a name (category from `NAME_CATEGORIES`). */
export interface Chip {
  category: string;
  text: string;
}

/** Longer names are only searched in their first part (UTF-16 units). */
export const MAX_INPUT_LENGTH = 512;

const WORD_UNIT = /[\p{L}\p{M}\p{N}]/u;

/** Letters, marks and digits; surrogate halves count as letters. */
function isWordUnit(code: number): boolean {
  return (code >= 0xd800 && code <= 0xdfff) || WORD_UNIT.test(String.fromCharCode(code));
}

/** Letters and digits never touch a match (`start`…`end` in UTF-16 units of `text`). */
export function isWholeToken(start: number, end: number, text: string): boolean {
  if (start > 0 && isWordUnit(text.charCodeAt(start - 1)) && isWordUnit(text.charCodeAt(start))) return false;
  if (end < text.length && isWordUnit(text.charCodeAt(end)) && isWordUnit(text.charCodeAt(end - 1))) return false;
  return true;
}

/** `NSString.pathExtension`: after the last dot of the last path component, without spaces. */
export function pathExtension(name: string): string {
  const component = name.replace(/\/+$/, "").split("/").pop() ?? "";
  const dot = component.lastIndexOf(".");
  if (dot <= 0) return "";
  const ext = component.slice(dot + 1);
  return ext.includes(" ") ? "" : ext;
}

const EXTENSIONS: [ContentKind, string[]][] = [
  ["video", ["mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "webm", "mpg", "vob"]],
  ["music", ["flac", "mp3", "m4a", "ogg", "opus", "wav", "ape", "aiff", "dsf"]],
  ["audiobook", ["m4b"]],
  ["book", ["epub", "fb2", "mobi", "azw3", "djvu"]],
  ["comic", ["cbz", "cbr", "cb7"]],
  ["game", ["nsp", "xci", "nsz", "pkg", "cia"]],
  ["rom", ["chd", "rvz", "wbfs"]],
  ["software", ["iso", "dmg", "exe", "msi", "img", "apk", "appimage"]],
];

/** The kind a file extension stands for, if it is a telling one. */
export function kindForExtension(ext: string): ContentKind | null {
  const e = ext.toLowerCase();
  return EXTENSIONS.find(([, list]) => list.includes(e))?.[0] ?? null;
}

/** In order: "audiobooks" is checked before "books". "=key" matches only the whole word. */
const CATEGORY_WORDS: [ContentKind, string[]][] = [
  ["adult", ["adult", "=xxx"]],
  ["audiobook", ["audiobook", "аудиокниг"]],
  ["comic", ["comic", "manga", "комикс", "манга"]],
  ["rom", ["=rom", "=roms", "emulat", "эмулятор"]],
  ["game", ["game", "игр"]],
  ["book", ["book", "ebook", "книг", "литератур"]],
  ["music", ["music", "audio", "album", "podcast", "музык", "подкаст"]],
  ["video", ["movie", "film", "video", "series", "=tv", "show", "anime", "documentar", "cartoon", "course",
    "фильм", "кино", "сериал", "аниме", "мульт", "видео", "документ", "курс"]],
  ["software", ["software", "=app", "=apps", "application", "program", "linux", "distro", "=iso", "=isos", "программ", "софт"]],
];

/** A kind for a category name ("Movies", "Музыка"); matches the start of any word. */
export function kindForCategory(category: string): ContentKind | null {
  const words = category.toLowerCase().split(/[^\p{Alphabetic}\p{M}]+/u).filter(Boolean);
  for (const [kind, keys] of CATEGORY_WORDS) {
    if (words.some((w) => keys.some((k) => (k.startsWith("=") ? w === k.slice(1) : w.startsWith(k))))) return kind;
  }
  return null;
}

const PATTERNS = {
  adult: String.raw`(?-i:XXX)|^[A-Za-z][A-Za-z0-9]{1,30}[ ._]\d{2}[ ._](?:0[1-9]|1[0-2])[ ._](?:0[1-9]|[12]\d|3[01])[ ._]`,
  rom: String.raw`No-?Intro|Redump|TOSEC|MAME|FBNeo|FinalBurn|RetroArch|(?-i:ROMs?|CHD)|ROM[ ._]?sets?|\[!\]|\((?:USA|Europe|Japan|World)(?:, ?[A-Z][a-z]+)*\)|\(Rev[ ._]?[0-9A-Z]{1,3}\)`,
  audiobook: String.raw`M4B|Audio[ ._-]?books?|Аудиокниг[аи]|Аудиоспектакль|читает|Narrated[ ._]by|Unabridged|Abridged`,
  comic: String.raw`CB[ZR7]|Manga|Манга|Manhwa|Манхва|Comics?|Комиксы?|Webtoon|Graphic[ ._]Novel|Scanlations?|(?-i:c)\d{3,4}`,
  currentGame: String.raw`FitGirl|DODI|xatab|ElAmigos|(?-i:CODEX|PLAZA|RUNE|TENOKE|SKIDROW|EMPRESS|DARKSiDERS|GOG|NSP|XCI|NSZ|PKG|NSW)|[\[(]Switch[\])]|Nintendo[ ._]Switch|Steam-?Rip|PS[45]|Xbox[ ._]?(?:One|Series)|Механики`,
  game: String.raw`PS[23]|PSP|PS[ ._]?Vita|Xbox|DLCs?|MULTi\d{1,2}|GOTY`,
  videoWords: String.raw`Udemy|Coursera|Pluralsight|Skillshare|Domestika|MasterClass|Skillbox|GeekBrains|Нетология|(?:Video[ ._])?Course|Курс|Lectures|Tutorials?|Видеоурок\w*|Documentary|DOCU`,
  book: String.raw`EPUB|FB2|MOBI|AZW3?|DjVu|e-?Books?|Книг[аи]|Библиотека|Сборник[ ._]книг|Собрание[ ._]сочинений|Complete[ ._]Works`,
  music: String.raw`Discography|Дискография|Album|Альбом|Soundtrack|(?-i:OST|EP|Single)|Сингл|Podcast|Подкаст|kbps|FLAC|ALAC|MP3|V0|Lossless|Hi-?Res|Vinyl`,
  software: String.raw`Linux|BSD|Ubuntu|Debian|Fedora|Windows|macOS|Portable|x64|x86(?:_64)?|amd64|arm64|aarch64|Setup|Installer|Multilingual|Pre-?Activated`,
};

const VIDEO_CATEGORIES = new Set(["resolution", "hdr", "videoCodec"]);
/** Sources that music, comics and disc images share with video. */
const SHARED_SOURCES = new Set(["web", "vinyl", "cd", "digital", "dvd"]);

/** Whether `re` has a whole-token match in `text`. */
function matches(re: RegExp | undefined, text: string): boolean {
  if (!re) return false;
  for (const m of text.matchAll(re)) {
    if (m[0].length > 0 && isWholeToken(m.index!, m.index! + m[0].length, text)) return true;
  }
  return false;
}

export class ContentKindDetector {
  private readonly re: Partial<Record<keyof typeof PATTERNS, RegExp>> = {};

  /** `spellOutCase`: compile without JS regex modifiers (tests both paths). */
  constructor(spellOutCase = false) {
    for (const k of Object.keys(PATTERNS) as (keyof typeof PATTERNS)[]) {
      const c = compileIcu(PATTERNS[k], true, spellOutCase);
      if (c) this.re[k] = c.re;
    }
  }

  kind(name: string, chips: Chip[]): ContentKind {
    const text = name.length > MAX_INPUT_LENGTH ? name.slice(0, MAX_INPUT_LENGTH) : name;
    const r = this.re;
    if (matches(r.adult, text)) return "adult";
    if (matches(r.currentGame, text)) return "game";
    if (matches(r.rom, text)) return "rom";
    if (matches(r.audiobook, text)) return "audiobook";
    if (matches(r.comic, text) || chips.some((c) => c.category === "format" && (c.text === "CBZ" || c.text === "CBR"))) return "comic";
    const hasAudio = chips.some((c) => c.category === "audio");
    const videoTag = (c: Chip) => VIDEO_CATEGORIES.has(c.category) || (c.category === "source" && !SHARED_SOURCES.has(c.text.toLowerCase()));
    if (chips.some(videoTag) || (!hasAudio && chips.some((c) => c.category === "seasonEpisode")) || matches(r.videoWords, text)) return "video";
    if (matches(r.game, text)) return "game";
    const hasSoftware = chips.some((c) => c.category === "platform" || c.category === "version");
    if (matches(r.book, text) || (!hasSoftware && chips.some((c) => c.category === "format" && c.text === "PDF"))) return "book";
    if (hasAudio || matches(r.music, text)) return "music";
    if (hasSoftware || matches(r.software, text)) return "software";
    return kindForExtension(pathExtension(text)) ?? "other";
  }
}
