//! Session-wide tracker settings and the shared tracker client.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use librqbit_core::hash_id::Id20;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracker_comms::{
    AnnounceSettings, HttpClientFactory, HttpClientOptions, NetPolicy, TrackerClient,
    UdpTrackerClient,
};

use crate::net::external_ip::{ExternalIpVoter, IpVoteSource};

/// Port reported to trackers when nothing is listening (trackers reject 0).
const FALLBACK_ANNOUNCE_PORT: u16 = 4240;

/// Tracker settings. All of them can be changed at runtime with
/// [`crate::Session::set_tracker_settings`]; changes apply to the next
/// announce of every torrent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackerSettings {
    /// Address reported to trackers as `ip=` (not sent in anonymous mode).
    pub announce_ip: Option<IpAddr>,
    /// Port reported to trackers; 0 = the listen port.
    pub announce_port: u16,
    /// Maximum concurrent HTTP(S) announces and scrapes; 0 = unlimited.
    pub max_concurrent_http_announces: usize,
    pub validate_https_tracker_certificate: bool,
    /// libtorrent `ssrf_mitigation` (see `tracker_comms::NetPolicy`).
    pub ssrf_mitigation: bool,
    /// How long to wait for `stopped` announces when a torrent stops;
    /// zero = don't send them.
    pub stop_tracker_timeout: Duration,
    pub announce_to_all_trackers: bool,
    pub announce_to_all_tiers: bool,
    /// libtorrent `anonymous_mode`: no `User-Agent` (except for private
    /// torrents) and no `ip=` parameter.
    pub anonymous_mode: bool,
    /// `User-Agent` for HTTP trackers; `None` = the session's client name.
    pub user_agent: Option<String>,
    /// Peers requested per announce (`numwant`).
    pub num_want: u32,
    /// Lower bound for announce intervals returned by trackers.
    pub min_announce_interval: Duration,
    /// Timeout of one announce or scrape.
    pub announce_timeout: Duration,
}

impl Default for TrackerSettings {
    fn default() -> Self {
        Self {
            announce_ip: None,
            announce_port: 0,
            max_concurrent_http_announces: 50,
            validate_https_tracker_certificate: true,
            ssrf_mitigation: true,
            stop_tracker_timeout: Duration::from_secs(5),
            announce_to_all_trackers: false,
            announce_to_all_tiers: true,
            anonymous_mode: false,
            user_agent: None,
            num_want: 200,
            min_announce_interval: Duration::from_secs(300),
            announce_timeout: Duration::from_secs(30),
        }
    }
}

/// Counts running announcers so that session shutdown can wait for their
/// `stopped` announces.
#[derive(Default)]
pub(crate) struct ActiveAnnouncers {
    count: AtomicUsize,
    idle: Notify,
}

pub(crate) struct ActiveGuard<'a>(&'a ActiveAnnouncers);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl ActiveAnnouncers {
    pub fn enter(&self) -> ActiveGuard<'_> {
        self.count.fetch_add(1, Ordering::AcqRel);
        ActiveGuard(self)
    }

    /// Wait until no announcer is running, or the deadline passes.
    pub async fn wait_idle(&self, deadline: Instant) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.count.load(Ordering::Acquire) == 0 {
                return;
            }
            if tokio::time::timeout_at(deadline.into(), notified)
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

pub(crate) struct SessionTrackers {
    pub client: TrackerClient,
    settings: RwLock<TrackerSettings>,
    // Both change at runtime (anonymous mode, listen port changes).
    peer_id: RwLock<Id20>,
    listen_port: RwLock<Option<u16>>,
    pub ipv4_only: bool,
    pub client_name: String,
    /// Disables announcing for all torrents.
    pub disabled: bool,
    pub active: ActiveAnnouncers,
    pub cancel: CancellationToken,
    /// Trackers report our public address (BEP 24) to the session voter.
    external_ip: Arc<ExternalIpVoter>,
}

pub(crate) struct SessionTrackersArgs {
    pub settings: TrackerSettings,
    pub udp: UdpTrackerClient,
    pub http_factory: HttpClientFactory,
    pub proxied: bool,
    pub peer_id: Id20,
    pub listen_port: Option<u16>,
    pub ipv4_only: bool,
    pub client_name: String,
    pub disabled: bool,
    pub cancel: CancellationToken,
    pub external_ip: Arc<ExternalIpVoter>,
}

