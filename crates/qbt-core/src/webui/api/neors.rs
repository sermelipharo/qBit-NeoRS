//! `neors/*`: qBit NeoRS extensions for the bundled Web UI. Not part of the
//! qBittorrent WebAPI; the standard endpoints stay unchanged.
//!
//! `neors/nameRules[?since=<version>]` returns the torrent name rules the
//! macOS app uses:
//!
//! ```json
//! {"version": "…", "available": true,
//!  "options": {"enabled": true, "semiboldTitles": true, "cardTags": true, "yearAsTag": true},
//!  "rules": [{"pattern": "…", "category": "resolution", "template": "", "caseSensitive": false}]}
//! ```
//!
//! With `since` equal to the current version only `{"version", "unchanged":
//! true}` is sent. Without rules from the app (the daemon, or before the app
//! pushed them) `available` is false and `rules` empty.

use serde_json::json;

use super::{ApiError, ApiResult, Call, Reply};

pub fn dispatch(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "nameRules" => Ok(Reply::Json(name_rules(c))),
        _ => Err(ApiError::endpoint_not_found()),
    }
}

fn name_rules(c: &Call<'_>) -> serde_json::Value {
    let Some(snapshot) = c.app.torrent_name_rules() else {
        return json!({
            "version": "",
            "available": false,
            "options": {"enabled": false, "semiboldTitles": false, "cardTags": false, "yearAsTag": true},
            "rules": [],
        });
    };
    if c.params.str("since") == snapshot.version {
        return json!({"version": snapshot.version, "unchanged": true});
    }
    let r = &snapshot.rules;
    json!({
        "version": snapshot.version,
        "available": true,
        "options": {
            "enabled": r.enabled,
            "semiboldTitles": r.semibold_titles,
            "cardTags": r.card_tags,
            "yearAsTag": r.year_as_tag,
        },
        "rules": r.rules,
    })
}
