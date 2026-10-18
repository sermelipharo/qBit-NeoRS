//! RSS auto-download rules (qBittorrent `RSS::AutoDownloadRule`): matching
//! (must/must-not contain in wildcard or regex mode, episode filter, smart
//! episode filter, ignore days) and the `download_rules.json` /
//! `rss/rules` JSON format including the deprecated keys.
//!
//! User patterns are compiled with the linear-time `regex` crate under size
//! limits; wildcard tokens are converted with every metacharacter escaped.
//! An invalid pattern never matches (like an invalid `QRegularExpression`).

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value};

use super::date::{format_rfc2822, parse_rfc2822};
use crate::model::{
    AddTorrentParams, ContentLayout, RssAutoDownloadRule, ShareLimitAction, StopCondition, TorrentParamsTemplate,
};

const MAX_PATTERN_LEN: usize = 4096;
const REGEX_SIZE_LIMIT: usize = 2 * 1024 * 1024;
const MS_PER_DAY: i64 = 86_400_000;

/// Compiles a user-supplied, case-insensitive pattern with resource limits.
pub(crate) fn compile_user_regex(pattern: &str, extended: bool) -> Option<Regex> {
    if pattern.len() > MAX_PATTERN_LEN {
        return None;
    }
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .ignore_whitespace(extended)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .nest_limit(64)
        .build()
        .ok()
}

/// `QRegularExpression::wildcardToRegularExpression(p, Unanchored |
/// NonPath)`: `*` any run, `?` one character, `[...]`/`[!...]` sets; every
/// other character is literal. Unterminated sets make the pattern invalid.
pub(crate) fn wildcard_to_regex(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::from("(?s:");
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                let mut j = i + 1;
                let mut class = String::from("[");
                if j < chars.len() && (chars[j] == '!' || chars[j] == '^') {
                    class.push('^');
                    j += 1;
                }
                // A leading ']' is a literal member.
                if j < chars.len() && chars[j] == ']' {
                    class.push_str("\\]");
                    j += 1;
                }
                let mut closed = false;
                while j < chars.len() {
                    let c = chars[j];
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    if c == '-' && j + 1 < chars.len() && chars[j + 1] != ']' && class.len() > 1 && !class.ends_with('^') {
                        class.push('-');
                    } else {
                        push_class_literal(&mut class, c);
                    }
                    j += 1;
                }
                if !closed {
                    return None;
                }
                class.push(']');
                out.push_str(&class);
                i = j;
            }
            c => out.push_str(&regex::escape(c.encode_utf8(&mut [0; 4]))),
        }
        i += 1;
    }
    out.push(')');
    Some(out)
}

fn push_class_literal(class: &mut String, c: char) {
    if matches!(c, '\\' | '[' | ']' | '^' | '-' | '&' | '~') {
        class.push('\\');
    }
    class.push(c);
}

/// One `mustContain`/`mustNotContain` alternative.
#[derive(Clone, Debug)]
enum Expr {
    /// Empty alternative: always matches.
    Empty,
    /// Regex mode; `None` = invalid.
    Regex(Option<Regex>),
    /// Wildcard mode: every token must match (any order).
    Tokens(Vec<Option<Regex>>),
}

impl Expr {
    fn compile(expr: &str, use_regex: bool) -> Self {
        if expr.is_empty() {
            return Expr::Empty;
        }
        if use_regex {
            return Expr::Regex(compile_user_regex(expr, false));
        }
        Expr::Tokens(
            expr.split_whitespace()
                .map(|tok| wildcard_to_regex(tok).and_then(|p| compile_user_regex(&p, false)))
                .collect(),
        )
    }

    fn matches(&self, title: &str) -> bool {
        match self {
            Expr::Empty => true,
            Expr::Regex(re) => re.as_ref().is_some_and(|r| r.is_match(title)),
            Expr::Tokens(tokens) => tokens.iter().all(|t| t.as_ref().is_some_and(|r| r.is_match(title))),
        }
    }
}

/// `\b` and `\d` are ASCII in these patterns, as in PCRE without UCP.
static EPISODE_RANGE_1: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?-u:\b)s0?([0-9]{1,4})[ -_\.]?e(0?[0-9]{1,4})(?:[^0-9]|(?-u:\b))")
        .case_insensitive(true)
        .build()
        .expect("valid regex")
});
static EPISODE_RANGE_2: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?-u:\b)([0-9]{1,4})x(0?[0-9]{1,4})(?:[^0-9]|(?-u:\b))")
        .case_insensitive(true)
        .build()
        .expect("valid regex")
});
static EPISODE_FILTER: LazyLock<Regex> =
    LazyLock::new(|| RegexBuilder::new(r"^([0-9]{1,4})x(.*;)$").case_insensitive(true).build().expect("valid regex"));
/// GUI validation pattern of the episode filter field.
static EPISODE_FILTER_UI: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"^(?:[0-9]{1,4}x(?:[0-9]{1,4}(?:-(?:[0-9]{1,4})?)?;)+)")
        .case_insensitive(true)
        .build()
        .expect("valid regex")
});

/// `true` if `filter` passes the RSS Downloader dialog's validation
/// (empty is valid).
pub fn is_valid_episode_filter(filter: &str) -> bool {
    filter.is_empty() || EPISODE_FILTER_UI.is_match(filter)
}

#[derive(Clone, Debug)]
enum EpisodeToken {
    /// `N-`: episode N and later of the season, and every later season.
    From(i64),
    /// `A-B` (ignored when A > B).
    Range(i64, i64),
    /// Single episode (pre-built pattern; `None` if it could not compile).
    Single(Option<Regex>),
}