fn http_options(s: &TrackerSettings, ipv4_only: bool, proxied: bool) -> HttpClientOptions {
    HttpClientOptions {
        validate_https_certificates: s.validate_https_tracker_certificate,
        ipv4_only,
        proxied,
    }
}

impl SessionTrackers {
    pub fn new(args: SessionTrackersArgs) -> anyhow::Result<Self> {
        let s = args.settings;
        let policy = Arc::new(NetPolicy::new(s.ssrf_mitigation));
        let client = TrackerClient::new(
            args.udp,
            policy,
            Some(args.http_factory),
            http_options(&s, args.ipv4_only, args.proxied),
            s.max_concurrent_http_announces,
        )?;
        Ok(Self {
            client,
            settings: RwLock::new(s),
            peer_id: RwLock::new(args.peer_id),
            listen_port: RwLock::new(args.listen_port),
            ipv4_only: args.ipv4_only,
            client_name: args.client_name,
            disabled: args.disabled,
            active: Default::default(),
            cancel: args.cancel,
            external_ip: args.external_ip,
        })
    }

    /// The tracker at `url` saw us at `ip` (its `external ip` key).
    pub fn record_observed_ip(&self, url: &str, ip: IpAddr) {
        let parsed = url::Url::parse(url).ok();
        let host = parsed.as_ref().and_then(|u| u.host_str()).unwrap_or(url);
        self.external_ip.cast_vote(ip, IpVoteSource::Tracker(host));
    }

    pub fn settings(&self) -> TrackerSettings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, s: TrackerSettings) -> anyhow::Result<()> {
        let proxied = self.client.http_options().proxied;
        self.client
            .set_http_options(http_options(&s, self.ipv4_only, proxied))?;
        self.client
            .set_max_concurrent_http_announces(s.max_concurrent_http_announces);
        self.client.policy().set_ssrf_mitigation(s.ssrf_mitigation);
        *self.settings.write() = s;
        Ok(())
    }

    pub fn announce_port(&self, s: &TrackerSettings) -> u16 {
        if s.announce_port != 0 {
            return s.announce_port;
        }
        self.listen_port.read().unwrap_or(FALLBACK_ANNOUNCE_PORT)
    }

    pub fn peer_id(&self) -> Id20 {
        *self.peer_id.read()
    }

    pub fn set_peer_id(&self, id: Id20) {
        *self.peer_id.write() = id;
    }

    pub fn set_listen_port(&self, port: Option<u16>) {
        *self.listen_port.write() = port;
    }

    /// Mark HTTP trackers as going through a proxy (or not) and skip UDP
    /// trackers while they do. The HTTP client is rebuilt so that the
    /// factory picks up the current proxy.
    pub fn set_proxied(&self, proxied: bool) -> anyhow::Result<()> {
        let mut opts = self.client.http_options();
        opts.proxied = proxied;
        self.client.set_udp_disabled(proxied);
        if self.client.http_options() == opts {
            self.client.rebuild_http()
        } else {
            self.client.set_http_options(opts)
        }
    }

    pub fn user_agent(&self, s: &TrackerSettings, private: bool) -> Option<String> {
        if s.anonymous_mode && !private {
            return None;
        }
        Some(
            s.user_agent
                .clone()
                .unwrap_or_else(|| self.client_name.clone()),
        )
    }

    pub fn announce_ip(&self, s: &TrackerSettings) -> Option<IpAddr> {
        if s.anonymous_mode {
            None
        } else {
            s.announce_ip
        }
    }

    pub fn announce_settings(
        &self,
        s: &TrackerSettings,
        force_interval: Option<Duration>,
    ) -> AnnounceSettings {
        AnnounceSettings {
            announce_to_all_trackers: s.announce_to_all_trackers,
            announce_to_all_tiers: s.announce_to_all_tiers,
            min_announce_interval: s.min_announce_interval,
            force_interval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn active_announcers_wait() {
        let a = Arc::new(ActiveAnnouncers::default());
        a.wait_idle(Instant::now() + Duration::from_secs(1)).await;
        let a2 = a.clone();
        let h = tokio::spawn(async move {
            let _g = a2.enter();
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let start = Instant::now();
        a.wait_idle(Instant::now() + Duration::from_secs(5)).await;
        assert!(start.elapsed() < Duration::from_secs(2));
        h.await.unwrap();
        // Deadline is honoured while busy.
        let _g = a.enter();
        let start = Instant::now();
        a.wait_idle(Instant::now() + Duration::from_millis(50))
            .await;
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
