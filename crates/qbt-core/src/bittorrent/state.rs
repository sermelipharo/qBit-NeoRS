//! Pure torrent state logic: `TorrentImpl::updateState`, status filter
//! buckets (`TorrentFilter::matchStatus`) and the derived values (ratio,
//! popularity, ETA, time since activity).

use crate::model::{MAX_ETA, MAX_RATIO, StatusFilter, TorrentState};

/// Everything `updateState` looks at.
#[derive(Clone, Copy, Debug, Default)]
pub struct StateInputs {
    /// Restored from resume data, not yet handed to the engine.
    pub checking_resume_data: bool,
    pub moving: bool,
    pub missing_files: bool,
    pub error: bool,
    pub has_metadata: bool,
    pub stopped: bool,
    /// Queueing enabled, not stopped and held back by the queue.
    pub queued: bool,
    /// Forced operating mode.
    pub forced: bool,
    /// The engine is hash-checking the data.
    pub checking: bool,
    /// All wanted pieces are present.
    pub finished: bool,
    /// The torrent completed at some point (seed status).
    pub has_finished_status: bool,
    pub dl_rate: i64,
    pub ul_rate: i64,
}

/// `TorrentImpl::updateState`, first match wins.
pub fn compute_state(i: &StateInputs) -> TorrentState {
    use TorrentState::*;
    let forced = i.forced && !i.stopped;
    let queued = i.queued && !i.stopped;
    if i.checking_resume_data {
        return CheckingResumeData;
    }
    if i.moving {
        return Moving;
    }
    if i.missing_files {
        return MissingFiles;
    }
    if i.error {
        return Error;
    }
    if !i.has_metadata {
        return if i.stopped {
            StoppedDownloading
        } else if queued {
            QueuedDownloading
        } else if forced {
            ForcedDownloadingMetadata
        } else {
            DownloadingMetadata
        };
    }
    if i.checking && !i.stopped {
        return if i.has_finished_status { CheckingUploading } else { CheckingDownloading };
    }
    if i.finished {
        if i.stopped {
            StoppedUploading
        } else if queued {
            QueuedUploading
        } else if forced {
            ForcedUploading
        } else if i.ul_rate > 0 {
            Uploading
        } else {
            StalledUploading
        }
    } else if i.stopped {
        StoppedDownloading
    } else if queued {
        QueuedDownloading
    } else if forced {
        ForcedDownloading
    } else if i.dl_rate > 0 {
        Downloading
    } else {
        StalledDownloading
    }
}

/// `TorrentFilter::matchStatus`.
pub fn matches_status(filter: StatusFilter, state: TorrentState, stopped: bool, dl_rate: i64, ul_rate: i64) -> bool {
    use TorrentState as S;
    let active = dl_rate > 0 || ul_rate > 0;
    match filter {
        StatusFilter::All => true,
        StatusFilter::Downloading => state.is_downloading(),
        StatusFilter::Seeding => state.is_uploading(),
        StatusFilter::Completed => state.is_completed(),
        StatusFilter::Running => !stopped,
        StatusFilter::Stopped => stopped,
        StatusFilter::Active => active,
        StatusFilter::Inactive => !active,
        StatusFilter::Stalled => matches!(state, S::StalledUploading | S::StalledDownloading),
        StatusFilter::StalledUploading => state == S::StalledUploading,
        StatusFilter::StalledDownloading => state == S::StalledDownloading,
        StatusFilter::Checking => matches!(state, S::CheckingUploading | S::CheckingDownloading | S::CheckingResumeData),
        StatusFilter::Moving => state == S::Moving,
        StatusFilter::Errored => matches!(state, S::MissingFiles | S::Error),
    }
}

/// `realRatio`; `f64::INFINITY` when downloaded is 0 but uploaded is not.
pub fn real_ratio(all_time_upload: i64, all_time_download: i64, total_done: i64) -> f64 {
    let download = if (all_time_download as f64) < (total_done as f64 * 0.01) { total_done } else { all_time_download };
    if download <= 0 {
        return if all_time_upload <= 0 { 0.0 } else { f64::INFINITY };
    }
    all_time_upload.max(0) as f64 / download as f64
}

/// Ratio as reported by the API: -1 for "infinite".
pub fn api_ratio(r: f64) -> f64 {
    if !r.is_finite() || r >= MAX_RATIO { -1.0 } else { r }
}

/// Average month as `std::chrono::months` (2629746 s).
const MONTH_SECS: f64 = 2_629_746.0;

/// `popularity`: ratio / active months.
pub fn popularity(ratio: f64, active_secs: i64) -> f64 {
    let months = active_secs as f64 / MONTH_SECS;
    if months > 0.0 { ratio / months } else { 0.0 }
}