#[derive(Clone, Debug)]
enum EpisodeFilter {
    /// No filter.
    None,
    /// The filter text does not match `(^\d{1,4})x(.*;$)`: rejects all.
    Invalid,
    Season { season: i64, tokens: Vec<EpisodeToken> },
}

/// Qt `QStringView::toInt()`: 0 on failure.
fn to_int(s: &str) -> i64 {
    s.parse::<i32>().map(i64::from).unwrap_or(0)
}

impl EpisodeFilter {
    fn compile(filter: &str) -> Self {
        if filter.is_empty() {
            return EpisodeFilter::None;
        }
        let Some(c) = EPISODE_FILTER.captures(filter) else {
            return EpisodeFilter::Invalid;
        };
        let season_str = c.get(1).map_or("", |m| m.as_str());
        let season = to_int(season_str);
        let mut tokens = Vec::new();
        for raw in c.get(2).map_or("", |m| m.as_str()).split(';') {
            if raw.is_empty() {
                continue;
            }
            let mut ep = raw;
            while ep.len() > 1 && ep.starts_with('0') {
                ep = &ep[1..];
            }
            if ep.contains('-') {
                if let Some(start) = ep.strip_suffix('-') {
                    tokens.push(EpisodeToken::From(to_int(start)));
                } else {
                    let parts: Vec<&str> = ep.split('-').collect();
                    let first = to_int(parts.first().copied().unwrap_or(""));
                    let last = to_int(parts.last().copied().unwrap_or(""));
                    tokens.push(EpisodeToken::Range(first, last));
                }
            } else {
                let ep = regex::escape(ep);
                let pattern = format!(
                    r"(?-u:\b)(?:s0?{season_str}[ -_\.]?e0?{ep}|{season_str}x0?{ep})(?:[^0-9]|(?-u:\b))"
                );
                tokens.push(EpisodeToken::Single(compile_user_regex(&pattern, false)));
            }
        }
        EpisodeFilter::Season { season, tokens }
    }

    /// `matchesEpisodeFilterExpression`.
    fn matches(&self, title: &str) -> bool {
        let (season_ours, tokens) = match self {
            EpisodeFilter::None => return true,
            EpisodeFilter::Invalid => return false,
            EpisodeFilter::Season { season, tokens } => (*season, tokens),
        };
        for token in tokens {
            match token {
                EpisodeToken::Single(re) => {
                    if re.as_ref().is_some_and(|r| r.is_match(title)) {
                        return true;
                    }
                }
                EpisodeToken::From(_) | EpisodeToken::Range(..) => {
                    let Some(c) = EPISODE_RANGE_1.captures(title).or_else(|| EPISODE_RANGE_2.captures(title)) else {
                        continue;
                    };
                    let season_theirs = to_int(c.get(1).map_or("", |m| m.as_str()));
                    let episode_theirs = to_int(c.get(2).map_or("", |m| m.as_str()));
                    match token {
                        EpisodeToken::From(start) => {
                            if (season_theirs == season_ours && episode_theirs >= *start) || season_theirs > season_ours {
                                return true;
                            }
                        }
                        EpisodeToken::Range(first, last) => {
                            if first > last {
                                continue;
                            }
                            if season_theirs == season_ours && *first <= episode_theirs && episode_theirs <= *last {
                                return true;
                            }
                        }
                        EpisodeToken::Single(_) => {}
                    }
                }
            }
        }
        false
    }
}

/// Global settings the matcher needs (`AutoDownloader` state).
pub(crate) struct MatchContext {
    filters: Vec<String>,
    /// `(?:_|\b)(?:f1|f2|…)(?:_|\b)`; `None` if the filters are invalid.
    /// Compiled on first use: most profiles never use smart episode filters.
    smart_regex: std::sync::OnceLock<Option<Regex>>,
    pub download_repacks: bool,
}

impl MatchContext {
    pub fn new(filters: &[String], download_repacks: bool) -> Self {
        Self { filters: filters.to_vec(), smart_regex: std::sync::OnceLock::new(), download_repacks }
    }

    fn smart_regex(&self) -> Option<&Regex> {
        self.smart_regex.get_or_init(|| smart_episode_regex(&self.filters)).as_ref()
    }
}

/// `computeSmartFilterRegex` with CaseInsensitive | Extended | UCP.
pub(crate) fn smart_episode_regex(filters: &[String]) -> Option<Regex> {
    let joined = filters.join("|");
    compile_user_regex(&format!(r"(?:_|\b)(?:{joined})(?:_|\b)"), true)
}

/// `computeEpisodeName`: `S01E05` -> `1x5`, `2017.01.05` stays as is.
pub(crate) fn compute_episode_name(title: &str, ctx: &MatchContext) -> String {
    let Some(re) = ctx.smart_regex() else {
        return String::new();
    };
    let Some(c) = re.captures(title) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for g in c.iter().skip(1).flatten() {
        let cap = g.as_str();
        if cap.is_empty() {
            continue;
        }
        parts.push(match cap.parse::<i32>() {
            Ok(n) => n.to_string(),
            Err(_) => cap.to_owned(),
        });
    }
    parts.join("x")
}

