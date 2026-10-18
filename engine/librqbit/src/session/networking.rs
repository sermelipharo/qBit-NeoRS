//! engine-net: runtime reconfiguration of the session's networking
//! (listeners, interface, DHT/LSD, encryption, proxy, anonymous mode, socket
//! options, connection speed, PeX, IP filter, port mapping).

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::Context;
use dht::{Dht, DhtBuilder, DhtConfig, DhtPersistenceConfig, PersistentDht, dht_listen_addr};
use futures::{StreamExt, stream::BoxStream};
use librqbit_core::{hash_id::Id20, spawn_utils::spawn_with_cancel};
use librqbit_lsd::{LocalServiceDiscovery, LocalServiceDiscoveryOptions};
use librqbit_utp::BindDevice;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, debug_span, info, warn};
use tracker_comms::IpFilterFn;

use super::{DhtSessionConfig, Session};
use crate::{
    ListenerOptions, ManagedTorrent, PeerSource,
    net::{
        BlockedPeerCallback, EncryptionPolicy, MappingProtocol, NetSettings, PortMappingConfig,
        PortMappingStatus, ProxyConfig, ProxySettings, reqwest_proxy,
    },
};

/// Current listeners.
#[derive(Default)]
pub(crate) struct ListenState {
    pub opts: Option<ListenerOptions>,
    pub addr: Option<SocketAddr>,
    pub announce_port: Option<u16>,
    pub tcp: bool,
    pub utp: bool,
    token: Option<CancellationToken>,
}

/// A service (DHT) that can be stopped and started again with the same
/// configuration.
pub(crate) struct ServiceCtl<C> {
    pub config: Option<C>,
    pub token: Option<CancellationToken>,
}

/// What is needed to start the DHT again.
#[derive(Clone)]
pub(crate) struct DhtRestartConfig {
    bootstrap_addrs: Option<Vec<String>>,
    port: Option<u16>,
    persistence: Option<(Option<Duration>, Option<PathBuf>)>,
}

impl DhtRestartConfig {
    pub fn from_session_config(c: DhtSessionConfig) -> Self {
        Self {
            bootstrap_addrs: c.bootstrap_addrs,
            port: c.port,
            persistence: c.persistence.map(|p| (p.dump_interval, p.config_filename)),
        }
    }
}

pub(crate) async fn start_dht(
    cfg: &DhtRestartConfig,
    ipv4_only: bool,
    bind_device: Option<&BindDevice>,
    token: CancellationToken,
) -> anyhow::Result<Dht> {
    let cfg = cfg.clone();
    match cfg.persistence {
        Some((dump_interval, config_filename)) => PersistentDht::create(
            DhtPersistenceConfig {
                dump_interval,
                config_filename,
            },
            cfg.port,
            ipv4_only,
            cfg.bootstrap_addrs,
            Some(token),
            bind_device,
        )
        .await
        .context("error initializing persistent DHT"),
        None => {
            let listen_addr = dht_listen_addr(cfg.port, None, ipv4_only);
            DhtBuilder::with_config(DhtConfig {
                bootstrap_addrs: cfg.bootstrap_addrs,
                cancellation_token: Some(token),
                bind_device,
                listen_addr: Some(listen_addr),
                ..Default::default()
            })
            .await
            .context("error initializing DHT")
        }
    }
}

pub(crate) async fn start_lsd(
    bind_device: Option<&BindDevice>,
    token: CancellationToken,
) -> Option<LocalServiceDiscovery> {
    LocalServiceDiscovery::new(LocalServiceDiscoveryOptions {
        cancel_token: token,
        bind_device,
        ..Default::default()
    })
    .await
    .inspect_err(|e| warn!("error starting local service discovery: {e:#}"))
    .ok()
}

/// Proxy and interface for HTTP clients (trackers, torrent downloads).
pub(crate) struct HttpEnv {
    proxy: RwLock<Option<ProxyConfig>>,
    interface: RwLock<Option<String>>,
}

impl HttpEnv {
    pub fn new(proxy: Option<ProxyConfig>, interface: Option<String>) -> Self {
        Self {
            proxy: RwLock::new(proxy),
            interface: RwLock::new(interface),
        }
    }