/// `timeSinceActivity` from the two partial values (-1 = unknown).
pub fn time_since_activity(since_upload: i64, since_download: i64) -> i64 {
    if (since_upload < 0) != (since_download < 0) {
        since_upload.max(since_download)
    } else {
        since_upload.min(since_download)
    }
}

pub struct EtaInputs {
    pub stopped: bool,
    pub finished: bool,
    pub avg_dl: f64,
    pub avg_ul: f64,
    pub wanted_size: i64,
    pub completed_size: i64,
    pub all_time_download: i64,
    pub all_time_upload: i64,
    /// Effective limits (-1 = none).
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub inactive_seeding_time_limit: i64,
    pub seeding_time: i64,
    pub time_since_activity: i64,
}

/// `TorrentImpl::eta`.
pub fn eta(i: &EtaInputs) -> i64 {
    if i.stopped {
        return MAX_ETA;
    }
    if i.finished {
        if i.ratio_limit < 0.0 && i.seeding_time_limit < 0 && i.inactive_seeding_time_limit < 0 {
            return MAX_ETA;
        }
        let mut ratio_eta = MAX_ETA;
        if i.avg_ul > 0.0 && i.ratio_limit >= 0.0 {
            let real_dl = if i.all_time_download <= 0 { i.wanted_size } else { i.all_time_download };
            ratio_eta = (((real_dl as f64) * i.ratio_limit - i.all_time_upload as f64) / i.avg_ul) as i64;
        }
        let mut seeding_eta = MAX_ETA;
        if i.seeding_time_limit >= 0 {
            seeding_eta = (i.seeding_time_limit * 60 - i.seeding_time).max(0);
        }
        let mut inactive_eta = MAX_ETA;
        if i.inactive_seeding_time_limit >= 0 {
            inactive_eta = (i.inactive_seeding_time_limit * 60 - i.time_since_activity).max(0);
        }
        return ratio_eta.min(seeding_eta).min(inactive_eta);
    }
    if i.avg_dl <= 0.0 {
        return MAX_ETA;
    }
    (((i.wanted_size - i.completed_size).max(0) as f64) / i.avg_dl) as i64
}

/// Rolling average used for the ETA (`SpeedMonitor`, 30 samples). A
/// cumulative mean for the first samples, then an exponential average with
/// the same horizon; two numbers per torrent instead of a sample buffer.
#[derive(Clone, Copy, Debug, Default)]
pub struct RateAverage {
    pub dl: f64,
    pub ul: f64,
    n: u32,
}

impl RateAverage {
    const WINDOW: u32 = 30;