/// A rule with its compiled matchers.
#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub name: String,
    pub enabled: bool,
    pub priority: i64,
    pub use_regex: bool,
    must_contain: Vec<String>,
    must_not_contain: Vec<String>,
    pub episode_filter: String,
    pub feed_urls: Vec<String>,
    pub ignore_days: i64,
    /// Unix ms of the last accepted article.
    pub last_match: Option<i64>,
    pub smart_filter: bool,
    pub previously_matched: Vec<String>,
    pub params: TorrentParamsTemplate,
    /// `torrentParams` keys the template does not model (`operating_mode`,
    /// `share_limit_action`, `ssl_*`, unknown keys), kept for round trips.
    pub params_extra: Map<String, Value>,
    must_exprs: Vec<Expr>,
    must_not_exprs: Vec<Expr>,
    episode: EpisodeFilter,
}

/// `setMustContain`: regex mode keeps the text whole, wildcard mode splits
/// on `|`; a single empty string means "no condition".
fn split_expressions(text: &str, use_regex: bool) -> Vec<String> {
    let list: Vec<String> = if use_regex { vec![text.to_owned()] } else { text.split('|').map(str::to_owned).collect() };
    if list.len() == 1 && list[0].is_empty() { Vec::new() } else { list }
}

impl Rule {
    pub fn new(name: &str) -> Self {
        let mut r = Self {
            name: name.to_owned(),
            enabled: true,
            priority: 0,
            use_regex: false,
            must_contain: Vec::new(),
            must_not_contain: Vec::new(),
            episode_filter: String::new(),
            feed_urls: Vec::new(),
            ignore_days: 0,
            last_match: None,
            smart_filter: false,
            previously_matched: Vec::new(),
            params: default_template(),
            params_extra: Map::new(),
            must_exprs: Vec::new(),
            must_not_exprs: Vec::new(),
            episode: EpisodeFilter::None,
        };
        r.compile();
        r
    }

    /// Sets the filters (order matters: `use_regex` decides the splitting).
    pub fn set_filters(&mut self, use_regex: bool, must_contain: &str, must_not_contain: &str, episode_filter: &str) {
        self.use_regex = use_regex;
        self.must_contain = split_expressions(must_contain, use_regex);
        self.must_not_contain = split_expressions(must_not_contain, use_regex);
        self.episode_filter = episode_filter.to_owned();
        self.compile();
    }

    fn compile(&mut self) {
        self.must_exprs = self.must_contain.iter().map(|e| Expr::compile(e, self.use_regex)).collect();
        self.must_not_exprs = self.must_not_contain.iter().map(|e| Expr::compile(e, self.use_regex)).collect();
        self.episode = EpisodeFilter::compile(&self.episode_filter);
    }

    pub fn must_contain(&self) -> String {
        self.must_contain.join("|")
    }

    pub fn must_not_contain(&self) -> String {
        self.must_not_contain.join("|")
    }

    fn smart_filter_matches(&self, title: &str, ctx: &MatchContext, episodes: &mut Vec<String>) -> bool {
        if !self.smart_filter {
            return true;
        }
        let episode = compute_episode_name(title, ctx);
        if episode.is_empty() {
            return false;
        }
        if !self.previously_matched.contains(&episode) {
            episodes.push(episode);
            return true;
        }
        if !ctx.download_repacks {
            return false;
        }
        let lower = title.to_lowercase();
        let is_repack = lower.contains("repack");
        let is_proper = lower.contains("proper");
        if !is_repack && !is_proper {
            return false;
        }
        let full = format!(
            "{episode}{}{}",
            if is_repack { "-REPACK" } else { "" },
            if is_proper { "-PROPER" } else { "" }
        );
        if self.previously_matched.contains(&full) {
            return false;
        }
        episodes.push(full);
        if is_repack && is_proper {
            episodes.push(format!("{episode}-REPACK"));
            episodes.push(format!("{episode}-PROPER"));
        }
        true
    }

    /// `AutoDownloadRule::matches` (side-effect free). On success returns the
    /// episode keys `accepts` would remember.
    pub fn matches(&self, title: &str, date_ms: i64, ctx: &MatchContext) -> Option<Vec<String>> {
        if self.ignore_days > 0
            && let Some(last) = self.last_match
            && date_ms < last.saturating_add(self.ignore_days.saturating_mul(MS_PER_DAY))
        {
            return None;
        }
        if !self.must_exprs.is_empty() && !self.must_exprs.iter().any(|e| e.matches(title)) {
            return None;
        }
        if !self.must_not_exprs.is_empty() && self.must_not_exprs.iter().any(|e| e.matches(title)) {
            return None;
        }
        if !self.episode.matches(title) {
            return None;
        }
        let mut episodes = Vec::new();
        if !self.smart_filter_matches(title, ctx, &mut episodes) {
            return None;
        }
        Some(episodes)
    }

    /// `AutoDownloadRule::accepts`: like `matches`, then records the match.
    pub fn accepts(&mut self, title: &str, date_ms: i64, ctx: &MatchContext) -> bool {
        let Some(episodes) = self.matches(title, date_ms, ctx) else {
            return false;
        };
        self.last_match = Some(date_ms);
        self.previously_matched.extend(episodes);
        true
    }

