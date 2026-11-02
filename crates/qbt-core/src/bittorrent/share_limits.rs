//! Share limit enforcement (`SessionImpl::processTorrentShareLimits`) as a
//! pure decision.

use crate::model::{ShareLimitAction, ShareLimitsMode};

#[derive(Clone, Copy, Debug)]
pub struct ShareInputs {
    pub finished: bool,
    pub forced: bool,
    pub stopped: bool,
    pub super_seeding: bool,
    /// `realRatio` (may be infinite).
    pub ratio: f64,
    /// Finished (seeding) time in seconds.
    pub seeding_secs: i64,
    /// Seconds, -1 = never active.
    pub time_since_activity: i64,
    /// Effective limits after category/global resolution; -1 = none.
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub inactive_seeding_time_limit: i64,
    /// Effective mode; `Default` is treated as `MatchAny`.
    pub mode: ShareLimitsMode,
    /// Effective action (never `Default`).
    pub action: ShareLimitAction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShareDecision {
    pub action: ShareLimitAction,
    /// e.g. "Torrent reached the share ratio limit."
    pub description: &'static str,
    /// e.g. "Torrent stopped."
    pub action_text: &'static str,
}

pub fn evaluate(i: &ShareInputs) -> Option<ShareDecision> {
    if !i.finished || i.forced {
        return None;
    }
    // If all share limits are disabled, there is nothing to check.
    if !any_limit_set(i.ratio_limit, i.seeding_time_limit, i.inactive_seeding_time_limit) {
        return None;
    }
    let ratio_set = i.ratio_limit >= 0.0;
    let seeding_set = i.seeding_time_limit >= 0;
    let inactive_set = i.inactive_seeding_time_limit >= 0;
    let ratio_reached = i.ratio >= i.ratio_limit;
    let seeding_reached = i.seeding_secs / 60 >= i.seeding_time_limit;
    let inactive_reached = i.time_since_activity / 60 >= i.inactive_seeding_time_limit;
    let description = if i.mode == ShareLimitsMode::MatchAll {
        // Every enabled limit must be reached.
        if (ratio_set && !ratio_reached) || (seeding_set && !seeding_reached) || (inactive_set && !inactive_reached) {
            return None;
        }
        "Torrent reached the share limit(s)."
    } else if ratio_set && ratio_reached {
        "Torrent reached the share ratio limit."
    } else if seeding_set && seeding_reached {
        "Torrent reached the seeding time limit."
    } else if inactive_set && inactive_reached {
        "Torrent reached the inactive seeding time limit."
    } else {
        return None;
    };
    let action = if i.action == ShareLimitAction::Default { ShareLimitAction::Stop } else { i.action };
    let action_text = match action {
        ShareLimitAction::Remove => "Removing torrent.",
        ShareLimitAction::RemoveWithContent => "Removing torrent and deleting its content.",
        ShareLimitAction::Stop | ShareLimitAction::Default if !i.stopped => "Torrent stopped.",
        ShareLimitAction::EnableSuperSeeding if !i.stopped && !i.super_seeding => "Super seeding enabled.",
        _ => return None,
    };
    Some(ShareDecision { action, description, action_text })
}

/// True when any limit could ever trigger (decides whether the 10 s
/// evaluation is worth running).
pub fn any_limit_set(ratio_limit: f64, seeding_time_limit: i64, inactive_seeding_time_limit: i64) -> bool {
    ratio_limit >= 0.0 || seeding_time_limit >= 0 || inactive_seeding_time_limit >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ShareInputs {
        ShareInputs {
            finished: true,
            forced: false,
            stopped: false,
            super_seeding: false,
            ratio: 0.5,
            seeding_secs: 0,
            time_since_activity: 0,
            ratio_limit: -1.0,
            seeding_time_limit: -1,
            inactive_seeding_time_limit: -1,
            mode: ShareLimitsMode::MatchAny,
            action: ShareLimitAction::Stop,
        }
    }

    #[test]
    fn no_limits_no_action() {
        assert_eq!(evaluate(&inputs()), None);
    }

    #[test]
    fn ratio_limit_first() {
        let mut i = inputs();
        i.ratio_limit = 0.5;
        i.seeding_time_limit = 0;
        let d = evaluate(&i).unwrap();
        assert_eq!(d.description, "Torrent reached the share ratio limit.");
        assert_eq!(d.action_text, "Torrent stopped.");
        i.ratio = f64::INFINITY;
        assert!(evaluate(&i).is_some());
    }

    #[test]
    fn seeding_and_inactive_limits() {
        let mut i = inputs();
        i.seeding_time_limit = 10;
        i.seeding_secs = 599;
        assert_eq!(evaluate(&i), None);
        i.seeding_secs = 600;
        assert_eq!(evaluate(&i).unwrap().description, "Torrent reached the seeding time limit.");
        let mut j = inputs();
        j.inactive_seeding_time_limit = 0;
        j.time_since_activity = -1;
        assert_eq!(evaluate(&j).unwrap().description, "Torrent reached the inactive seeding time limit.");
    }

    #[test]
    fn skips_unfinished_forced_and_redundant_actions() {
        let mut i = inputs();
        i.ratio_limit = 0.1;
        i.finished = false;
        assert_eq!(evaluate(&i), None);
        i.finished = true;
        i.forced = true;
        assert_eq!(evaluate(&i), None);
        i.forced = false;
        i.stopped = true;
        assert_eq!(evaluate(&i), None);
        i.action = ShareLimitAction::Remove;
        assert_eq!(evaluate(&i).unwrap().action, ShareLimitAction::Remove);
        i.action = ShareLimitAction::RemoveWithContent;
        assert_eq!(evaluate(&i).unwrap().action_text, "Removing torrent and deleting its content.");
        i.action = ShareLimitAction::EnableSuperSeeding;
        assert_eq!(evaluate(&i), None);
        i.stopped = false;
        assert_eq!(evaluate(&i).unwrap().action, ShareLimitAction::EnableSuperSeeding);
        i.super_seeding = true;
        assert_eq!(evaluate(&i), None);
    }

    #[test]
    fn match_all_needs_every_enabled_limit() {
        let mut i = inputs();
        i.mode = ShareLimitsMode::MatchAll;
        assert_eq!(evaluate(&i), None, "all limits disabled");
        i.ratio_limit = 0.5;
        i.seeding_time_limit = 10;
        i.seeding_secs = 599;
        // Ratio reached, seeding time not yet.
        assert_eq!(evaluate(&i), None);
        i.mode = ShareLimitsMode::Default;
        assert_eq!(evaluate(&i).unwrap().description, "Torrent reached the share ratio limit.", "Default = MatchAny");
        i.mode = ShareLimitsMode::MatchAll;
        i.seeding_secs = 600;
        let d = evaluate(&i).unwrap();
        assert_eq!(d.description, "Torrent reached the share limit(s).");
        assert_eq!(d.action_text, "Torrent stopped.");
        // The disabled inactive limit is ignored; enabling it blocks again.
        i.inactive_seeding_time_limit = 5;
        i.time_since_activity = 299;
        assert_eq!(evaluate(&i), None);
        i.time_since_activity = 300;
        assert!(evaluate(&i).is_some());
        // Ratio falls short.
        i.ratio = 0.4;
        assert_eq!(evaluate(&i), None);
        // A single enabled limit behaves like MatchAny but with the generic text.
        let mut j = inputs();
        j.mode = ShareLimitsMode::MatchAll;
        j.seeding_time_limit = 0;
        assert_eq!(evaluate(&j).unwrap().description, "Torrent reached the share limit(s).");
        j.forced = true;
        assert_eq!(evaluate(&j), None);
    }
}
