//! Share limit enforcement (`SessionImpl::processTorrentShareLimits`) as a
//! pure decision.

use crate::model::ShareLimitAction;

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
    let description = if i.ratio_limit >= 0.0 && i.ratio >= i.ratio_limit {
        "Torrent reached the share ratio limit."
    } else if i.seeding_time_limit >= 0 && i.seeding_secs / 60 >= i.seeding_time_limit {
        "Torrent reached the seeding time limit."
    } else if i.inactive_seeding_time_limit >= 0 && i.time_since_activity / 60 >= i.inactive_seeding_time_limit {
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
}