    /// Parameters for `Session::add_torrent`.
    pub fn add_torrent_params(&self) -> AddTorrentParams {
        let p = &self.params;
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        AddTorrentParams {
            category: non_empty(&p.category),
            tags: p.tags.clone(),
            save_path: non_empty(&p.save_path),
            use_download_path: p.use_download_path,
            download_path: non_empty(&p.download_path),
            auto_tmm: p.auto_tmm,
            stopped: p.stopped,
            stop_condition: p.stop_condition,
            skip_checking: p.skip_checking,
            add_to_queue_top: p.add_to_queue_top,
            content_layout: p.content_layout,
            forced: self.params_extra.get("operating_mode").and_then(Value::as_str) == Some("Forced"),
            download_limit: (p.download_limit > 0).then_some(p.download_limit),
            upload_limit: (p.upload_limit > 0).then_some(p.upload_limit),
            ratio_limit: Some(p.ratio_limit),
            seeding_time_limit: Some(p.seeding_time_limit),
            inactive_seeding_time_limit: Some(p.inactive_seeding_time_limit),
            share_limit_action: self
                .params_extra
                .get("share_limit_action")
                .and_then(Value::as_str)
                .and_then(share_limit_action_from_str)
                .filter(|a| *a != ShareLimitAction::Default),
            ..Default::default()
        }
    }

    // ---- DTO ----------------------------------------------------------------

    pub fn to_dto(&self) -> RssAutoDownloadRule {
        RssAutoDownloadRule {
            name: self.name.clone(),
            enabled: self.enabled,
            priority: self.priority,
            use_regex: self.use_regex,
            must_contain: self.must_contain(),
            must_not_contain: self.must_not_contain(),
            episode_filter: self.episode_filter.clone(),
            affected_feeds: self.feed_urls.clone(),
            last_match: self.last_match.map(format_rfc2822).unwrap_or_default(),
            ignore_days: self.ignore_days,
            smart_filter: self.smart_filter,
            previously_matched_episodes: self.previously_matched.clone(),
            torrent_params: self.params.clone(),
        }
    }

    /// Builds a rule from the GUI record. `previous` (the stored rule of the
    /// same name) supplies `torrentParams` keys the record cannot express.
    pub fn from_dto(dto: &RssAutoDownloadRule, previous: Option<&Rule>) -> Self {
        let mut r = Rule::new(&dto.name);
        r.enabled = dto.enabled;
        r.priority = dto.priority.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        r.feed_urls = dto.affected_feeds.clone();
        r.last_match = parse_rfc2822(&dto.last_match);
        r.ignore_days = dto.ignore_days.clamp(0, i64::from(i32::MAX));
        r.smart_filter = dto.smart_filter;
        r.previously_matched = dto.previously_matched_episodes.clone();
        r.params = dto.torrent_params.clone();
        if let Some(prev) = previous {
            r.params_extra = prev.params_extra.clone();
        }
        r.set_filters(dto.use_regex, &dto.must_contain, &dto.must_not_contain, &dto.episode_filter);
        r
    }

    // ---- JSON (download_rules.json, rss/rules, export) ----------------------

    /// `AutoDownloadRule::toJsonObject`.
    pub fn to_json(&self) -> Value {
        let p = &self.params;
        let mut m = Map::new();
        m.insert("enabled".into(), self.enabled.into());
        m.insert("priority".into(), self.priority.into());
        m.insert("useRegex".into(), self.use_regex.into());
        m.insert("mustContain".into(), self.must_contain().into());
        m.insert("mustNotContain".into(), self.must_not_contain().into());
        m.insert("episodeFilter".into(), self.episode_filter.clone().into());
        m.insert("affectedFeeds".into(), self.feed_urls.clone().into());
        m.insert("lastMatch".into(), self.last_match.map(format_rfc2822).unwrap_or_default().into());
        m.insert("ignoreDays".into(), self.ignore_days.into());
        m.insert("smartFilter".into(), self.smart_filter.into());
        m.insert("previouslyMatchedEpisodes".into(), self.previously_matched.clone().into());
        // Deprecated mirrors still written by qBittorrent 5.x.
        m.insert("addPaused".into(), p.stopped.map_or(Value::Null, Value::Bool));
        m.insert(
            "torrentContentLayout".into(),
            p.content_layout.map_or(Value::Null, |l| content_layout_str(l).into()),
        );
        m.insert("savePath".into(), p.save_path.clone().into());
        m.insert("assignedCategory".into(), p.category.clone().into());
        m.insert("torrentParams".into(), serialize_params(p, &self.params_extra));
        Value::Object(m)
    }

    /// `AutoDownloadRule::fromJsonObject`.
    pub fn from_json(obj: &Map<String, Value>, name: &str) -> Self {
        let name = if name.is_empty() { obj.get("name").and_then(Value::as_str).unwrap_or_default() } else { name };
        let mut r = Rule::new(name);
        r.enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        r.priority = json_int(obj.get("priority")).unwrap_or(0);
        let use_regex = obj.get("useRegex").and_then(Value::as_bool).unwrap_or(false);
        let text = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        r.last_match = parse_rfc2822(&text("lastMatch"));
        r.ignore_days = json_int(obj.get("ignoreDays")).unwrap_or(0);
        r.smart_filter = obj.get("smartFilter").and_then(Value::as_bool).unwrap_or(false);
        r.feed_urls = string_list(obj.get("affectedFeeds"));
        r.previously_matched = string_list(obj.get("previouslyMatchedEpisodes"));

        match obj.get("torrentParams") {
            Some(v) => {
                let empty = Map::new();
                let (params, extra) = parse_params(v.as_object().unwrap_or(&empty));
                r.params = params;
                r.params_extra = extra;
            }
            None => {
                let mut p = default_template();
                p.save_path = text("savePath");
                p.category = text("assignedCategory");
                p.stopped = obj.get("addPaused").and_then(Value::as_bool);
                if !p.save_path.is_empty() {
                    p.auto_tmm = Some(false);
                }
                p.content_layout = if obj.contains_key("torrentContentLayout") {
                    obj.get("torrentContentLayout").and_then(Value::as_str).filter(|s| !s.is_empty()).map(content_layout_from_str)
                } else {
                    obj.get("createSubfolder")
                        .and_then(Value::as_bool)
                        .map(|b| if b { ContentLayout::Original } else { ContentLayout::NoSubfolder })
                };
                r.params = p;
            }
        }
        r.set_filters(use_regex, &text("mustContain"), &text("mustNotContain"), &text("episodeFilter"));
        r
    }
}