    pub fn push(&mut self, dl: i64, ul: i64) {
        self.n = (self.n + 1).min(Self::WINDOW);
        let k = 1.0 / self.n as f64;
        self.dl += (dl as f64 - self.dl) * k;
        self.ul += (ul as f64 - self.ul) * k;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TorrentState as S;

    fn base() -> StateInputs {
        StateInputs { has_metadata: true, ..Default::default() }
    }

    #[test]
    fn state_priority_order() {
        let mut i = base();
        i.checking_resume_data = true;
        i.moving = true;
        assert_eq!(compute_state(&i), S::CheckingResumeData);
        i.checking_resume_data = false;
        assert_eq!(compute_state(&i), S::Moving);
        i.moving = false;
        i.missing_files = true;
        i.error = true;
        assert_eq!(compute_state(&i), S::MissingFiles);
        i.missing_files = false;
        assert_eq!(compute_state(&i), S::Error);
    }

    #[test]
    fn metadata_states() {
        let mut i = StateInputs::default();
        assert_eq!(compute_state(&i), S::DownloadingMetadata);
        i.forced = true;
        assert_eq!(compute_state(&i), S::ForcedDownloadingMetadata);
        i.queued = true;
        assert_eq!(compute_state(&i), S::QueuedDownloading);
        i.stopped = true;
        assert_eq!(compute_state(&i), S::StoppedDownloading);
    }

    #[test]
    fn checking_and_transfer_states() {
        let mut i = base();
        i.checking = true;
        assert_eq!(compute_state(&i), S::CheckingDownloading);
        i.has_finished_status = true;
        assert_eq!(compute_state(&i), S::CheckingUploading);
        i.stopped = true;
        assert_eq!(compute_state(&i), S::StoppedDownloading);
        i.checking = false;
        i.finished = true;
        assert_eq!(compute_state(&i), S::StoppedUploading);
        i.stopped = false;
        assert_eq!(compute_state(&i), S::StalledUploading);
        i.ul_rate = 5;
        assert_eq!(compute_state(&i), S::Uploading);
        i.forced = true;
        assert_eq!(compute_state(&i), S::ForcedUploading);
        i.queued = true;
        assert_eq!(compute_state(&i), S::QueuedUploading);
        let mut d = base();
        assert_eq!(compute_state(&d), S::StalledDownloading);
        d.dl_rate = 1;
        assert_eq!(compute_state(&d), S::Downloading);
        d.forced = true;
        assert_eq!(compute_state(&d), S::ForcedDownloading);
        d.queued = true;
        assert_eq!(compute_state(&d), S::QueuedDownloading);
    }

    #[test]
    fn status_filters() {
        assert!(matches_status(StatusFilter::Downloading, S::StoppedDownloading, true, 0, 0));
        assert!(!matches_status(StatusFilter::Seeding, S::StoppedUploading, true, 0, 0));
        assert!(matches_status(StatusFilter::Completed, S::StoppedUploading, true, 0, 0));
        assert!(matches_status(StatusFilter::Running, S::StalledUploading, false, 0, 0));
        assert!(matches_status(StatusFilter::Stopped, S::StoppedUploading, true, 0, 0));
        assert!(matches_status(StatusFilter::Active, S::Uploading, false, 0, 10));
        assert!(matches_status(StatusFilter::Inactive, S::StalledUploading, false, 0, 0));
        assert!(matches_status(StatusFilter::Stalled, S::StalledDownloading, false, 0, 0));
        assert!(matches_status(StatusFilter::StalledUploading, S::StalledUploading, false, 0, 0));
        assert!(!matches_status(StatusFilter::StalledDownloading, S::StalledUploading, false, 0, 0));
        assert!(matches_status(StatusFilter::Checking, S::CheckingResumeData, false, 0, 0));
        assert!(matches_status(StatusFilter::Moving, S::Moving, false, 0, 0));
        assert!(matches_status(StatusFilter::Errored, S::MissingFiles, false, 0, 0));
        for s in [S::CheckingResumeData, S::Moving, S::MissingFiles, S::Error] {
            assert!(!matches_status(StatusFilter::Downloading, s, false, 0, 0));
            assert!(!matches_status(StatusFilter::Seeding, s, false, 0, 0));
            assert!(!matches_status(StatusFilter::Completed, s, false, 0, 0));
        }
    }

    #[test]
    fn ratio_and_popularity() {
        assert_eq!(real_ratio(0, 0, 0), 0.0);
        assert!(real_ratio(10, 0, 0).is_infinite());
        assert_eq!(real_ratio(50, 100, 100), 0.5);
        // Seeder that lost its stats: total_done is used.
        assert_eq!(real_ratio(200, 0, 100), 2.0);
        assert_eq!(api_ratio(f64::INFINITY), -1.0);
        assert_eq!(api_ratio(10000.0), -1.0);
        assert_eq!(api_ratio(1.5), 1.5);
        assert_eq!(popularity(2.0, 0), 0.0);
        assert!((popularity(2.0, 2_629_746) - 2.0).abs() < 1e-9);
        assert_eq!(time_since_activity(-1, 30), 30);
        assert_eq!(time_since_activity(10, 30), 10);
        assert_eq!(time_since_activity(-1, -1), -1);
    }

    fn eta_inputs() -> EtaInputs {
        EtaInputs {
            stopped: false,
            finished: false,
            avg_dl: 0.0,
            avg_ul: 0.0,
            wanted_size: 1000,
            completed_size: 400,
            all_time_download: 400,
            all_time_upload: 0,
            ratio_limit: -1.0,
            seeding_time_limit: -1,
            inactive_seeding_time_limit: -1,
            seeding_time: 0,
            time_since_activity: -1,
        }
    }

    #[test]
    fn eta_rules() {
        let mut i = eta_inputs();
        assert_eq!(eta(&i), MAX_ETA);
        i.avg_dl = 100.0;
        assert_eq!(eta(&i), 6);
        i.stopped = true;
        assert_eq!(eta(&i), MAX_ETA);
        i.stopped = false;
        i.finished = true;
        assert_eq!(eta(&i), MAX_ETA);
        i.seeding_time_limit = 10;
        i.seeding_time = 300;
        assert_eq!(eta(&i), 300);
        i.ratio_limit = 1.0;
        i.avg_ul = 10.0;
        i.all_time_download = 1000;
        i.all_time_upload = 900;
        assert_eq!(eta(&i), 10);
        i.inactive_seeding_time_limit = 1;
        i.time_since_activity = 120;
        assert_eq!(eta(&i), 0);
    }

    #[test]
    fn rate_average() {
        let mut a = RateAverage::default();
        a.push(100, 10);
        a.push(300, 30);
        assert_eq!(a.dl, 200.0);
        assert_eq!(a.ul, 20.0);
        for _ in 0..1000 {
            a.push(0, 0);
        }
        assert!(a.dl < 1.0);
    }
}
