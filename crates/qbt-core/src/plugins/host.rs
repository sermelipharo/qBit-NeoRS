//! What plugins can see and change: a narrow interface the worker uses
//! instead of the session itself (tests use a fake).

use std::sync::{Arc, Weak};

use crate::bittorrent::Session;
use crate::context::CoreContext;
use crate::model::{ShareLimitAction, ShareLimitsMode, TorrentInfo, TrackerEntry};

/// `BitTorrent::ShareLimits`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShareLimitsValue {
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub inactive_seeding_time_limit: i64,
    pub mode: ShareLimitsMode,
    pub action: ShareLimitAction,
}

impl Default for ShareLimitsValue {
    /// `DEFAULT_RATIO_LIMIT`, `DEFAULT_SEEDING_TIME_LIMIT`, `Default` mode and action.
    fn default() -> Self {
        Self {
            ratio_limit: -2.0,
            seeding_time_limit: -2,
            inactive_seeding_time_limit: -2,
            mode: ShareLimitsMode::Default,
            action: ShareLimitAction::Default,
        }
    }
}

/// Changes a plugin can make to a torrent (the torrent object's setters
/// and methods).
#[derive(Clone, Debug, PartialEq)]
pub enum TorrentAction {
    Rename(String),
    SetComment(String),
    SetSavePath(String),
    SetDownloadPath(String),
    SetCategory(String),
    AddTag(String),
    RemoveTag(String),
    ClearTags,
    SetShareLimits(ShareLimitsValue),
    Start,
    Stop,
}

pub trait PluginHost: Send + Sync + 'static {
    fn torrent_info(&self, id: &str) -> Option<TorrentInfo>;
    /// (path, size) of every file; empty without metadata.
    fn files(&self, id: &str) -> Vec<(String, i64)>;
    /// Real trackers (no DHT/PeX/LSD rows).
    fn trackers(&self, id: &str) -> Vec<TrackerEntry>;
    fn url_seeds(&self, id: &str) -> Vec<String>;
    /// Applies a change; `false` when the torrent is gone or the change was
    /// refused (unknown category, invalid tag ...). Slow changes (moving
    /// files, starting) are started in the background.
    fn apply(&self, id: &str, action: TorrentAction) -> bool;
    /// The global share limit action and mode (for `effectiveShareLimits`).
    fn global_share_limits(&self) -> (ShareLimitsMode, ShareLimitAction);
    /// Sends an e-mail with the notification settings, in the background.
    fn send_mail(&self, plugin_id: &str, subject: String, body: String);
}

/// The real host: the session of the running app.
pub struct SessionHost {
    session: Weak<Session>,
    ctx: CoreContext,
}

impl SessionHost {
    pub fn new(session: &Arc<Session>, ctx: &CoreContext) -> Arc<Self> {
        Arc::new(Self { session: Arc::downgrade(session), ctx: ctx.clone() })
    }

    fn session(&self) -> Option<Arc<Session>> {
        self.session.upgrade()
    }
}

impl PluginHost for SessionHost {
    fn torrent_info(&self, id: &str) -> Option<TorrentInfo> {
        self.session()?.torrent_info(id).ok()
    }

    fn files(&self, id: &str) -> Vec<(String, i64)> {
        let Some(s) = self.session() else { return Vec::new() };
        s.files(id, &[]).map(|f| f.into_iter().map(|f| (f.name, f.size)).collect()).unwrap_or_default()
    }

    fn trackers(&self, id: &str) -> Vec<TrackerEntry> {
        let Some(s) = self.session() else { return Vec::new() };
        s.trackers(id).map(|t| t.into_iter().filter(|t| t.tier >= 0 && !t.url.starts_with("** [")).collect()).unwrap_or_default()
    }

    fn url_seeds(&self, id: &str) -> Vec<String> {
        let Some(s) = self.session() else { return Vec::new() };
        s.web_seeds(id).map(|w| w.into_iter().map(|w| w.url).collect()).unwrap_or_default()
    }

    fn apply(&self, id: &str, action: TorrentAction) -> bool {
        let Some(s) = self.session() else { return false };
        let ids = [id.to_string()];
        match action {
            TorrentAction::Rename(name) => s.rename(id, &name).is_ok(),
            TorrentAction::SetComment(c) => s.set_comment(&ids, &c).is_ok(),
            TorrentAction::SetCategory(c) => s.set_category(&ids, &c).is_ok(),
            TorrentAction::AddTag(t) => s.add_tags(&ids, &[t]).is_ok(),
            TorrentAction::RemoveTag(t) => !t.trim().is_empty() && s.remove_tags(&ids, &[t]).is_ok(),
            TorrentAction::ClearTags => s.remove_tags(&ids, &[]).is_ok(),
            TorrentAction::SetShareLimits(l) => s
                .set_share_limits(&ids, l.ratio_limit, l.seeding_time_limit, l.inactive_seeding_time_limit, l.mode, l.action)
                .is_ok(),
            TorrentAction::SetSavePath(p) => {
                self.ctx.rt.spawn(async move {
                    let _ = s.set_save_path(&ids, &p).await;
                });
                true
            }
            TorrentAction::SetDownloadPath(p) => {
                self.ctx.rt.spawn(async move {
                    let _ = s.set_download_path(&ids, &p).await;
                });
                true
            }
            TorrentAction::Start => {
                self.ctx.rt.spawn(async move {
                    let _ = s.start(&ids).await;
                });
                true
            }
            TorrentAction::Stop => {
                self.ctx.rt.spawn(async move {
                    let _ = s.stop(&ids).await;
                });
                true
            }
        }
    }

    fn global_share_limits(&self) -> (ShareLimitsMode, ShareLimitAction) {
        let p = self.ctx.prefs.current();
        (p.bittorrent.share_limits_mode, p.bittorrent.max_ratio_act)
    }

    fn send_mail(&self, plugin_id: &str, subject: String, body: String) {
        let ctx = self.ctx.clone();
        ctx.logger.info(format!("Plugin '{plugin_id}' is sending an e-mail. Subject: \"{subject}\""));
        let task_ctx = ctx.clone();
        ctx.rt.spawn(async move {
            let _ = crate::net::smtp::send_mail(&task_ctx, &subject, &body).await;
        });
    }
}