/// `QJsonValue::toInt()`: integral doubles in `int` range only.
fn json_int(v: Option<&Value>) -> Option<i64> {
    let f = v?.as_f64()?;
    (f.fract() == 0.0 && f >= f64::from(i32::MIN) && f <= f64::from(i32::MAX)).then_some(f as i64)
}

/// Array of strings, or a single string.
fn string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().map(|x| x.as_str().unwrap_or_default().to_owned()).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn default_template() -> TorrentParamsTemplate {
    TorrentParamsTemplate::default()
}

fn content_layout_str(l: ContentLayout) -> &'static str {
    match l {
        ContentLayout::Original => "Original",
        ContentLayout::Subfolder => "Subfolder",
        ContentLayout::NoSubfolder => "NoSubfolder",
    }
}

/// `Utils::String::toEnum(str, Original)`.
fn content_layout_from_str(s: &str) -> ContentLayout {
    match s {
        "Subfolder" => ContentLayout::Subfolder,
        "NoSubfolder" => ContentLayout::NoSubfolder,
        _ => ContentLayout::Original,
    }
}

fn stop_condition_str(c: StopCondition) -> &'static str {
    match c {
        StopCondition::None => "None",
        StopCondition::MetadataReceived => "MetadataReceived",
        StopCondition::FilesChecked => "FilesChecked",
    }
}

fn stop_condition_from_str(s: &str) -> StopCondition {
    match s {
        "MetadataReceived" => StopCondition::MetadataReceived,
        "FilesChecked" => StopCondition::FilesChecked,
        _ => StopCondition::None,
    }
}

fn share_limit_action_from_str(s: &str) -> Option<ShareLimitAction> {
    Some(match s {
        "Default" => ShareLimitAction::Default,
        "Stop" => ShareLimitAction::Stop,
        "Remove" => ShareLimitAction::Remove,
        "RemoveWithContent" => ShareLimitAction::RemoveWithContent,
        "EnableSuperSeeding" => ShareLimitAction::EnableSuperSeeding,
        _ => return None,
    })
}

const EXTRA_PARAM_KEYS: [&str; 5] = ["operating_mode", "share_limit_action", "ssl_certificate", "ssl_private_key", "ssl_dh_params"];
const KNOWN_PARAM_KEYS: [&str; 16] = [
    "category",
    "tags",
    "save_path",
    "use_download_path",
    "download_path",
    "add_to_top_of_queue",
    "stopped",
    "stop_condition",
    "skip_checking",
    "content_layout",
    "use_auto_tmm",
    "upload_limit",
    "download_limit",
    "seeding_time_limit",
    "inactive_seeding_time_limit",
    "ratio_limit",
];

/// `BitTorrent::parseAddTorrentParams`.
pub(crate) fn parse_params(obj: &Map<String, Value>) -> (TorrentParamsTemplate, Map<String, Value>) {
    let opt_bool = |k: &str| match obj.get(k) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().unwrap_or(false)),
    };
    let opt_str = |k: &str| match obj.get(k) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().unwrap_or_default().to_owned()),
    };
    let text = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut tags: Vec<String> = Vec::new();
    if let Some(Value::Array(a)) = obj.get("tags") {
        for t in a {
            let t = t.as_str().unwrap_or_default();
            if !t.is_empty() && !tags.iter().any(|x| x == t) {
                tags.push(t.to_owned());
            }
        }
    }
    let params = TorrentParamsTemplate {
        category: text("category"),
        tags,
        save_path: text("save_path"),
        use_download_path: opt_bool("use_download_path"),
        download_path: text("download_path"),
        auto_tmm: opt_bool("use_auto_tmm"),
        stopped: opt_bool("stopped"),
        stop_condition: opt_str("stop_condition").map(|s| stop_condition_from_str(&s)),
        skip_checking: obj.get("skip_checking").and_then(Value::as_bool).unwrap_or(false),
        add_to_queue_top: opt_bool("add_to_top_of_queue"),
        content_layout: opt_str("content_layout").map(|s| content_layout_from_str(&s)),
        upload_limit: json_int(obj.get("upload_limit")).unwrap_or(-1),
        download_limit: json_int(obj.get("download_limit")).unwrap_or(-1),
        ratio_limit: obj.get("ratio_limit").and_then(Value::as_f64).unwrap_or(-2.0),
        seeding_time_limit: json_int(obj.get("seeding_time_limit")).unwrap_or(-2),
        inactive_seeding_time_limit: json_int(obj.get("inactive_seeding_time_limit")).unwrap_or(-2),
    };
    let mut extra = Map::new();
    for (k, v) in obj {
        if KNOWN_PARAM_KEYS.contains(&k.as_str()) {
            continue;
        }
        if extra.len() >= 32 {
            break;
        }
        let v = match k.as_str() {
            "operating_mode" => Value::from(if v.as_str() == Some("Forced") { "Forced" } else { "AutoManaged" }),
            "share_limit_action" => {
                Value::from(v.as_str().filter(|s| share_limit_action_from_str(s).is_some()).unwrap_or("Default"))
            }
            _ => v.clone(),
        };
        extra.insert(k.clone(), v);
    }
    (params, extra)
}

