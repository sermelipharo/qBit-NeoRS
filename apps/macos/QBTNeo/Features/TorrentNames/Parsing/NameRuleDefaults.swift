// Rule patterns are data; wrapping them would make them harder to read.
// swiftlint:disable line_length
import Foundation

/// The built-in rules of the torrent name parser, in matching order: clean-up
/// first, then resolution, dates and years, episodes, video, audio, subtitles
/// before audio languages, editions, platforms, other flags and release groups
/// last (earlier rules claim their text first). Covers common scene, P2P,
/// anime and Russian release naming with dots, underscores, spaces or
/// brackets as separators.
enum NameRuleDefaults {
    static let idPrefix = "default."

    static let rules: [NameRule] = [
        NameRule(id: idPrefix + "fileExtension", category: .hidden, pattern: #"\.(?:mkv|mp4|m4v|avi|mov|wmv|ts|m2ts|webm|iso|dmg|zip|rar|7z|torrent|flac|mp3|m4a|nsp|xci|nsz|chd|cia|pkg|exe|msi)$"#),
        NameRule(id: idPrefix + "checksum", category: .hidden, pattern: #"\[[0-9A-F]{8}\]"#),
        NameRule(id: idPrefix + "sitePrefix", category: .hidden, pattern: #"^www\.[\w-]+\.[a-z]{2,4}\s*[-–]\s*"#),
        NameRule(id: idPrefix + "siteTag", category: .hidden, pattern: #"\[(?:www\.)?[\w-]+\.(?:com|org|net|me|to|cc|ru|tv|info)\]"#),
        NameRule(id: idPrefix + "comicScanner", category: .releaseGroup, pattern: #"\(([\p{L}\p{N}]{2,20}-(?:Empire|DCP|Novus))\)(?=\.[A-Za-z0-9]{2,4}$|$)"#, template: "$1"),
        NameRule(id: idPrefix + "courseSite", category: .releaseGroup, pattern: #"^(Udemy|Coursera|LinkedIn[ ._]Learning|Lynda|Pluralsight|Skillshare|Domestika|MasterClass|edX|Stepik|Skillbox|GeekBrains|Нетология)[ ._]*[-–:|][ ._]*"#, template: "$1"),
        NameRule(id: idPrefix + "studioBeforeDate", category: .releaseGroup, pattern: #"^[A-Za-z][A-Za-z0-9]{1,30}(?=[ ._]\d{2}[ ._](?:0[1-9]|1[0-2])[ ._](?:0[1-9]|[12]\d|3[01])[ ._])"#),
        NameRule(id: idPrefix + "shortDate", category: .other, pattern: #"(?<=^[^ ._]{2,31}[ ._])(\d{2})[ ._](0[1-9]|1[0-2])[ ._](0[1-9]|[12]\d|3[01])(?=[ ._])"#, template: "20$1.$2.$3"),
        NameRule(id: idPrefix + "res8k", category: .resolution, pattern: #"4320p|8K"#, template: "8K"),
        NameRule(id: idPrefix + "res4k", category: .resolution, pattern: #"2160p|3840x2160|4K|UHD"#, template: "4K"),
        NameRule(id: idPrefix + "res1080", category: .resolution, pattern: #"1080[pi]|1920x1080|FHD|Full[ ._-]?HD"#, template: "1080p"),
        NameRule(id: idPrefix + "res720", category: .resolution, pattern: #"720p|1280x720"#, template: "720p"),
        NameRule(id: idPrefix + "res576", category: .resolution, pattern: #"576[pi]"#, template: "576p"),
        NameRule(id: idPrefix + "res480", category: .resolution, pattern: #"480[pi]"#, template: "480p"),
        NameRule(id: idPrefix + "date", category: .other, pattern: #"(?:19|20)\d{2}[.-](?:0[1-9]|1[0-2])[.-](?:0[1-9]|[12]\d|3[01])"#),
        NameRule(id: idPrefix + "yearRange", category: .year, pattern: #"(?<=[\s.\[(_,-])(?:19\d{2}|20[0-3]\d)\s?[-–]\s?(?:19\d{2}|20[0-3]\d)(?=[\s.\])_,-]|$)"#),
        NameRule(id: idPrefix + "year", category: .year, pattern: #"(?<=[\s.\[(_,-])(?:19\d{2}|20[0-3]\d)(?=[\s.\])_,-]|$)"#),
        NameRule(id: idPrefix + "episodeRange", category: .seasonEpisode, pattern: #"S(\d{1,2})[ ._]?E(\d{1,4})[ ._]?-?[ ._]?E(\d{1,4})"#, template: "S$1E$2–E$3"),
        NameRule(id: idPrefix + "seasonEpisode", category: .seasonEpisode, pattern: #"S(\d{1,2})[ ._]?E(\d{1,4})"#, template: "S$1E$2"),
        NameRule(id: idPrefix + "crossEpisode", category: .seasonEpisode, pattern: #"(\d{1,2})x(\d{2,3})"#, template: "S$1E$2"),
        NameRule(id: idPrefix + "seasonRange", category: .seasonEpisode, pattern: #"S(\d{1,2})[ ._]?[-–][ ._]?S(\d{1,2})"#, template: "S$1–S$2"),
        NameRule(id: idPrefix + "season", category: .seasonEpisode, pattern: #"S(\d{1,2})"#, template: "S$1"),
        NameRule(id: idPrefix + "seasonWordRange", category: .seasonEpisode, pattern: #"Season[ ._]?(\d{1,2})[ ._]?[-–][ ._]?(\d{1,2})"#, template: "S$1–S$2"),
        NameRule(id: idPrefix + "seasonWord", category: .seasonEpisode, pattern: #"Season[ ._]?(\d{1,2})"#, template: "S$1"),
        NameRule(id: idPrefix + "seasonRussian", category: .seasonEpisode, pattern: #"Сезон[ ._:]*(\d{1,2})"#, template: "S$1"),
        NameRule(id: idPrefix + "seasonRussianSuffix", category: .seasonEpisode, pattern: #"(\d{1,2})[ ._-]?(?:й[ ._])?сезон"#, template: "S$1"),
        NameRule(id: idPrefix + "episodesOfRussian", category: .seasonEpisode, pattern: #"(?:Серии|Серия|Эпизоды)?:?\s*(\d{1,4})\s?[-–]\s?(\d{1,4})\s+из\s+(\d{1,4})"#, template: "E$1–$2/$3"),
        NameRule(id: idPrefix + "episodeOfRussian", category: .seasonEpisode, pattern: #"(?:Серия|Эпизод)?:?\s*(\d{1,4})\s+из\s+(\d{1,4})"#, template: "E$1/$2"),
        NameRule(id: idPrefix + "episodesRussian", category: .seasonEpisode, pattern: #"(?:Серии|Эпизоды):?\s*(\d{1,4})\s?[-–]\s?(\d{1,4})"#, template: "E$1–$2"),
        NameRule(id: idPrefix + "episodeBatch", category: .seasonEpisode, pattern: #"\[(?!(?:16|24)-(?:44|48|88|96|176|192)\])(\d{1,4})\s?[-~]\s?(\d{1,4})\]"#, template: "E$1–$2"),
        NameRule(id: idPrefix + "episodeWord", category: .seasonEpisode, pattern: #"(?:Episode|Ep\.?)[ ._]?(\d{1,4})"#, template: "E$1"),
        NameRule(id: idPrefix + "episodeDash", category: .seasonEpisode, pattern: #"(?<=\s)[-–][ ._](\d{2,4})(?:v\d)?(?=[ ._]*(?:[\[(]|$))"#, template: "E$1"),
        NameRule(id: idPrefix + "complete", category: .seasonEpisode, pattern: #"COMPLETE|Complete[ ._](?:Series|Season)|Полный[ ._]сезон|Все[ ._]серии"#, template: "Complete", isCaseSensitive: true),
        NameRule(id: idPrefix + "volumeRange", category: .volume, pattern: #"(?:Vol(?:ume)?\.?[ ._]?|Том[ ._]?|(?-i:v)(?=\d{2}))(\d{1,3})[ ._]?[-–~][ ._]?(?:Vol\.?[ ._]?|(?-i:v))?(\d{1,3})(?!\.\d)"#, template: "Vol. $1–$2"),
        NameRule(id: idPrefix + "volume", category: .volume, pattern: #"(?:Vol(?:ume)?\.?[ ._]?|Том[ ._]?|(?-i:v)(?=\d{2}))(\d{1,3})(?!\.\d)"#, template: "Vol. $1"),
        NameRule(id: idPrefix + "chapterRange", category: .volume, pattern: #"(?:Ch(?:apter)?\.?[ ._]?|Главы[ ._]?|(?-i:c)(?=\d{3}))(\d{1,4})[ ._]?[-–~][ ._]?(?:Ch\.?[ ._]?|(?-i:c))?(\d{1,4})"#, template: "Ch. $1–$2"),
        NameRule(id: idPrefix + "chapter", category: .volume, pattern: #"(?:Ch(?:apter)?\.?[ ._]?|Глава[ ._]?|(?-i:c)(?=\d{3}))(\d{1,4})"#, template: "Ch. $1"),
        NameRule(id: idPrefix + "issue", category: .volume, pattern: #"#(\d{1,4})|(?<= )(0\d{2})(?= \()"#, template: "#$1$2"),
        NameRule(id: idPrefix + "disc", category: .volume, pattern: #"(?:Disc|Disk|CD)[ ._]?(\d{1,2})"#, template: "Disc $1"),
        NameRule(id: idPrefix + "dolbyVision", category: .hdr, pattern: #"DoVi|DV|Dolby[ ._]?Vision"#, template: "Dolby Vision"),
        NameRule(id: idPrefix + "hdr10Plus", category: .hdr, pattern: #"HDR10(?:\+|Plus)"#, template: "HDR10+"),
        NameRule(id: idPrefix + "hdr", category: .hdr, pattern: #"HDR10|HDR"#, template: "HDR"),
        NameRule(id: idPrefix + "hlg", category: .hdr, pattern: #"HLG"#, template: "HLG"),
        NameRule(id: idPrefix + "remux", category: .source, pattern: #"(?:UHD[ ._-]?)?(?:BD|Blu-?Ray)?[ ._-]?Remux"#, template: "Remux"),
        NameRule(id: idPrefix + "webDLRip", category: .source, pattern: #"WEB-?DL-?Rip"#, template: "WEB-DLRip"),
        NameRule(id: idPrefix + "webDL", category: .source, pattern: #"WEB-?DL"#, template: "WEB-DL"),
        NameRule(id: idPrefix + "webRip", category: .source, pattern: #"WEB-?Rip"#, template: "WEBRip"),
        NameRule(id: idPrefix + "web", category: .source, pattern: #"WEB"#, template: "WEB", isCaseSensitive: true),
        NameRule(id: idPrefix + "bdRip", category: .source, pattern: #"BD-?Rip|BR-?Rip"#, template: "BDRip"),
        NameRule(id: idPrefix + "bluRay", category: .source, pattern: #"Blu-?Ray|BD(?:25|50|66|100)?"#, template: "BluRay"),
        NameRule(id: idPrefix + "hdtv", category: .source, pattern: #"HDTV(?:-?Rip)?"#, template: "HDTV"),
        NameRule(id: idPrefix + "hdRip", category: .source, pattern: #"HD-?Rip"#, template: "HDRip"),
        NameRule(id: idPrefix + "dvdRip", category: .source, pattern: #"DVD-?Rip"#, template: "DVDRip"),
        NameRule(id: idPrefix + "dvd", category: .source, pattern: #"DVD(?:5|9|R)?"#, template: "DVD"),
        NameRule(id: idPrefix + "cam", category: .source, pattern: #"CAM-?Rip|HD-?CAM|CAM|TeleSync|HDTS|TELESYNC"#, template: "CAM", isCaseSensitive: true),
        NameRule(id: idPrefix + "satRip", category: .source, pattern: #"SAT-?Rip|DVB"#, template: "SATRip"),
        NameRule(id: idPrefix + "amazon", category: .source, pattern: #"AMZN"#, template: "Amazon"),
        NameRule(id: idPrefix + "netflix", category: .source, pattern: #"NF|Netflix"#, template: "Netflix"),
        NameRule(id: idPrefix + "disney", category: .source, pattern: #"DSNP|DSNY"#, template: "Disney+"),
        NameRule(id: idPrefix + "appleTV", category: .source, pattern: #"ATVP|iTunes"#, template: "Apple TV"),
        NameRule(id: idPrefix + "max", category: .source, pattern: #"HMAX"#, template: "Max"),
        NameRule(id: idPrefix + "vinyl", category: .source, pattern: #"Vinyl(?:[ ._-]?Rip)?|LP[ ._-]?Rip"#, template: "Vinyl"),
        NameRule(id: idPrefix + "cd", category: .source, pattern: #"CD(?:[ ._-]?Rip|DA)?"#, template: "CD"),
        NameRule(id: idPrefix + "digital", category: .source, pattern: #"\(Digital\)|\[Digital\]"#, template: "Digital"),
        NameRule(id: idPrefix + "hevc", category: .videoCodec, pattern: #"[xh]\.?265|HEVC"#, template: "HEVC"),
        NameRule(id: idPrefix + "avc", category: .videoCodec, pattern: #"[xh]\.?264|AVC"#, template: "H.264"),
        NameRule(id: idPrefix + "av1", category: .videoCodec, pattern: #"AV1"#, template: "AV1"),
        NameRule(id: idPrefix + "vp9", category: .videoCodec, pattern: #"VP9"#, template: "VP9"),
        NameRule(id: idPrefix + "xvid", category: .videoCodec, pattern: #"XviD|DivX"#),
        NameRule(id: idPrefix + "tenBit", category: .videoCodec, pattern: #"10[ ._-]?bit|Hi10P?"#, template: "10-bit"),
        NameRule(id: idPrefix + "atmos", category: .audio, pattern: #"Atmos"#, template: "Atmos"),
        NameRule(id: idPrefix + "trueHD", category: .audio, pattern: #"TrueHD(?:[ ._]?(\d\.\d))?"#, template: "TrueHD $1"),
        NameRule(id: idPrefix + "dtsX", category: .audio, pattern: #"DTS[ ._:-]?X"#, template: "DTS:X"),
        NameRule(id: idPrefix + "dtsHDMA", category: .audio, pattern: #"DTS[ ._-]?HD[ ._-]?MA(?:[ ._]?(\d\.\d))?"#, template: "DTS-HD MA $1"),
        NameRule(id: idPrefix + "dtsHD", category: .audio, pattern: #"DTS[ ._-]?HD(?:[ ._]?(\d\.\d))?"#, template: "DTS-HD $1"),
        NameRule(id: idPrefix + "dts", category: .audio, pattern: #"DTS(?:[ ._]?(\d\.\d))?"#, template: "DTS $1"),
        NameRule(id: idPrefix + "ddPlus", category: .audio, pattern: #"(?:DDP|DD\+|E-?AC-?3)(?:[ ._]?(\d\.\d))?"#, template: "DD+ $1"),
        NameRule(id: idPrefix + "dd", category: .audio, pattern: #"(?:DD|AC-?3|Dolby[ ._]Digital)(?:[ ._]?(\d\.\d))?"#, template: "DD $1"),
        NameRule(id: idPrefix + "aac", category: .audio, pattern: #"AAC(?:[ ._]?(\d\.\d))?"#, template: "AAC $1"),
        NameRule(id: idPrefix + "flac", category: .audio, pattern: #"FLAC"#, template: "FLAC"),
        NameRule(id: idPrefix + "hiResFormat", category: .audio, pattern: #"(16|24)(?:[ ._-]?bit)?[ ._/-](44|48|88|96|176|192)(?:[.,]\d)?(?:[ ._-]?kHz)?"#, template: "$1/$2"),
        NameRule(id: idPrefix + "bitDepth", category: .audio, pattern: #"(16|24)[ ._-]?bit"#, template: "$1-bit"),
        NameRule(id: idPrefix + "sampleRate", category: .audio, pattern: #"(\d{2,3}(?:[.,]\d)?)[ ._-]?kHz"#, template: "$1 kHz"),
        NameRule(id: idPrefix + "bitrateBracket", category: .audio, pattern: #"\[(128|192|256|320)\]"#, template: "$1 kbps"),
        NameRule(id: idPrefix + "mp3", category: .audio, pattern: #"MP3(?:[ ._-]?(\d{3})(?:[ ._-]?kbps)?)?"#, template: "MP3 $1"),
        NameRule(id: idPrefix + "bitrate", category: .audio, pattern: #"(\d{2,4})[ ._-]?kbps"#, template: "$1 kbps"),
        NameRule(id: idPrefix + "otherAudio", category: .audio, pattern: #"ALAC|Opus|Vorbis|WAV|APE|DSD(?:64|128|256)?|Hi-?Res|Lossless"#),
        NameRule(id: idPrefix + "channels", category: .audio, pattern: #"[57]\.1|2\.0(?=[ ._-]?ch)"#, template: "$0"),
        NameRule(id: idPrefix + "vbr", category: .audio, pattern: #"V0|V2(?=[ ._-]?VBR)|VBR"#),
        NameRule(id: idPrefix + "multiSub", category: .subtitles, pattern: #"Multi(?:ple)?[ ._-]?Sub(?:title)?s?"#, template: "MultiSub"),
        NameRule(id: idPrefix + "engSub", category: .subtitles, pattern: #"(?:ENG|English)[ ._-]?Sub(?:title)?s?|Subs?[ ._-]?ENG"#, template: "ENG Sub"),
        NameRule(id: idPrefix + "rusSub", category: .subtitles, pattern: #"(?:RUS|Russian)[ ._-]?Sub(?:title)?s?|Subs?[ ._-]?RUS|Рус(?:ские|\.)?[ ._]?суб(?:титры)?"#, template: "RUS Sub"),
        NameRule(id: idPrefix + "langSub", category: .subtitles, pattern: #"(ENG|RUS|UKR|JPN|JAP|GER|FRA|FRE|ITA|SPA|ESP|KOR|CHI|CHS|CHT|POR|POL|TUR|ARA)[ ._-]?Sub(?:title)?s?"#, template: "$1 Sub"),
        NameRule(id: idPrefix + "vostfr", category: .subtitles, pattern: #"VOSTFR"#, template: "FRA Sub"),
        NameRule(id: idPrefix + "hardsub", category: .subtitles, pattern: #"Hard-?Subs?|HC"#, template: "Hardsub"),
        NameRule(id: idPrefix + "subs", category: .subtitles, pattern: #"Sub(?:title)?s?|Субтитры|Sub"#, template: "Subs"),
        NameRule(id: idPrefix + "dubRussian", category: .audioLanguage, pattern: #"Дубляж|Дублированный|Дублирование"#),
        NameRule(id: idPrefix + "dub", category: .audioLanguage, pattern: #"Dub(?:bed)?"#, template: "Dub"),
        NameRule(id: idPrefix + "mvo", category: .audioLanguage, pattern: #"MVO|Многоголосый(?:[ ._]закадровый)?"#, template: "MVO"),
        NameRule(id: idPrefix + "dvo", category: .audioLanguage, pattern: #"DVO|Двухголосый(?:[ ._]закадровый)?"#, template: "DVO"),
        NameRule(id: idPrefix + "avo", category: .audioLanguage, pattern: #"AVO|Авторский(?:[ ._]одноголосый)?"#, template: "AVO"),
        NameRule(id: idPrefix + "vo", category: .audioLanguage, pattern: #"VO|Одноголосый(?:[ ._]закадровый)?"#, template: "VO"),
        NameRule(id: idPrefix + "multiLanguageCount", category: .audioLanguage, pattern: #"MULTi[ ._-]?(\d{1,2})"#, template: "Multi $1"),
        NameRule(id: idPrefix + "languageList", category: .audioLanguage, pattern: #"\(((?:En|Fr|De|Es|It|Ja|Nl|Pt|Sv|No|Da|Fi|Zh|Ko|Pl|Ru)(?:,(?:En|Fr|De|Es|It|Ja|Nl|Pt|Sv|No|Da|Fi|Zh|Ko|Pl|Ru))+)\)"#, template: "$1", isCaseSensitive: true),
        NameRule(id: idPrefix + "languagePair", category: .audioLanguage, pattern: #"\[((?:Multi|Rus?|Eng?|Ukr?)(?:[/+,] ?(?:Multi|Rus?|Eng?|Ukr?))+)\]"#, template: "$1"),
        NameRule(id: idPrefix + "dualAudio", category: .audioLanguage, pattern: #"Dual[ ._-]?Audio"#, template: "Dual Audio"),
        NameRule(id: idPrefix + "multiAudio", category: .audioLanguage, pattern: #"Multi(?:lingual|[ ._-]?(?:Audio|Lang))?\d{0,2}"#, template: "Multi"),
        NameRule(id: idPrefix + "russianAudio", category: .audioLanguage, pattern: #"RUS"#, template: "RUS"),
        NameRule(id: idPrefix + "englishAudio", category: .audioLanguage, pattern: #"ENG"#, template: "ENG"),
        NameRule(id: idPrefix + "ukrainianAudio", category: .audioLanguage, pattern: #"UKR"#, template: "UKR"),
        NameRule(id: idPrefix + "japaneseAudio", category: .audioLanguage, pattern: #"JPN|JAP"#, template: "JPN"),
        NameRule(id: idPrefix + "otherAudioLanguage", category: .audioLanguage, pattern: #"GER|FRA|FRE|ITA|SPA|ESP|KOR|CHI|POR|POL|TUR"#),
        NameRule(id: idPrefix + "directorsCut", category: .edition, pattern: #"Director'?s[ ._]Cut|Режисс[её]рская[ ._]версия"#, template: "Director's Cut"),
        NameRule(id: idPrefix + "extended", category: .edition, pattern: #"Extended(?:[ ._](?:Cut|Edition))?|Расширенная[ ._]версия"#, template: "Extended"),
        NameRule(id: idPrefix + "theatrical", category: .edition, pattern: #"Theatrical(?:[ ._]Cut)?|Театральная[ ._]версия"#, template: "Theatrical"),
        NameRule(id: idPrefix + "unrated", category: .edition, pattern: #"Unrated|Uncut"#),
        NameRule(id: idPrefix + "imax", category: .edition, pattern: #"IMAX(?:[ ._]Enhanced)?"#, template: "IMAX"),
        NameRule(id: idPrefix + "remastered", category: .edition, pattern: #"Remastered|Restored"#),
        NameRule(id: idPrefix + "criterion", category: .edition, pattern: #"Criterion(?:[ ._]Collection)?"#, template: "Criterion"),
        NameRule(id: idPrefix + "specialEdition", category: .edition, pattern: #"(?:Special|Anniversary|Collector'?s|Deluxe|Definitive|Ultimate|Gold|Complete|Enhanced|Digital[ ._]Deluxe)[ ._]Edition"#),
        NameRule(id: idPrefix + "openMatte", category: .edition, pattern: #"Open[ ._]Matte"#, template: "Open Matte"),
        NameRule(id: idPrefix + "musicRelease", category: .edition, pattern: #"[\[(](Single|EP|Album|LP|Compilation|Live|OST|Soundtrack|Bootleg|Mixtape|Remix(?:es)?)[\])]"#, template: "$1"),
        NameRule(id: idPrefix + "ep", category: .edition, pattern: #"EP"#, template: "EP", isCaseSensitive: true),
        NameRule(id: idPrefix + "discography", category: .edition, pattern: #"Discography|Дискография"#),
        NameRule(id: idPrefix + "collection", category: .edition, pattern: #"(?<=[\[(])(?:Collection|Сборник|Box[ ._]?Set|Anthology|Антология)(?=[\])])|Complete[ ._](?:Works|Collection)"#),
        NameRule(id: idPrefix + "unabridged", category: .edition, pattern: #"Unabridged|Abridged"#),
        NameRule(id: idPrefix + "goty", category: .edition, pattern: #"GOTY|Game[ ._]of[ ._]the[ ._]Year(?:[ ._]Edition)?"#, template: "GOTY"),
        NameRule(id: idPrefix + "dlcCount", category: .edition, pattern: #"\+[ ._]?(\d{1,3})[ ._]?DLCs?"#, template: "+$1 DLC"),
        NameRule(id: idPrefix + "dlc", category: .edition, pattern: #"(?:\+[ ._]?|incl\.?[ ._]|All[ ._])?DLCs?"#, template: "DLC"),
        NameRule(id: idPrefix + "x64", category: .platform, pattern: #"x86[_-]64|x64|amd64|Win64|64[ ._-]?bit"#, template: "x64"),
        NameRule(id: idPrefix + "x86", category: .platform, pattern: #"x86|i[36]86|Win32|32[ ._-]?bit"#, template: "x86"),
        NameRule(id: idPrefix + "arm64", category: .platform, pattern: #"arm64|aarch64|Apple[ ._]Silicon"#, template: "ARM64"),
        NameRule(id: idPrefix + "macOS", category: .platform, pattern: #"macOS|Mac[ ._]?OS[ ._]?X|OSX"#, template: "macOS"),
        NameRule(id: idPrefix + "windows", category: .platform, pattern: #"Windows|Win(?:7|8|10|11)?"#, template: "Windows"),
        NameRule(id: idPrefix + "linux", category: .platform, pattern: #"[\[(](?:GNU/)?Linux[\])]|(?<=for[ ._])Linux"#, template: "Linux"),
        NameRule(id: idPrefix + "portable", category: .platform, pattern: #"Portable"#, template: "Portable"),
        NameRule(id: idPrefix + "gog", category: .platform, pattern: #"GOG"#, template: "GOG"),
        NameRule(id: idPrefix + "steamRip", category: .platform, pattern: #"Steam-?Rip"#, template: "SteamRip"),
        NameRule(id: idPrefix + "playStation", category: .platform, pattern: #"PS([2-5])"#, template: "PS$1"),
        NameRule(id: idPrefix + "handheld", category: .platform, pattern: #"PSP|PS[ ._]?Vita"#),
        NameRule(id: idPrefix + "nintendoSwitch", category: .platform, pattern: #"(?<=[\[(])Switch(?=[\])])|NSW|Nintendo[ ._]Switch"#, template: "Switch", isCaseSensitive: true),
        NameRule(id: idPrefix + "xbox", category: .platform, pattern: #"Xbox(?:[ ._]?(360|One|Series[ ._]?[XS]))?"#, template: "Xbox $1"),
        NameRule(id: idPrefix + "retroConsole", category: .platform, pattern: #"NES|SNES|N64|GBA|GBC|NDS|3DS|Wii[ ._]?U|Wii|GameCube|NGC|Dreamcast|PSX|Mega[ ._]?Drive|Sega[ ._]CD"#, isCaseSensitive: true),
        NameRule(id: idPrefix + "pc", category: .platform, pattern: #"PC"#, template: "PC", isCaseSensitive: true),
        NameRule(id: idPrefix + "version", category: .version, pattern: #"v\d+(?:\.\d+){1,3}"#),
        NameRule(id: idPrefix + "build", category: .version, pattern: #"Build[ ._]?(\d{2,6})"#, template: "Build $1"),
        NameRule(id: idPrefix + "update", category: .version, pattern: #"Update[ ._]?(\d{1,3}(?:\.\d{1,3})?)"#, template: "Update $1"),
        NameRule(id: idPrefix + "revision", category: .version, pattern: #"\(Rev[ ._]?([0-9A-Z]{1,3})\)"#, template: "Rev $1"),
        NameRule(id: idPrefix + "plainVersion", category: .version, pattern: #"(?<=[ ._(\[])\d{1,4}\.\d{1,4}\.\d{1,5}(?:\.\d{1,6})?"#),
        NameRule(id: idPrefix + "region", category: .region, pattern: #"\(((?:USA|Europe|Japan|World|Korea|Asia|Australia|Brazil|China|France|Germany|Italy|Spain|UK|Canada|Russia|Taiwan)(?:, ?[A-Z][A-Za-z]{1,14})*)\)"#, template: "$1", isCaseSensitive: true),
        NameRule(id: idPrefix + "tvSystem", category: .region, pattern: #"PAL|NTSC(?:-[UJ])?"#, isCaseSensitive: true),
        NameRule(id: idPrefix + "epub", category: .format, pattern: #"EPUB"#, template: "EPUB"),
        NameRule(id: idPrefix + "fb2", category: .format, pattern: #"FB2"#, template: "FB2"),
        NameRule(id: idPrefix + "pdf", category: .format, pattern: #"PDF"#, template: "PDF"),
        NameRule(id: idPrefix + "mobi", category: .format, pattern: #"MOBI|AZW3?"#, template: "Kindle"),
        NameRule(id: idPrefix + "djvu", category: .format, pattern: #"DjVu"#, template: "DjVu"),
        NameRule(id: idPrefix + "cbz", category: .format, pattern: #"CBZ"#, template: "CBZ"),
        NameRule(id: idPrefix + "cbr", category: .format, pattern: #"CBR"#, template: "CBR"),
        NameRule(id: idPrefix + "m4b", category: .format, pattern: #"M4B"#, template: "M4B"),
        NameRule(id: idPrefix + "discImage", category: .format, pattern: #"NSP|XCI|NSZ|XCZ|CIA|RVZ|WBFS|CHD|PKG|VPK|ISO|BIN/CUE"#, isCaseSensitive: true),
        NameRule(id: idPrefix + "container", category: .format, pattern: #"MP4|MKV|AVI"#, isCaseSensitive: true),
        NameRule(id: idPrefix + "repackBy", category: .releaseGroup, pattern: #"(?:Re-?Pack|Rip)[ ._]+(?:от|by)[ ._]+([\p{L}\p{N}_.-]{2,30})"#, template: "$1"),
        NameRule(id: idPrefix + "releasedBy", category: .releaseGroup, pattern: #"(?<=\s)от\s+([\p{L}\p{N}_.-]{2,30})"#, template: "$1"),
        NameRule(id: idPrefix + "narrator", category: .other, pattern: #"(?:читает|Читает|чит\.|Narrated[ ._]by|Read[ ._]by)[ :]+(?-i:\p{Lu})[\p{L}.'’-]+(?:[ ](?-i:\p{Lu})[\p{L}.'’-]+){0,2}"#),
        NameRule(id: idPrefix + "publisher", category: .releaseGroup, pattern: #"(?:Издательство|Изд\.|Publisher)[ :]+([^\[\]()|,]{2,30}?)(?=[\])|,]|$)"#, template: "$1"),
        NameRule(id: idPrefix + "knownGroup", category: .releaseGroup, pattern: #"FitGirl|DODI|xatab|ElAmigos|CODEX|PLAZA|RUNE|TENOKE|SKIDROW|EMPRESS|DARKSiDERS|TinyRepacks|TNT|K'ed"#, isCaseSensitive: true),
        NameRule(id: idPrefix + "romSet", category: .releaseGroup, pattern: #"No-?Intro|Redump|TOSEC|GoodTools|TruRip|FBNeo|FinalBurn[ ._]Neo"#),
        NameRule(id: idPrefix + "repack", category: .other, pattern: #"REPACK\d?|PROPER|RERIP|iNTERNAL|LIMITED|READNFO"#),
        NameRule(id: idPrefix + "verified", category: .other, pattern: #"\[!\]"#, template: "Verified"),
        NameRule(id: idPrefix + "romFlag", category: .other, pattern: #"\((Beta|Proto(?:type)?|Demo|Sample|Unl|Hack|Aftermarket|Homebrew)(?:[ ._]\d{1,2})?\)"#, template: "$1"),
        NameRule(id: idPrefix + "romSetType", category: .other, pattern: #"\(((?:Non-)?Merged|Split)\)"#, template: "$1"),
        NameRule(id: idPrefix + "adultMarker", category: .other, pattern: #"XXX"#, template: "XXX", isCaseSensitive: true),
        NameRule(id: idPrefix + "documentary", category: .other, pattern: #"Documentary|DOCU"#, template: "Documentary"),
        NameRule(id: idPrefix + "lectures", category: .other, pattern: #"\d{1,4}[ ._](?:lectures|lessons|уроков|урока|лекций|лекции)"#),
        NameRule(id: idPrefix + "catalog", category: .other, pattern: #"(?<=[\[(,] ?)[A-Z]{2,6}[- ]?\d{3,6}(?=[\]),])"#, isCaseSensitive: true),
        NameRule(id: idPrefix + "license", category: .other, pattern: #"Лицензия|License"#),
        NameRule(id: idPrefix + "threeD", category: .other, pattern: #"3D|Half[ ._-]?SBS|H-?SBS|H-?OU"#),
        NameRule(id: idPrefix + "audiobookWord", category: .hidden, pattern: #"[\[(](?:Audio[ ._-]?books?|Аудиокнига|Аудиоспектакль)[\])]|^(?:Audio[ ._-]?book|Аудиокнига|Аудиоспектакль)[ ._:-]+"#),
        NameRule(id: idPrefix + "leadingGroup", category: .releaseGroup, pattern: #"^\[([^\]\[]{1,40})\]"#, template: "$1"),
        NameRule(id: idPrefix + "trailingGroup", category: .releaseGroup, pattern: #"-((?=[\p{L}\p{N}_]*\p{L})[\p{L}\p{N}][\p{L}\p{N}_]{1,24})(?=\.[A-Za-z0-9]{2,4}$|$)"#, template: "$1"),
        NameRule(id: idPrefix + "trailingBracketGroup", category: .releaseGroup, pattern: #"\[((?=[^\]\[]*\p{L})[^\]\[]{2,30})\](?=\.[A-Za-z0-9]{2,4}$|$)"#, template: "$1"),
    ]

    /// Localized names of the built-in rules (shown until the user renames one).
    static func localizedName(of id: String) -> String? {
        guard id.hasPrefix(idPrefix) else { return nil }
        return switch id.dropFirst(idPrefix.count) {
        case "fileExtension": String(localized: "names.rule.fileExtension", defaultValue: "File extension", comment: "Built-in torrent name rule")
        case "checksum": String(localized: "names.rule.checksum", defaultValue: "CRC32 checksum", comment: "Built-in torrent name rule")
        case "sitePrefix": String(localized: "names.rule.sitePrefix", defaultValue: "Website prefix", comment: "Built-in torrent name rule")
        case "siteTag": String(localized: "names.rule.siteTag", defaultValue: "Website tag", comment: "Built-in torrent name rule")
        case "courseSite": String(localized: "names.rule.courseSite", defaultValue: "Course platform prefix (Udemy - …)", comment: "Built-in torrent name rule")
        case "studioBeforeDate": String(localized: "names.rule.studioBeforeDate", defaultValue: "Studio before a short date (Site.25.03.14)", comment: "Built-in torrent name rule")
        case "shortDate": String(localized: "names.rule.shortDate", defaultValue: "Short date after the studio (25.03.14)", comment: "Built-in torrent name rule")
        case "res8k": String(localized: "names.rule.res8k", defaultValue: "8K", comment: "Built-in torrent name rule")
        case "res4k": String(localized: "names.rule.res4k", defaultValue: "4K / 2160p", comment: "Built-in torrent name rule")
        case "res1080": String(localized: "names.rule.res1080", defaultValue: "1080p", comment: "Built-in torrent name rule")
        case "res720": String(localized: "names.rule.res720", defaultValue: "720p", comment: "Built-in torrent name rule")
        case "res576": String(localized: "names.rule.res576", defaultValue: "576p", comment: "Built-in torrent name rule")
        case "res480": String(localized: "names.rule.res480", defaultValue: "480p", comment: "Built-in torrent name rule")
        case "date": String(localized: "names.rule.date", defaultValue: "Date", comment: "Built-in torrent name rule")
        case "yearRange": String(localized: "names.rule.yearRange", defaultValue: "Year range", comment: "Built-in torrent name rule")
        case "year": String(localized: "names.rule.year", defaultValue: "Year", comment: "Built-in torrent name rule")
        case "episodeRange": String(localized: "names.rule.episodeRange", defaultValue: "Episode range (S01E01-E03)", comment: "Built-in torrent name rule")
        case "seasonEpisode": String(localized: "names.rule.seasonEpisode", defaultValue: "Season and episode (S01E02)", comment: "Built-in torrent name rule")
        case "crossEpisode": String(localized: "names.rule.crossEpisode", defaultValue: "Season and episode (1x02)", comment: "Built-in torrent name rule")
        case "seasonRange": String(localized: "names.rule.seasonRange", defaultValue: "Season range (S01-S03)", comment: "Built-in torrent name rule")
        case "season": String(localized: "names.rule.season", defaultValue: "Season (S01)", comment: "Built-in torrent name rule")
        case "seasonWordRange": String(localized: "names.rule.seasonWordRange", defaultValue: "Season range (Season 1-5)", comment: "Built-in torrent name rule")
        case "seasonWord": String(localized: "names.rule.seasonWord", defaultValue: "Season (Season 2)", comment: "Built-in torrent name rule")
        case "seasonRussian": String(localized: "names.rule.seasonRussian", defaultValue: "Season (Сезон 2)", comment: "Built-in torrent name rule")
        case "seasonRussianSuffix": String(localized: "names.rule.seasonRussianSuffix", defaultValue: "Season (2 сезон)", comment: "Built-in torrent name rule")
        case "episodesOfRussian": String(localized: "names.rule.episodesOfRussian", defaultValue: "Episodes of total (1-8 из 10)", comment: "Built-in torrent name rule")
        case "episodeOfRussian": String(localized: "names.rule.episodeOfRussian", defaultValue: "Episode of total (3 из 10)", comment: "Built-in torrent name rule")
        case "episodesRussian": String(localized: "names.rule.episodesRussian", defaultValue: "Episodes (Серии 1-8)", comment: "Built-in torrent name rule")
        case "episodeBatch": String(localized: "names.rule.episodeBatch", defaultValue: "Episode batch ([01-12])", comment: "Built-in torrent name rule")
        case "episodeWord": String(localized: "names.rule.episodeWord", defaultValue: "Episode (Ep 05)", comment: "Built-in torrent name rule")
        case "episodeDash": String(localized: "names.rule.episodeDash", defaultValue: "Episode after a dash (- 05)", comment: "Built-in torrent name rule")
        case "complete": String(localized: "names.rule.complete", defaultValue: "Complete series", comment: "Built-in torrent name rule")
        case "volumeRange": String(localized: "names.rule.volumeRange", defaultValue: "Volume range (v01-v12, Vol. 1-3)", comment: "Built-in torrent name rule")
        case "volume": String(localized: "names.rule.volume", defaultValue: "Volume (v01, Vol. 3, Том 2)", comment: "Built-in torrent name rule")
        case "chapterRange": String(localized: "names.rule.chapterRange", defaultValue: "Chapter range (c001-c050)", comment: "Built-in torrent name rule")
        case "chapter": String(localized: "names.rule.chapter", defaultValue: "Chapter (c001, Ch. 12, Глава 3)", comment: "Built-in torrent name rule")
        case "issue": String(localized: "names.rule.issue", defaultValue: "Comic issue (#12, 001)", comment: "Built-in torrent name rule")
        case "disc": String(localized: "names.rule.disc", defaultValue: "Disc (CD1, Disc 2)", comment: "Built-in torrent name rule")
        case "dolbyVision": String(localized: "names.rule.dolbyVision", defaultValue: "Dolby Vision", comment: "Built-in torrent name rule")
        case "hdr10Plus": String(localized: "names.rule.hdr10Plus", defaultValue: "HDR10+", comment: "Built-in torrent name rule")
        case "hdr": String(localized: "names.rule.hdr", defaultValue: "HDR / HDR10", comment: "Built-in torrent name rule")
        case "hlg": String(localized: "names.rule.hlg", defaultValue: "HLG", comment: "Built-in torrent name rule")
        case "remux": String(localized: "names.rule.remux", defaultValue: "Remux", comment: "Built-in torrent name rule")
        case "webDLRip": String(localized: "names.rule.webDLRip", defaultValue: "WEB-DLRip", comment: "Built-in torrent name rule")
        case "webDL": String(localized: "names.rule.webDL", defaultValue: "WEB-DL", comment: "Built-in torrent name rule")
        case "webRip": String(localized: "names.rule.webRip", defaultValue: "WEBRip", comment: "Built-in torrent name rule")
        case "web": String(localized: "names.rule.web", defaultValue: "WEB", comment: "Built-in torrent name rule")
        case "bdRip": String(localized: "names.rule.bdRip", defaultValue: "BDRip", comment: "Built-in torrent name rule")
        case "bluRay": String(localized: "names.rule.bluRay", defaultValue: "Blu-ray", comment: "Built-in torrent name rule")
        case "hdtv": String(localized: "names.rule.hdtv", defaultValue: "HDTV", comment: "Built-in torrent name rule")
        case "hdRip": String(localized: "names.rule.hdRip", defaultValue: "HDRip", comment: "Built-in torrent name rule")
        case "dvdRip": String(localized: "names.rule.dvdRip", defaultValue: "DVDRip", comment: "Built-in torrent name rule")
        case "dvd": String(localized: "names.rule.dvd", defaultValue: "DVD", comment: "Built-in torrent name rule")
        case "cam": String(localized: "names.rule.cam", defaultValue: "Camera recording", comment: "Built-in torrent name rule")
        case "satRip": String(localized: "names.rule.satRip", defaultValue: "Satellite", comment: "Built-in torrent name rule")
        case "amazon": String(localized: "names.rule.amazon", defaultValue: "Amazon", comment: "Built-in torrent name rule")
        case "netflix": String(localized: "names.rule.netflix", defaultValue: "Netflix", comment: "Built-in torrent name rule")
        case "disney": String(localized: "names.rule.disney", defaultValue: "Disney+", comment: "Built-in torrent name rule")
        case "appleTV": String(localized: "names.rule.appleTV", defaultValue: "Apple TV / iTunes", comment: "Built-in torrent name rule")
        case "max": String(localized: "names.rule.max", defaultValue: "Max", comment: "Built-in torrent name rule")
        case "vinyl": String(localized: "names.rule.vinyl", defaultValue: "Vinyl rip", comment: "Built-in torrent name rule")
        case "cd": String(localized: "names.rule.cd", defaultValue: "CD rip", comment: "Built-in torrent name rule")
        case "digital": String(localized: "names.rule.digital", defaultValue: "Digital release (comics)", comment: "Built-in torrent name rule")
        case "hevc": String(localized: "names.rule.hevc", defaultValue: "HEVC / H.265", comment: "Built-in torrent name rule")
        case "avc": String(localized: "names.rule.avc", defaultValue: "H.264 / AVC", comment: "Built-in torrent name rule")
        case "av1": String(localized: "names.rule.av1", defaultValue: "AV1", comment: "Built-in torrent name rule")
        case "vp9": String(localized: "names.rule.vp9", defaultValue: "VP9", comment: "Built-in torrent name rule")
        case "xvid": String(localized: "names.rule.xvid", defaultValue: "XviD / DivX", comment: "Built-in torrent name rule")
        case "tenBit": String(localized: "names.rule.tenBit", defaultValue: "10-bit video", comment: "Built-in torrent name rule")
        case "atmos": String(localized: "names.rule.atmos", defaultValue: "Dolby Atmos", comment: "Built-in torrent name rule")
        case "trueHD": String(localized: "names.rule.trueHD", defaultValue: "Dolby TrueHD", comment: "Built-in torrent name rule")
        case "dtsX": String(localized: "names.rule.dtsX", defaultValue: "DTS:X", comment: "Built-in torrent name rule")
        case "dtsHDMA": String(localized: "names.rule.dtsHDMA", defaultValue: "DTS-HD Master Audio", comment: "Built-in torrent name rule")
        case "dtsHD": String(localized: "names.rule.dtsHD", defaultValue: "DTS-HD", comment: "Built-in torrent name rule")
        case "dts": String(localized: "names.rule.dts", defaultValue: "DTS", comment: "Built-in torrent name rule")
        case "ddPlus": String(localized: "names.rule.ddPlus", defaultValue: "Dolby Digital Plus", comment: "Built-in torrent name rule")
        case "dd": String(localized: "names.rule.dd", defaultValue: "Dolby Digital", comment: "Built-in torrent name rule")
        case "aac": String(localized: "names.rule.aac", defaultValue: "AAC", comment: "Built-in torrent name rule")
        case "flac": String(localized: "names.rule.flac", defaultValue: "FLAC", comment: "Built-in torrent name rule")
        case "hiResFormat": String(localized: "names.rule.hiResFormat", defaultValue: "Bit depth and sample rate (24/96)", comment: "Built-in torrent name rule")
        case "bitDepth": String(localized: "names.rule.bitDepth", defaultValue: "Audio bit depth (24-bit)", comment: "Built-in torrent name rule")
        case "sampleRate": String(localized: "names.rule.sampleRate", defaultValue: "Sample rate (96 kHz)", comment: "Built-in torrent name rule")
        case "bitrateBracket": String(localized: "names.rule.bitrateBracket", defaultValue: "Bitrate in brackets ([320])", comment: "Built-in torrent name rule")
        case "mp3": String(localized: "names.rule.mp3", defaultValue: "MP3", comment: "Built-in torrent name rule")
        case "bitrate": String(localized: "names.rule.bitrate", defaultValue: "Bitrate (320 kbps)", comment: "Built-in torrent name rule")
        case "otherAudio": String(localized: "names.rule.otherAudio", defaultValue: "Other audio formats", comment: "Built-in torrent name rule")
        case "channels": String(localized: "names.rule.channels", defaultValue: "Audio channels (5.1)", comment: "Built-in torrent name rule")
        case "vbr": String(localized: "names.rule.vbr", defaultValue: "MP3 VBR (V0)", comment: "Built-in torrent name rule")
        case "multiSub": String(localized: "names.rule.multiSub", defaultValue: "Multiple subtitles", comment: "Built-in torrent name rule")
        case "engSub": String(localized: "names.rule.engSub", defaultValue: "English subtitles", comment: "Built-in torrent name rule")
        case "rusSub": String(localized: "names.rule.rusSub", defaultValue: "Russian subtitles", comment: "Built-in torrent name rule")
        case "langSub": String(localized: "names.rule.langSub", defaultValue: "Other language subtitles", comment: "Built-in torrent name rule")
        case "vostfr": String(localized: "names.rule.vostfr", defaultValue: "French subtitles (VOSTFR)", comment: "Built-in torrent name rule")
        case "hardsub": String(localized: "names.rule.hardsub", defaultValue: "Burned-in subtitles", comment: "Built-in torrent name rule")
        case "subs": String(localized: "names.rule.subs", defaultValue: "Subtitles", comment: "Built-in torrent name rule")
        case "dubRussian": String(localized: "names.rule.dubRussian", defaultValue: "Dub (Russian names)", comment: "Built-in torrent name rule")
        case "dub": String(localized: "names.rule.dub", defaultValue: "Dub", comment: "Built-in torrent name rule")
        case "mvo": String(localized: "names.rule.mvo", defaultValue: "Multi-voice voice-over", comment: "Built-in torrent name rule")
        case "dvo": String(localized: "names.rule.dvo", defaultValue: "Two-voice voice-over", comment: "Built-in torrent name rule")
        case "avo": String(localized: "names.rule.avo", defaultValue: "Author's voice-over", comment: "Built-in torrent name rule")
        case "vo": String(localized: "names.rule.vo", defaultValue: "Single-voice voice-over", comment: "Built-in torrent name rule")
        case "multiLanguageCount": String(localized: "names.rule.multiLanguageCount", defaultValue: "Number of languages (MULTi10)", comment: "Built-in torrent name rule")
        case "languageList": String(localized: "names.rule.languageList", defaultValue: "Language list ((En,Fr,De))", comment: "Built-in torrent name rule")
        case "languagePair": String(localized: "names.rule.languagePair", defaultValue: "Interface languages ([Ru/En])", comment: "Built-in torrent name rule")
        case "dualAudio": String(localized: "names.rule.dualAudio", defaultValue: "Dual audio", comment: "Built-in torrent name rule")
        case "multiAudio": String(localized: "names.rule.multiAudio", defaultValue: "Multiple audio languages", comment: "Built-in torrent name rule")
        case "russianAudio": String(localized: "names.rule.russianAudio", defaultValue: "Russian audio", comment: "Built-in torrent name rule")
        case "englishAudio": String(localized: "names.rule.englishAudio", defaultValue: "English audio", comment: "Built-in torrent name rule")
        case "ukrainianAudio": String(localized: "names.rule.ukrainianAudio", defaultValue: "Ukrainian audio", comment: "Built-in torrent name rule")
        case "japaneseAudio": String(localized: "names.rule.japaneseAudio", defaultValue: "Japanese audio", comment: "Built-in torrent name rule")
        case "otherAudioLanguage": String(localized: "names.rule.otherAudioLanguage", defaultValue: "Other audio languages", comment: "Built-in torrent name rule")
        case "directorsCut": String(localized: "names.rule.directorsCut", defaultValue: "Director's Cut", comment: "Built-in torrent name rule")
        case "extended": String(localized: "names.rule.extended", defaultValue: "Extended", comment: "Built-in torrent name rule")
        case "theatrical": String(localized: "names.rule.theatrical", defaultValue: "Theatrical", comment: "Built-in torrent name rule")
        case "unrated": String(localized: "names.rule.unrated", defaultValue: "Unrated / Uncut", comment: "Built-in torrent name rule")
        case "imax": String(localized: "names.rule.imax", defaultValue: "IMAX", comment: "Built-in torrent name rule")
        case "remastered": String(localized: "names.rule.remastered", defaultValue: "Remastered", comment: "Built-in torrent name rule")
        case "criterion": String(localized: "names.rule.criterion", defaultValue: "Criterion", comment: "Built-in torrent name rule")
        case "specialEdition": String(localized: "names.rule.specialEdition", defaultValue: "Special editions", comment: "Built-in torrent name rule")
        case "openMatte": String(localized: "names.rule.openMatte", defaultValue: "Open Matte", comment: "Built-in torrent name rule")
        case "musicRelease": String(localized: "names.rule.musicRelease", defaultValue: "Music release type ((EP), [Single])", comment: "Built-in torrent name rule")
        case "ep": String(localized: "names.rule.ep", defaultValue: "EP", comment: "Built-in torrent name rule")
        case "discography": String(localized: "names.rule.discography", defaultValue: "Discography", comment: "Built-in torrent name rule")
        case "collection": String(localized: "names.rule.collection", defaultValue: "Collection", comment: "Built-in torrent name rule")
        case "unabridged": String(localized: "names.rule.unabridged", defaultValue: "Unabridged / abridged", comment: "Built-in torrent name rule")
        case "goty": String(localized: "names.rule.goty", defaultValue: "Game of the Year edition", comment: "Built-in torrent name rule")
        case "dlcCount": String(localized: "names.rule.dlcCount", defaultValue: "Number of DLCs (+5 DLC)", comment: "Built-in torrent name rule")
        case "dlc": String(localized: "names.rule.dlc", defaultValue: "DLC", comment: "Built-in torrent name rule")
        case "x64": String(localized: "names.rule.x64", defaultValue: "64-bit (x64)", comment: "Built-in torrent name rule")
        case "x86": String(localized: "names.rule.x86", defaultValue: "32-bit (x86)", comment: "Built-in torrent name rule")
        case "arm64": String(localized: "names.rule.arm64", defaultValue: "ARM64", comment: "Built-in torrent name rule")
        case "macOS": String(localized: "names.rule.macOS", defaultValue: "macOS", comment: "Built-in torrent name rule")
        case "windows": String(localized: "names.rule.windows", defaultValue: "Windows", comment: "Built-in torrent name rule")
        case "linux": String(localized: "names.rule.linux", defaultValue: "Linux", comment: "Built-in torrent name rule")
        case "portable": String(localized: "names.rule.portable", defaultValue: "Portable", comment: "Built-in torrent name rule")
        case "gog": String(localized: "names.rule.gog", defaultValue: "GOG", comment: "Built-in torrent name rule")
        case "steamRip": String(localized: "names.rule.steamRip", defaultValue: "SteamRip", comment: "Built-in torrent name rule")
        case "playStation": String(localized: "names.rule.playStation", defaultValue: "PlayStation (PS4, PS5)", comment: "Built-in torrent name rule")
        case "handheld": String(localized: "names.rule.handheld", defaultValue: "PSP / PS Vita", comment: "Built-in torrent name rule")
        case "nintendoSwitch": String(localized: "names.rule.nintendoSwitch", defaultValue: "Nintendo Switch", comment: "Built-in torrent name rule")
        case "xbox": String(localized: "names.rule.xbox", defaultValue: "Xbox", comment: "Built-in torrent name rule")
        case "retroConsole": String(localized: "names.rule.retroConsole", defaultValue: "Retro consoles (SNES, GBA, Wii)", comment: "Built-in torrent name rule")
        case "pc": String(localized: "names.rule.pc", defaultValue: "PC", comment: "Built-in torrent name rule")
        case "version": String(localized: "names.rule.version", defaultValue: "Version (v1.2.3)", comment: "Built-in torrent name rule")
        case "build": String(localized: "names.rule.build", defaultValue: "Build number (Build 1234)", comment: "Built-in torrent name rule")
        case "update": String(localized: "names.rule.update", defaultValue: "Update (Update 5)", comment: "Built-in torrent name rule")
        case "revision": String(localized: "names.rule.revision", defaultValue: "Revision ((Rev 1))", comment: "Built-in torrent name rule")
        case "plainVersion": String(localized: "names.rule.plainVersion", defaultValue: "Version without v (3.2.1)", comment: "Built-in torrent name rule")
        case "region": String(localized: "names.rule.region", defaultValue: "Region ((USA), (Europe))", comment: "Built-in torrent name rule")
        case "tvSystem": String(localized: "names.rule.tvSystem", defaultValue: "PAL / NTSC", comment: "Built-in torrent name rule")
        case "epub": String(localized: "names.rule.epub", defaultValue: "EPUB", comment: "Built-in torrent name rule")
        case "fb2": String(localized: "names.rule.fb2", defaultValue: "FB2", comment: "Built-in torrent name rule")
        case "pdf": String(localized: "names.rule.pdf", defaultValue: "PDF", comment: "Built-in torrent name rule")
        case "mobi": String(localized: "names.rule.mobi", defaultValue: "Kindle (MOBI, AZW3)", comment: "Built-in torrent name rule")
        case "djvu": String(localized: "names.rule.djvu", defaultValue: "DjVu", comment: "Built-in torrent name rule")
        case "cbz": String(localized: "names.rule.cbz", defaultValue: "CBZ comic archive", comment: "Built-in torrent name rule")
        case "cbr": String(localized: "names.rule.cbr", defaultValue: "CBR comic archive", comment: "Built-in torrent name rule")
        case "m4b": String(localized: "names.rule.m4b", defaultValue: "M4B audiobook", comment: "Built-in torrent name rule")
        case "discImage": String(localized: "names.rule.discImage", defaultValue: "Game and disc images (NSP, CHD, ISO)", comment: "Built-in torrent name rule")
        case "container": String(localized: "names.rule.container", defaultValue: "Video container (MP4, MKV)", comment: "Built-in torrent name rule")
        case "repackBy": String(localized: "names.rule.repackBy", defaultValue: "Repack author (RePack by …)", comment: "Built-in torrent name rule")
        case "releasedBy": String(localized: "names.rule.releasedBy", defaultValue: "Release author (от …)", comment: "Built-in torrent name rule")
        case "narrator": String(localized: "names.rule.narrator", defaultValue: "Narrator (читает …, Narrated by …)", comment: "Built-in torrent name rule")
        case "publisher": String(localized: "names.rule.publisher", defaultValue: "Publisher (Издательство …)", comment: "Built-in torrent name rule")
        case "knownGroup": String(localized: "names.rule.knownGroup", defaultValue: "Well-known repack and release groups", comment: "Built-in torrent name rule")
        case "romSet": String(localized: "names.rule.romSet", defaultValue: "ROM set (No-Intro, Redump, TOSEC)", comment: "Built-in torrent name rule")
        case "repack": String(localized: "names.rule.repack", defaultValue: "Scene flags (REPACK, PROPER)", comment: "Built-in torrent name rule")
        case "verified": String(localized: "names.rule.verified", defaultValue: "Verified dump ([!])", comment: "Built-in torrent name rule")
        case "romFlag": String(localized: "names.rule.romFlag", defaultValue: "ROM flags ((Beta), (Unl))", comment: "Built-in torrent name rule")
        case "romSetType": String(localized: "names.rule.romSetType", defaultValue: "ROM set type ((Merged))", comment: "Built-in torrent name rule")
        case "adultMarker": String(localized: "names.rule.adultMarker", defaultValue: "Adult marker (XXX)", comment: "Built-in torrent name rule")
        case "documentary": String(localized: "names.rule.documentary", defaultValue: "Documentary", comment: "Built-in torrent name rule")
        case "lectures": String(localized: "names.rule.lectures", defaultValue: "Number of lectures", comment: "Built-in torrent name rule")
        case "catalog": String(localized: "names.rule.catalog", defaultValue: "Catalog number ([ABC-1234])", comment: "Built-in torrent name rule")
        // No longer built in; kept for rule lists edited in earlier versions.
        case "bookFormat": String(localized: "names.rule.bookFormat", defaultValue: "Book formats", comment: "Built-in torrent name rule")
        case "license": String(localized: "names.rule.license", defaultValue: "Licensed release", comment: "Built-in torrent name rule")
        case "threeD": String(localized: "names.rule.threeD", defaultValue: "3D", comment: "Built-in torrent name rule")
        case "audiobookWord": String(localized: "names.rule.audiobookWord", defaultValue: "The word \"audiobook\"", comment: "Built-in torrent name rule")
        case "leadingGroup": String(localized: "names.rule.leadingGroup", defaultValue: "Group in leading brackets ([Group] …)", comment: "Built-in torrent name rule")
        case "comicScanner": String(localized: "names.rule.comicScanner", defaultValue: "Comic scanner tag (… (Zone-Empire))", comment: "Built-in torrent name rule")
        case "trailingGroup": String(localized: "names.rule.trailingGroup", defaultValue: "Group after the last dash (…-GROUP)", comment: "Built-in torrent name rule")
        case "trailingBracketGroup": String(localized: "names.rule.trailingBracketGroup", defaultValue: "Group in trailing brackets (… [Group])", comment: "Built-in torrent name rule")
        default: nil
        }
    }
}
// swiftlint:enable line_length