    pub fn set_proxy(&self, proxy: Option<ProxyConfig>) {
        *self.proxy.write() = proxy;
    }

    pub fn set_interface(&self, interface: Option<String>) {
        *self.interface.write() = interface;
    }

    pub fn interface(&self) -> Option<String> {
        self.interface.read().clone()
    }

    pub fn apply(&self, b: reqwest::ClientBuilder) -> anyhow::Result<reqwest::ClientBuilder> {
        if let Some(p) = self.proxy.read().as_ref() {
            return Ok(b.proxy(reqwest_proxy(p)?));
        }
        #[cfg(not(windows))]
        if let Some(i) = self.interface.read().as_ref() {
            return Ok(b.interface(i));
        }
        Ok(b)
    }

    pub fn builder(&self) -> anyhow::Result<reqwest::ClientBuilder> {
        self.apply(reqwest::Client::builder())
    }
}

/// Listener start attempts while the previous listener's socket is being
/// released (same port).
const LISTEN_ATTEMPTS: u32 = 10;

impl Session {
    /// The peer id used for new connections and announces: a generic one in
    /// anonymous mode.
    pub(crate) fn effective_peer_id(&self) -> Id20 {
        if self.connector.settings().anonymous_mode {
            self.anon_peer_id
        } else {
            self.peer_id
        }
    }

    /// Whether `id` is one of our peer ids.
    pub(crate) fn is_own_peer_id(&self, id: Id20) -> bool {
        id == self.peer_id || id == self.anon_peer_id
    }

    // ---------------------------------------------------------------- listen

    /// The current listener options (`None` when not listening).
    pub fn listen_options(&self) -> Option<ListenerOptions> {
        self.listen.read().opts.clone()
    }