/// `BitTorrent::serializeAddTorrentParams`.
pub(crate) fn serialize_params(p: &TorrentParamsTemplate, extra: &Map<String, Value>) -> Value {
    let mut m = Map::new();
    for (k, v) in extra {
        if !EXTRA_PARAM_KEYS.contains(&k.as_str()) {
            m.insert(k.clone(), v.clone());
        }
    }
    let extra_str = |k: &str, default: &str| -> Value {
        extra.get(k).and_then(Value::as_str).unwrap_or(default).to_owned().into()
    };
    m.insert("category".into(), p.category.clone().into());
    m.insert("tags".into(), p.tags.clone().into());
    m.insert("save_path".into(), p.save_path.clone().into());
    m.insert("download_path".into(), p.download_path.clone().into());
    m.insert("operating_mode".into(), extra_str("operating_mode", "AutoManaged"));
    m.insert("skip_checking".into(), p.skip_checking.into());
    m.insert("upload_limit".into(), p.upload_limit.into());
    m.insert("download_limit".into(), p.download_limit.into());
    m.insert("seeding_time_limit".into(), p.seeding_time_limit.into());
    m.insert("inactive_seeding_time_limit".into(), p.inactive_seeding_time_limit.into());
    m.insert("share_limit_action".into(), extra_str("share_limit_action", "Default"));
    m.insert("ratio_limit".into(), serde_json::Number::from_f64(p.ratio_limit).map_or(Value::from(-2), Value::Number));
    m.insert("ssl_certificate".into(), extra_str("ssl_certificate", ""));
    m.insert("ssl_private_key".into(), extra_str("ssl_private_key", ""));
    m.insert("ssl_dh_params".into(), extra_str("ssl_dh_params", ""));
    if let Some(v) = p.add_to_queue_top {
        m.insert("add_to_top_of_queue".into(), v.into());
    }
    if let Some(v) = p.stopped {
        m.insert("stopped".into(), v.into());
    }
    if let Some(v) = p.stop_condition {
        m.insert("stop_condition".into(), stop_condition_str(v).into());
    }
    if let Some(v) = p.content_layout {
        m.insert("content_layout".into(), content_layout_str(v).into());
    }
    if let Some(v) = p.auto_tmm {
        m.insert("use_auto_tmm".into(), v.into());
    }
    if let Some(v) = p.use_download_path {
        m.insert("use_download_path".into(), v.into());
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MatchContext {
        MatchContext::new(&crate::prefs::RssPrefs::default().rss_smart_episode_filters, true)
    }

    fn rule(use_regex: bool, must: &str, must_not: &str, episode: &str) -> Rule {
        let mut r = Rule::new("r");
        r.set_filters(use_regex, must, must_not, episode);
        r
    }

    fn m(r: &Rule, title: &str) -> bool {
        r.matches(title, 0, &ctx()).is_some()
    }

    #[test]
    fn wildcard_conversion() {
        assert_eq!(wildcard_to_regex("a*b?").unwrap(), "(?s:a.*b.)");
        assert_eq!(wildcard_to_regex("a.b+(c)").unwrap(), r"(?s:a\.b\+\(c\))");
        assert_eq!(wildcard_to_regex("[!a]").unwrap(), "(?s:[^a])");
        assert_eq!(wildcard_to_regex("[a-c]").unwrap(), "(?s:[a-c])");
        assert_eq!(wildcard_to_regex("[]]").unwrap(), r"(?s:[\]])");
        assert_eq!(wildcard_to_regex(r"\d").unwrap(), r"(?s:\\d)");
        assert!(wildcard_to_regex("abc[").is_none());
        assert!(wildcard_to_regex("[").is_none());
        // Every produced pattern compiles.
        for p in ["[&&]", "[~~]", "[--]", "[a-]", "[\\]", "{a,b}", "$^|", "(?i)", "[^]x]", "[[:alpha:]]"] {
            if let Some(re) = wildcard_to_regex(p) {
                assert!(compile_user_regex(&re, false).is_some(), "{p} -> {re}");
            }
        }
    }

    #[test]
    fn wildcard_matching() {
        let r = rule(false, "ubuntu desktop", "", "");
        assert!(m(&r, "Ubuntu 26.04 Desktop amd64"));
        assert!(m(&r, "desktop UBUNTU"), "order irrelevant");
        assert!(!m(&r, "Ubuntu Server"));

        let r = rule(false, "ubuntu*desktop|fedora", "", "");
        assert!(m(&r, "ubuntu-26.04-desktop"));
        assert!(!m(&r, "desktop ubuntu"), "* keeps order");
        assert!(m(&r, "Fedora 44"));

        let r = rule(false, "s0?e0?", "", "");
        assert!(m(&r, "Show S01E02"));
        assert!(!m(&r, "Show S1E2"));

        let r = rule(false, "", "720p|cam", "");
        assert!(m(&r, "Show 1080p"));
        assert!(!m(&r, "Show 720p"));
        assert!(!m(&r, "Show CAM"));

        // Empty alternative: matches everything / excludes everything.
        assert!(m(&rule(false, "nothing|", "", ""), "anything"));
        assert!(!m(&rule(false, "", "nothing|", ""), "anything"));

        // Metacharacters are literal in wildcard mode.
        let r = rule(false, "c++ (2026)", "", "");
        assert!(m(&r, "C++ guide (2026)"));
        assert!(!m(&r, "cc guide 2026"));
        // Unterminated set never matches.
        assert!(!m(&rule(false, "abc[", "", ""), "abc["));
        assert_eq!(rule(false, "a|b", "c|d", "").must_contain(), "a|b");
    }

    #[test]
    fn regex_matching() {
        let r = rule(true, r"^show.*(720|1080)p", "", "");
        assert!(m(&r, "SHOW S01E01 1080p"));
        assert!(!m(&r, "The show 1080p"));
        // In regex mode `|` belongs to the regex.
        let r = rule(true, "foo|bar", "", "");
        assert!(m(&r, "BAR"));
        assert_eq!(r.must_contain(), "foo|bar");
        // Invalid regex never matches (must) and never excludes (must not).
        assert!(!m(&rule(true, "(", "", ""), "("));
        assert!(m(&rule(true, "", "(", ""), "("));
        // Unsupported constructs (backreferences) are invalid.
        assert!(!m(&rule(true, r"(a)\1", "", ""), "aa"));
        // Oversized patterns are rejected.
        assert!(!m(&rule(true, &"a".repeat(MAX_PATTERN_LEN + 1), "", ""), "a"));
        // Huge repetition counts hit the size limit instead of exhausting memory.
        assert!(!m(&rule(true, "(a{1000}){1000}", "", ""), "a"));
    }

    #[test]
    fn episode_filter_grammar() {
        let f = |filter: &str, title: &str| m(&rule(false, "", "", filter), title);
        // Example from the tooltip: 1x2;8-15;5;30-;
        let filter = "1x2;8-15;5;30-;";
        for (title, expected) in [
            ("Show S01E02", true),
            ("Show S01E01", false),
            ("Show S01E05", true),
            ("Show S01E08", true),
            ("Show S01E15", true),
            ("Show S01E16", false),
            ("Show S01E30", true),
            ("Show S01E99", true),
            ("Show S02E01", true),
            ("Show 1x09 HDTV", true),
            ("Show 1x02", true),
            ("Show 1x3", false),
            ("Show s1.e2", true),
            ("Show S01 E05", true),
            ("Show S01_E05", true),
            ("Show S01E050", true),
            ("Show without numbers", false),
        ] {
            assert_eq!(f(filter, title), expected, "{title}");
        }
        // Single numbers need a non-digit after the episode.
        assert!(!f("1x5;", "Show S01E050"));
        assert!(!f("1x5;", "Show S01E51"));
        assert!(f("1x5;", "Show.S01E05.720p"));
        // Leading zeros in the filter and in the title.
        assert!(f("01x005;", "Show S01E05"));
        assert!(f("1x0;", "Show S01E00"));
        assert!(f("1x00;", "Show S01E00"));
        // Ranges are per season.
        assert!(!f("2x1-3;", "Show S01E02"));
        assert!(f("2x1-3;", "Show S02E03"));
        assert!(!f("2x3-1;", "Show S02E02"), "reversed range ignored");
        assert!(!f("2x5-;", "Show S01E09"), "earlier season");
        assert!(f("2x5-;", "Show S03E01"), "later season");
        // Grammar errors reject everything.
        assert!(!f("1x2", "Show S01E02"), "missing ';'");
        assert!(!f("x2;", "Show S01E02"));
        assert!(!f("12345x1;", "Show S12345E01"));
        assert!(f("1X2;", "Show S01E02"), "case-insensitive");
        // Regex metacharacters in a token cannot inject patterns.
        assert!(!f("1x.*;", "Show S01E99"));
        // Range uses the first S..E.. match only.
        assert!(f("1x1-2;", "Show S01E02 S05E07"));
    }

    #[test]
    fn episode_filter_validation() {
        assert!(is_valid_episode_filter(""));
        assert!(is_valid_episode_filter("1x2;8-15;5;30-;"));
        assert!(!is_valid_episode_filter("1x2"));
        assert!(!is_valid_episode_filter("abc"));
    }

    #[test]
    fn smart_filter_and_repacks() {
        let c = ctx();
        assert_eq!(compute_episode_name("Show.S01E05.720p", &c), "1x5");
        assert_eq!(compute_episode_name("Show 1x05", &c), "1x5");
        assert_eq!(compute_episode_name("Show 2017.01.05 HDTV", &c), "2017.01.05");
        assert_eq!(compute_episode_name("Show 05-01-2017", &c), "05-01-2017");
        assert_eq!(compute_episode_name("Show_S02E10_", &c), "2x10");
        assert_eq!(compute_episode_name("Movie 2017", &c), "");

        let mut r = rule(false, "show", "", "");
        r.smart_filter = true;
        assert!(r.accepts("Show S01E05 720p", 1, &c));
        assert_eq!(r.previously_matched, ["1x5"]);
        assert!(!r.accepts("Show S01E05 1080p", 2, &c), "duplicate episode");
        assert!(!r.accepts("Show 2017", 2, &c), "no episode number");
        assert!(r.accepts("Show S01E05 REPACK", 3, &c));
        assert!(!r.accepts("Show S01E05 repack 2", 4, &c));
        assert!(r.accepts("Show S01E05 PROPER", 5, &c));
        assert!(r.accepts("Show S01E06 PROPER REPACK", 6, &c), "new episode");
        assert!(r.accepts("Show S01E06 REPACK PROPER", 7, &c), "repack+proper of a known episode");
        assert!(r.previously_matched.contains(&"1x6-REPACK-PROPER".to_owned()));
        assert!(r.previously_matched.contains(&"1x6-REPACK".to_owned()));
        assert!(!r.accepts("Show S01E06 REPACK", 8, &c));
        assert_eq!(r.last_match, Some(7));

        let no_repacks = MatchContext::new(&crate::prefs::RssPrefs::default().rss_smart_episode_filters, false);
        let mut r = rule(false, "", "", "");
        r.smart_filter = true;
        r.previously_matched = vec!["1x5".into()];
        assert!(!r.accepts("Show S01E05 REPACK", 1, &no_repacks));

        // Invalid smart filter regex: nothing matches.
        let broken = MatchContext::new(&["(".to_owned()], true);
        assert!(r.matches("Show S01E07", 0, &broken).is_none());
        // Extended syntax: whitespace in a filter line is ignored.
        let spaced = MatchContext::new(&[r"s (\d+) e (\d+)".to_owned()], true);
        assert_eq!(compute_episode_name("S03E04", &spaced), "3x4");
    }

    #[test]
    fn ignore_days() {
        let c = ctx();
        let mut r = rule(false, "show", "", "");
        r.ignore_days = 2;
        assert!(r.accepts("show a", 10 * MS_PER_DAY, &c));
        assert!(r.matches("show b", 11 * MS_PER_DAY, &c).is_none());
        assert!(r.matches("show b", 12 * MS_PER_DAY - 1, &c).is_none());
        assert!(r.matches("show b", 12 * MS_PER_DAY, &c).is_some());
        r.ignore_days = 0;
        assert!(r.matches("show b", 11 * MS_PER_DAY, &c).is_some());
    }

    #[test]
    fn json_round_trip() {
        let mut r = rule(false, "a|b", "c", "1x2;");
        r.name = "Rule".into();
        r.priority = 3;
        r.feed_urls = vec!["https://a/rss".into()];
        r.last_match = Some(1_791_000_306_000);
        r.ignore_days = 4;
        r.smart_filter = true;
        r.previously_matched = vec!["1x1".into()];
        r.params.category = "TV".into();
        r.params.stopped = Some(true);
        r.params.content_layout = Some(ContentLayout::Subfolder);
        r.params_extra.insert("share_limit_action".into(), "Stop".into());
        r.params_extra.insert("operating_mode".into(), "Forced".into());
        let v = r.to_json();
        assert_eq!(v["lastMatch"], "03 Oct 2026 04:05:06 +0000");
        assert_eq!(v["addPaused"], true);
        assert_eq!(v["torrentContentLayout"], "Subfolder");
        assert_eq!(v["assignedCategory"], "TV");
        assert_eq!(v["torrentParams"]["operating_mode"], "Forced");
        assert_eq!(v["torrentParams"]["share_limit_action"], "Stop");
        assert_eq!(v["torrentParams"]["ratio_limit"], -2.0);
        assert!(v["torrentParams"].get("use_auto_tmm").is_none());
        assert!(v["torrentParams"].get("stop_condition").is_none());

        let back = Rule::from_json(v.as_object().unwrap(), "Rule");
        assert_eq!(back.to_json(), v);
        let params = back.add_torrent_params();
        assert_eq!(params.category.as_deref(), Some("TV"));
        assert_eq!(params.stopped, Some(true));
        assert!(params.forced);
        assert_eq!(params.share_limit_action, Some(ShareLimitAction::Stop));
        assert_eq!(params.save_path, None);
        assert_eq!(params.upload_limit, None);
        assert_eq!(params.ratio_limit, Some(-2.0));

        // DTO round trip keeps the extra params of the stored rule.
        let dto = back.to_dto();
        assert_eq!(dto.must_contain, "a|b");
        let again = Rule::from_dto(&dto, Some(&back));
        assert_eq!(again.to_json(), v);
    }

    #[test]
    fn json_defaults_and_deprecated_keys() {
        let obj = serde_json::json!({
            "affectedFeeds": "https://single/rss",
            "previouslyMatchedEpisodes": "1x1",
            "savePath": "/downloads/tv",
            "assignedCategory": "TV",
            "addPaused": false,
            "createSubfolder": false,
            "priority": 1.5,
            "useRegex": "yes",
        });
        let r = Rule::from_json(obj.as_object().unwrap(), "x");
        assert!(r.enabled, "enabled defaults to true");
        assert_eq!(r.priority, 0, "non-integral priority");
        assert!(!r.use_regex);
        assert_eq!(r.feed_urls, ["https://single/rss"]);
        assert_eq!(r.previously_matched, ["1x1"]);
        assert_eq!(r.params.save_path, "/downloads/tv");
        assert_eq!(r.params.auto_tmm, Some(false));
        assert_eq!(r.params.stopped, Some(false));
        assert_eq!(r.params.content_layout, Some(ContentLayout::NoSubfolder));
        assert_eq!(r.params.upload_limit, -1);
        assert_eq!(r.last_match, None);

        let obj = serde_json::json!({"torrentContentLayout": "Bogus", "torrentParams": {"stop_condition": "Nope", "tags": ["a", "a", "", 5]}});
        let r = Rule::from_json(obj.as_object().unwrap(), "y");
        assert_eq!(r.params.stop_condition, Some(StopCondition::None));
        assert_eq!(r.params.tags, ["a"]);
        assert_eq!(r.params.content_layout, None, "torrentParams wins over deprecated keys");

        let empty = Rule::from_json(&Map::new(), "");
        assert_eq!(empty.name, "");
        assert!(empty.enabled);
        assert_eq!(empty.params.seeding_time_limit, -2);
    }
}