    /// Rebind the listeners (address, port, TCP/uTP mode) without restarting
    /// the session; `None` stops listening. Existing connections stay up
    /// (uTP connections of the old socket die with it). If binding the new
    /// address fails the old listeners are restored and the error returned.
    /// Announces (trackers, DHT, LSD) and port mappings follow the new port.
    pub async fn set_listen_options(
        self: &Arc<Self>,
        opts: Option<ListenerOptions>,
    ) -> anyhow::Result<()> {
        let _g = self.listen_lock.lock().await;
        let old = self.stop_listeners();
        if let Some(o) = &opts {
            let was = old.as_ref().is_some_and(|o| o.enable_upnp_port_forwarding);
            if o.enable_upnp_port_forwarding != was {
                let mut cfg = self.port_mapper.config();
                cfg.upnp = o.enable_upnp_port_forwarding;
                cfg.natpmp = o.enable_upnp_port_forwarding;
                self.port_mapper.set_config(cfg);
            }
        }
        let result = match opts {
            None => Ok(()),
            Some(opts) => match self.start_listeners(opts).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    if let Some(old) = old
                        && let Err(e2) = self.start_listeners(old).await
                    {
                        warn!("error restoring the previous listeners: {e2:#}");
                    }
                    Err(e)
                }
            },
        };
        self.after_listen_change();
        result
    }

    /// Change only the listen port (0 = random), keeping the other options.
    pub async fn set_listen_port(self: &Arc<Self>, port: u16) -> anyhow::Result<()> {
        let mut opts = self.listen_options().unwrap_or_default();
        opts.listen_addr.set_port(port);
        self.set_listen_options(Some(opts)).await
    }

    /// Change only the address to bind to (unspecified = all addresses).
    pub async fn set_listen_ip(self: &Arc<Self>, ip: IpAddr) -> anyhow::Result<()> {
        let mut opts = self.listen_options().unwrap_or_default();
        opts.listen_addr.set_ip(ip);
        self.set_listen_options(Some(opts)).await
    }

    fn stop_listeners(&self) -> Option<ListenerOptions> {
        let mut st = self.listen.write();
        if let Some(t) = st.token.take() {
            t.cancel();
        }
        st.addr = None;
        st.announce_port = None;
        st.tcp = false;
        st.utp = false;
        self.connector.set_utp_socket(None);
        st.opts.take()
    }

    async fn start_listeners(self: &Arc<Self>, opts: ListenerOptions) -> anyhow::Result<()> {
        let bind_device = self.bind_device.read().clone();
        let mut attempt = 0;
        let (mut res, token) = loop {
            let token = self.cancellation_token.child_token();
            match opts
                .clone()
                .start(self.rs(), token.clone(), bind_device.as_ref())
                .await
            {
                Ok(r) => break (r, token),
                Err(e) => {
                    token.cancel();
                    attempt += 1;
                    if attempt >= LISTEN_ATTEMPTS {
                        return Err(e);
                    }
                    debug!("error starting listeners (attempt {attempt}): {e:#}");
                    // The previous socket on this port may not be closed yet.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        let max_pending = res.max_pending_incoming_handshake_checks;
        let (tcp, utp) = (res.tcp_socket.is_some(), res.utp_socket.is_some());
        if let Some(l) = res.tcp_socket.take() {
            let this = self.clone();
            spawn_with_cancel(
                debug_span!(parent: self.rs(), "tcp_listen", addr = ?res.addr),
                "tcp_listen",
                token.clone(),
                async move { this.task_listener(l, max_pending).await },
            );
        }
        if let Some(l) = res.utp_socket.clone() {
            let this = self.clone();
            spawn_with_cancel(
                debug_span!(parent: self.rs(), "utp_listen", addr = ?res.addr),
                "utp_listen",
                token.clone(),
                async move { this.task_listener(l, max_pending).await },
            );
        }
        self.connector.set_utp_socket(res.utp_socket.take());
        let mut st = self.listen.write();
        st.opts = Some(opts);
        st.addr = Some(res.addr);
        st.announce_port = res.announce_port;
        st.tcp = tcp;
        st.utp = utp;
        st.token = Some(token);
        Ok(())
    }

    fn after_listen_change(&self) {
        let (port, announce_port, tcp, utp) = {
            let st = self.listen.read();
            (st.addr.map(|a| a.port()), st.announce_port, st.tcp, st.utp)
        };
        self.tracker_ctx.set_listen_port(announce_port);
        self.connector.set_advertised_listen_port(announce_port);
        let mut ports = Vec::new();
        if let Some(p) = announce_port.or(port) {
            if tcp {
                ports.push((MappingProtocol::Tcp, p));
            }
            if utp {
                ports.push((MappingProtocol::Udp, p));
            }
        }
        self.port_mapper.set_listen_ports(ports);
        self.bump_discovery();
        // Trackers learn the new port with the next announce; make it now.
        self.reannounce_all();
    }

    /// Bind peer connections, listeners, DHT, LSD, port mapping and HTTP
    /// clients to a network interface (`None` = any). The UDP tracker socket
    /// keeps the interface it was created with.
    pub async fn set_network_interface(
        self: &Arc<Self>,
        interface: Option<String>,
    ) -> anyhow::Result<()> {
        let bd = match interface.as_deref() {
            Some(name) => Some(
                BindDevice::new_from_name(name)
                    .with_context(|| format!("error creating bind device {name}"))?,
            ),
            None => None,
        };
        *self.bind_device.write() = bd.clone();
        self.connector.set_bind_device(bd.clone());
        self.http_env.set_interface(interface);
        self.tracker_ctx.client.rebuild_http()?;
        self.rebuild_webseed_client()?;
        self.port_mapper.set_bind_device(bd);
        let opts = self.listen_options();
        if opts.is_some() {
            self.set_listen_options(opts).await?;
        }
        if self.dht_enabled() {
            self.set_dht_enabled(false).await?;
            self.set_dht_enabled(true).await?;
        }
        if self.lsd_enabled() {
            self.set_lsd_enabled(false).await;
            self.set_lsd_enabled(true).await;
        }
        Ok(())
    }

    // ------------------------------------------------------------- DHT / LSD

    fn bump_discovery(&self) {
        self.discovery_gen.send_modify(|g| *g = g.wrapping_add(1));
    }

    pub fn dht_enabled(&self) -> bool {
        self.dht.load().is_some()
    }

    /// Start or stop the DHT. Restarting reuses the configuration the session
    /// was created with (`SessionOptions::dht`, defaults if it was `None`);
    /// with persistence the routing table and port are kept. Running
    /// torrents start/stop using it right away.
    pub async fn set_dht_enabled(self: &Arc<Self>, enabled: bool) -> anyhow::Result<()> {
        let _g = self.listen_lock.lock().await;
        if enabled == self.dht_enabled() {
            return Ok(());
        }
        if enabled {
            let cfg = self.dht_ctl.lock().config.clone().unwrap_or_else(|| {
                DhtRestartConfig::from_session_config(DhtSessionConfig::default())
            });
            let token = self.cancellation_token.child_token();
            let bd = self.bind_device.read().clone();
            let dht = start_dht(&cfg, self.ipv4_only, bd.as_ref(), token.clone()).await?;
            {
                let mut ctl = self.dht_ctl.lock();
                ctl.config = Some(cfg);
                ctl.token = Some(token);
            }
            self.dht.store(Some(dht));
            info!("DHT enabled");
        } else {
            self.dht.store(None);
            if let Some(t) = self.dht_ctl.lock().token.take() {
                t.cancel();
            }
            info!("DHT disabled");
        }
        self.bump_discovery();
        Ok(())
    }

    pub fn lsd_enabled(&self) -> bool {
        self.lsd.load().is_some()
    }

    /// Start or stop local service discovery. Returns whether LSD is running
    /// afterwards (starting can fail, e.g. without multicast).
    pub async fn set_lsd_enabled(self: &Arc<Self>, enabled: bool) -> bool {
        let _g = self.listen_lock.lock().await;
        if enabled == self.lsd_enabled() {
            return enabled;
        }
        if enabled {
            let token = self.cancellation_token.child_token();
            let bd = self.bind_device.read().clone();
            match start_lsd(bd.as_ref(), token.clone()).await {
                Some(lsd) => {
                    *self.lsd_token.lock() = Some(token);
                    self.lsd.store(Some(Arc::new(lsd)));
                }
                None => return false,
            }
        } else {
            self.lsd.store(None);
            if let Some(t) = self.lsd_token.lock().take() {
                t.cancel();
            }
        }
        self.bump_discovery();
        enabled
    }

    /// DHT or LSD peers for a torrent. The stream follows the session (DHT/LSD
    /// switched, new announce port) and the torrent's own switch
    /// (`disabled`, see [`crate::DisabledDiscovery`]).
    pub(crate) fn discovery_stream(
        self: &Arc<Self>,
        info_hash: Id20,
        announce: bool,
        source: PeerSource,
        disabled: tokio::sync::watch::Receiver<crate::DisabledDiscovery>,
    ) -> BoxStream<'static, (SocketAddr, PeerSource)> {
        let session = Arc::downgrade(self);
        switching_discovery_stream(
            self.discovery_gen.subscribe(),
            disabled,
            source,
            move || {
                let s = session.upgrade()?;
                let port = if announce { s.announce_port() } else { None };
                Some(match source {
                    PeerSource::Dht => s.get_dht().map(|d| d.get_peers(info_hash, port).boxed()),
                    _ => s.lsd.load_full().map(|l| l.announce(info_hash, port).boxed()),
                })
            },
        )
    }

    // --------------------------------------------------------- net settings

    /// Current networking settings.
    pub fn net_settings(&self) -> NetSettings {
        (*self.connector.settings()).clone()
    }

    /// Apply networking settings at runtime. New values are used by the next
    /// connection attempt; existing connections are kept (PeX switches
    /// apply to existing connections too). Changing the proxy rebuilds the
    /// tracker and web seed HTTP clients; anonymous mode also switches the
    /// tracker side (`TrackerSettings::anonymous_mode`) and the peer id.
    pub fn set_net_settings(&self, s: NetSettings) -> anyhow::Result<()> {
        s.validate()?;
        let old = self.connector.settings();
        // Everything that can fail happens before anything changes: a proxy
        // that can't be used must never leave trackers or web seeds on a
        // direct connection while peers go through the proxy.
        let webseed_clients = if old.peer_proxy() != s.peer_proxy()
            || old.anonymous_mode != s.anonymous_mode
        {
            Some(self.build_webseed_clients(&s)?)
        } else {
            None
        };
        if old.tracker_proxy() != s.tracker_proxy() {
            self.http_env.set_proxy(s.tracker_proxy().cloned());
            if let Err(e) = self.tracker_ctx.set_proxied(s.tracker_proxy().is_some()) {
                self.http_env.set_proxy(old.tracker_proxy().cloned());
                let _ = self.tracker_ctx.set_proxied(old.tracker_proxy().is_some());
                return Err(e);
            }
        }
        self.connector.set_settings(s.clone());
        self.connect_limiter.set_rate(s.connection_speed);
        if let Some(c) = webseed_clients {
            self.webseed_http.store(Arc::new(c));
        }
        if old.anonymous_mode != s.anonymous_mode {
            let mut ts = self.tracker_ctx.settings();
            ts.anonymous_mode = s.anonymous_mode;
            self.set_tracker_settings(ts)?;
            self.tracker_ctx.set_peer_id(self.effective_peer_id());
        }
        Ok(())
    }

    fn update_net(&self, f: impl FnOnce(&mut NetSettings)) -> anyhow::Result<()> {
        let mut s = self.net_settings();
        f(&mut s);
        self.set_net_settings(s)
    }

    /// Protocol encryption for new connections (incoming and outgoing).
    pub fn set_encryption_policy(&self, policy: EncryptionPolicy) -> anyhow::Result<()> {
        self.update_net(|s| s.encryption = policy)
    }

    /// The proxy for peers and/or trackers (`None` = direct connections).
    pub fn set_proxy(&self, proxy: Option<ProxySettings>) -> anyhow::Result<()> {
        self.update_net(|s| s.proxy = proxy)
    }

    pub fn set_anonymous_mode(&self, enabled: bool) -> anyhow::Result<()> {
        self.update_net(|s| s.anonymous_mode = enabled)
    }

    /// Peer exchange on/off: stops advertising, sending and accepting ut_pex.
    pub fn set_pex_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        self.update_net(|s| s.pex = enabled)
    }

    /// Outgoing connections over TCP and/or uTP. The listeners are not
    /// changed (use `set_listen_options` with a `ListenerMode`).
    pub fn set_outgoing_protocols(&self, tcp: bool, utp: bool) -> anyhow::Result<()> {
        self.update_net(|s| {
            s.enable_tcp = tcp;
            s.enable_utp = utp;
        })
    }

    pub fn set_outgoing_port_range(&self, range: Option<(u16, u16)>) -> anyhow::Result<()> {
        self.update_net(|s| s.outgoing_ports = range)
    }

    /// IP ToS byte of peer TCP sockets (DSCP << 2); 0 = OS default.
    pub fn set_peer_tos(&self, tos: u8) -> anyhow::Result<()> {
        self.update_net(|s| s.peer_tos = tos)
    }

    /// Socket buffer sizes in bytes of peer TCP sockets (0 = OS default).
    pub fn set_socket_buffer_sizes(&self, send: u32, recv: u32) -> anyhow::Result<()> {
        self.update_net(|s| {
            s.send_buffer_size = send;
            s.recv_buffer_size = recv;
        })
    }

    /// Outgoing connection attempts per second (0 = unlimited).
    pub fn set_connection_speed(&self, per_second: u32) -> anyhow::Result<()> {
        self.update_net(|s| s.connection_speed = per_second)
    }

    pub(crate) fn pex_enabled(&self) -> bool {
        self.connector.settings().pex
    }

    fn build_webseed_clients(&self, s: &NetSettings) -> anyhow::Result<super::WebSeedClients> {
        super::WebSeedClients::build(
            s.peer_proxy(),
            self.http_env.interface().as_deref(),
            (!s.anonymous_mode).then_some(self.client_name_and_version.as_str()),
        )
    }

    pub(crate) fn rebuild_webseed_client(&self) -> anyhow::Result<()> {
        let clients = self.build_webseed_clients(&self.connector.settings())?;
        self.webseed_http.store(Arc::new(clients));
        Ok(())
    }

    // ------------------------------------------------------------ IP filter

    /// Set (or clear) the peer IP filter: `true` = blocked. Connected peers
    /// that match are disconnected right away; blocked attempts are reported
    /// to the blocked-peer callback.
    pub fn set_peer_ip_filter(&self, filter: Option<IpFilterFn>) {
        self.peer_filter.set_filter(filter);
        self.disconnect_blocked_peers();
    }

    /// Replace the banned addresses; matching peers are disconnected.
    pub fn set_banned_ips(&self, ips: Vec<IpAddr>) {
        self.peer_filter.set_banned(ips);
        self.disconnect_blocked_peers();
    }

    /// Ban one address (qBittorrent "Ban peer permanently") and disconnect it.
    pub fn ban_peer_ip(&self, ip: IpAddr) {
        self.peer_filter.ban(ip);
        self.disconnect_blocked_peers();
    }

    pub fn unban_peer_ip(&self, ip: IpAddr) -> bool {
        self.peer_filter.unban(ip)
    }

    pub fn banned_ips(&self) -> Vec<IpAddr> {
        self.peer_filter.banned()
    }

    /// Called for every refused or closed connection because of the IP
    /// filter, a ban or I2P mixed mode (the "Blocked IPs" log).
    pub fn set_blocked_peer_callback(&self, cb: Option<BlockedPeerCallback>) {
        self.peer_filter.set_callback(cb);
    }

    /// Disconnects connected peers the filter now blocks. Returns how many.
    pub fn disconnect_blocked_peers(&self) -> usize {
        let lives: Vec<_> =
            self.with_torrents(|ts| ts.filter_map(|(_, t)| t.live()).collect::<Vec<_>>());
        lives
            .iter()
            .map(|l| l.disconnect_blocked_peers(&self.peer_filter))
            .sum()
    }

    /// Incoming connections are refused while peers go through a proxy and
    /// when the filter blocks the address.
    pub(crate) fn accept_incoming_allowed(&self, addr: SocketAddr) -> bool {
        if self.connector.settings().peer_proxy().is_some() && !crate::i2p::is_i2p_addr(addr) {
            debug!(
                ?addr,
                "refusing incoming connection: peers go through a proxy"
            );
            return false;
        }
        if self.peer_filter.check_and_report(addr, true) {
            self.stats
                .counters
                .blocked_incoming
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    // --------------------------------------------------------- port mapping

    pub fn port_mapping_config(&self) -> PortMappingConfig {
        self.port_mapper.config()
    }

    /// UPnP / NAT-PMP on/off (mappings are renewed while on and removed from
    /// NAT-PMP gateways when switched off).
    pub fn set_port_mapping(&self, config: PortMappingConfig) {
        self.port_mapper.set_config(config);
    }

    /// Also map `port` (e.g. the WebUI or the embedded tracker). Returns
    /// false if it was already requested.
    pub fn add_port_mapping(&self, protocol: MappingProtocol, port: u16) -> bool {
        self.port_mapper.add_port(protocol, port)
    }

    pub fn remove_port_mapping(&self, protocol: MappingProtocol, port: u16) -> bool {
        self.port_mapper.remove_port(protocol, port)
    }

    /// Our public IPv4 and IPv6 address as last reported by trackers
    /// (the `external ip` response key, BEP 24).
    pub fn tracker_external_ips(&self) -> (Option<IpAddr>, Option<IpAddr>) {
        self.tracker_ctx.observed_ips()
    }

    /// Requested ports, confirmed mappings and the external address.
    pub fn port_mapping_status(&self) -> PortMappingStatus {
        self.port_mapper.status()
    }

    // ------------------------------------------------------------------ I2P

    /// Whether a torrent may talk to `addr` (I2P mixed mode).
    pub(crate) fn i2p_mixed_allows(&self, torrent: &ManagedTorrent, addr: SocketAddr) -> bool {
        self.i2p.mixed_allows(torrent, addr)
    }
}

/// A discovery stream that (re)creates its inner peer stream with `make_inner`
/// whenever the session generation changes or the torrent's switch for
/// `source` flips. `make_inner` returns `None` once the session is gone (the
/// stream ends) and `Some(None)` when the source is unavailable (off in the
/// session). While the torrent has the source disabled nothing is created.
fn switching_discovery_stream(
    mut gen_rx: tokio::sync::watch::Receiver<u64>,
    mut disabled: tokio::sync::watch::Receiver<crate::DisabledDiscovery>,
    source: PeerSource,
    mut make_inner: impl FnMut() -> Option<Option<BoxStream<'static, SocketAddr>>> + Send + 'static,
) -> BoxStream<'static, (SocketAddr, PeerSource)> {
    enum Ev {
        Peer(SocketAddr),
        InnerEnded,
        Changed,
        Closed,
    }
    async_stream::stream! {
        loop {
            gen_rx.borrow_and_update();
            let off = disabled.borrow_and_update().blocks(source);
            let inner = if off {
                None
            } else {
                let Some(inner) = make_inner() else { return };
                inner
            };
            let mut changed = false;
            if let Some(mut inner) = inner {
                loop {
                    let ev = tokio::select! {
                        a = inner.next() => match a {
                            Some(a) => Ev::Peer(a),
                            None => Ev::InnerEnded,
                        },
                        r = gen_rx.changed() => if r.is_ok() { Ev::Changed } else { Ev::Closed },
                        r = disabled.changed() => if r.is_ok() { Ev::Changed } else { Ev::Closed },
                    };
                    match ev {
                        Ev::Peer(a) => yield (a, source),
                        Ev::InnerEnded => break,
                        Ev::Changed => {
                            changed = true;
                            break;
                        }
                        Ev::Closed => return,
                    }
                }
            }
            if !changed {
                let r = tokio::select! {
                    r = gen_rx.changed() => r,
                    r = disabled.changed() => r,
                };
                if r.is_err() {
                    return;
                }
            }
        }
    }
    .boxed()
}

#[cfg(test)]
mod discovery_tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::DisabledDiscovery;

    async fn next(
        s: &mut BoxStream<'static, (SocketAddr, PeerSource)>,
    ) -> Result<Option<(SocketAddr, PeerSource)>, tokio::time::error::Elapsed> {
        tokio::time::timeout(Duration::from_millis(200), s.next()).await
    }

    #[tokio::test]
    async fn torrent_switch_stops_and_restarts_discovery() {
        let gen_tx = tokio::sync::watch::Sender::new(0u64);
        let flags = tokio::sync::watch::Sender::new(DisabledDiscovery {
            dht: true,
            ..Default::default()
        });
        let created = Arc::new(AtomicUsize::new(0));
        let c = created.clone();
        let addr: SocketAddr = "1.2.3.4:5".parse().unwrap();
        // Each inner stream yields one peer and then stays open.
        let mut s = switching_discovery_stream(
            gen_tx.subscribe(),
            flags.subscribe(),
            PeerSource::Dht,
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                Some(Some(
                    futures::stream::iter([addr])
                        .chain(futures::stream::pending())
                        .boxed(),
                ))
            },
        );

        // Disabled for the torrent: no lookup at all.
        assert!(next(&mut s).await.is_err());
        assert_eq!(created.load(Ordering::SeqCst), 0);

        // Another source's switch doesn't matter.
        flags.send_replace(DisabledDiscovery {
            dht: true,
            pex: true,
            lsd: true,
        });
        assert!(next(&mut s).await.is_err());
        assert_eq!(created.load(Ordering::SeqCst), 0);

        // Enabled: the lookup starts and yields.
        flags.send_replace(DisabledDiscovery::default());
        assert_eq!(next(&mut s).await.unwrap(), Some((addr, PeerSource::Dht)));
        assert_eq!(created.load(Ordering::SeqCst), 1);

        // Disabled again: the running lookup is dropped.
        flags.send_replace(DisabledDiscovery {
            dht: true,
            ..Default::default()
        });
        assert!(next(&mut s).await.is_err());
        // A session change while disabled doesn't start one either.
        gen_tx.send_replace(1);
        assert!(next(&mut s).await.is_err());
        assert_eq!(created.load(Ordering::SeqCst), 1);

        // Back on.
        flags.send_replace(DisabledDiscovery::default());
        assert_eq!(next(&mut s).await.unwrap(), Some((addr, PeerSource::Dht)));
        assert_eq!(created.load(Ordering::SeqCst), 2);

        // The torrent is gone: the stream ends.
        drop(flags);
        assert_eq!(next(&mut s).await.unwrap(), None);
    }

    #[tokio::test]
    async fn ends_when_the_session_is_gone() {
        let gen_tx = tokio::sync::watch::Sender::new(0u64);
        let flags = tokio::sync::watch::Sender::new(DisabledDiscovery::default());
        let mut s =
            switching_discovery_stream(gen_tx.subscribe(), flags.subscribe(), PeerSource::Lsd, || {
                None
            });
        assert_eq!(s.next().await, None);
    }
}
