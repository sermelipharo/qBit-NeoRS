//! Port mapping of the listen port and extra ports (WebUI, embedded tracker)
//! with UPnP IGD and NAT-PMP, restarted whenever the set of ports or the
//! configuration changes.

use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use librqbit_core::spawn_utils::spawn_with_cancel;
use librqbit_natpmp::Protocol as NatPmpProtocol;
use librqbit_upnp::{UpnpPortForwarder, UpnpPortForwarderOptions, UpnpProtocol};
use librqbit_utp::BindDevice;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, debug_span, info};

use super::external_ip::{ExternalIpVoter, IpVoteSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum MappingProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum MappingTransport {
    NatPmp,
    Upnp,
}

/// One port mapping the gateway confirmed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortMapping {
    pub transport: MappingTransport,
    pub protocol: MappingProtocol,
    pub internal_port: u16,
    pub external_port: u16,
    /// Lease in seconds (renewed at half of it).
    pub lifetime_secs: u32,
}

/// What the port mapper is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PortMappingStatus {
    pub upnp: bool,
    pub natpmp: bool,
    /// Ports we ask to have mapped (listen port TCP/UDP plus extra ports).
    pub requested: Vec<(MappingProtocol, u16)>,
    /// Confirmed mappings (UPnP: last successful request per port).
    pub mappings: Vec<PortMapping>,
    /// External address reported by the gateway (NAT-PMP, or UPnP
    /// `GetExternalIPAddress`), as is: it can be a private address behind
    /// another NAT. [`crate::Session::external_ips`] has the voted address.
    pub external_ip: Option<IpAddr>,
    /// Last NAT-PMP error (no gateway, timeout, refused ...).
    pub natpmp_error: Option<String>,
    /// Last UPnP error.
    pub upnp_error: Option<String>,
}

/// Port mapping configuration (qBittorrent's "Use UPnP / NAT-PMP port
/// forwarding" sets both flags).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortMappingConfig {
    pub upnp: bool,
    pub natpmp: bool,
    /// NAT-PMP gateway; `None` = the default IPv4 gateway of the host.
    pub natpmp_gateway: Option<SocketAddr>,
    /// Lease requested from the gateways; 0 = default (UPnP 60 s, NAT-PMP 1 h).
    pub lease_secs: u32,
    /// UPnP mapping description.
    pub description: Option<String>,
}

impl PortMappingConfig {
    pub fn enabled(&self) -> bool {
        self.upnp || self.natpmp
    }
}

struct Inner {
    config: PortMappingConfig,
    listen_ports: BTreeSet<(MappingProtocol, u16)>,
    extra_ports: BTreeSet<(MappingProtocol, u16)>,
    token: Option<CancellationToken>,
    natpmp_stop: Option<watch::Sender<bool>>,
}

pub(crate) struct PortMapper {
    inner: Mutex<Inner>,
    // Bumped on every restart; tasks of older generations stop reporting.
    generation: Arc<AtomicU64>,
    status: Arc<RwLock<PortMappingStatus>>,
    parent: CancellationToken,
    bind_device: RwLock<Option<BindDevice>>,
    /// Gateway-reported addresses are votes for our external address.
    voter: Arc<ExternalIpVoter>,
}

async fn run_upnp(forwarder: UpnpPortForwarder) -> anyhow::Result<()> {
    forwarder.run_forever().await
}

fn to_upnp(p: MappingProtocol) -> UpnpProtocol {
    match p {
        MappingProtocol::Tcp => UpnpProtocol::Tcp,
        MappingProtocol::Udp => UpnpProtocol::Udp,
    }
}

fn to_natpmp(p: MappingProtocol) -> NatPmpProtocol {
    match p {
        MappingProtocol::Tcp => NatPmpProtocol::Tcp,
        MappingProtocol::Udp => NatPmpProtocol::Udp,
    }
}

impl PortMapper {
    pub fn new(
        parent: CancellationToken,
        config: PortMappingConfig,
        voter: Arc<ExternalIpVoter>,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                config,
                listen_ports: Default::default(),
                extra_ports: Default::default(),
                token: None,
                natpmp_stop: None,
            }),
            status: Default::default(),
            generation: Default::default(),
            parent,
            bind_device: RwLock::new(None),
            voter,
        }
    }

    pub fn status(&self) -> PortMappingStatus {
        self.status.read().clone()
    }

    pub fn config(&self) -> PortMappingConfig {
        self.inner.lock().config.clone()
    }

    pub fn set_config(&self, config: PortMappingConfig) {
        let mut g = self.inner.lock();
        if g.config == config {
            return;
        }
        g.config = config;
        self.restart(&mut g);
    }

    pub fn set_bind_device(&self, bd: Option<BindDevice>) {
        *self.bind_device.write() = bd;
        let mut g = self.inner.lock();
        self.restart(&mut g);
    }

    /// The listen port (TCP and/or UDP) changed.
    pub fn set_listen_ports(&self, ports: impl IntoIterator<Item = (MappingProtocol, u16)>) {
        let ports: BTreeSet<_> = ports.into_iter().filter(|(_, p)| *p != 0).collect();
        let mut g = self.inner.lock();
        if g.listen_ports == ports {
            return;
        }
        g.listen_ports = ports;
        self.restart(&mut g);
    }

    /// Map an extra port (e.g. the WebUI). Returns false if already mapped.
    pub fn add_port(&self, protocol: MappingProtocol, port: u16) -> bool {
        if port == 0 {
            return false;
        }
        let mut g = self.inner.lock();
        if !g.extra_ports.insert((protocol, port)) {
            return false;
        }
        self.restart(&mut g);
        true
    }

    pub fn remove_port(&self, protocol: MappingProtocol, port: u16) -> bool {
        let mut g = self.inner.lock();
        if !g.extra_ports.remove(&(protocol, port)) {
            return false;
        }
        self.restart(&mut g);
        true
    }

    fn restart(&self, g: &mut Inner) {
        if let Some(t) = g.token.take() {
            t.cancel();
        }
        if let Some(stop) = g.natpmp_stop.take() {
            // The old NAT-PMP task deletes its mappings, then exits.
            let _ = stop.send(true);
        }
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let ports: Vec<(MappingProtocol, u16)> = g
            .listen_ports
            .iter()
            .chain(g.extra_ports.iter())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        {
            let mut st = self.status.write();
            *st = PortMappingStatus {
                upnp: g.config.upnp,
                natpmp: g.config.natpmp,
                requested: ports.clone(),
                ..Default::default()
            };
        }
        self.voter.clear_router();
        if !g.config.enabled() || ports.is_empty() {
            return;
        }
        let token = self.parent.child_token();
        g.token = Some(token.clone());

        if g.config.upnp {
            self.spawn_upnp(&g.config, &ports, token.clone(), generation);
        }
        if g.config.natpmp {
            let (stop_tx, stop_rx) = watch::channel(false);
            g.natpmp_stop = Some(stop_tx);
            self.spawn_natpmp(&g.config, ports, stop_rx, generation);
        }
    }

    fn spawn_upnp(
        &self,
        config: &PortMappingConfig,
        ports: &[(MappingProtocol, u16)],
        token: CancellationToken,
        generation: u64,
    ) {
        let mut opts = UpnpPortForwarderOptions::default();
        if config.lease_secs > 0 {
            opts.lease_duration = Duration::from_secs(u64::from(config.lease_secs.max(30)));
        }
        if let Some(d) = &config.description {
            opts.description = d.clone();
        }
        let status = self.status.clone();
        let current = self.generation.clone();
        let (ip_status, ip_current, voter) = (status.clone(), current.clone(), self.voter.clone());
        let cb = Arc::new(move |ev: librqbit_upnp::UpnpMappingEvent| {
            if current.load(Ordering::Relaxed) != generation {
                return;
            }
            let protocol = match ev.protocol {
                UpnpProtocol::Tcp => MappingProtocol::Tcp,
                UpnpProtocol::Udp => MappingProtocol::Udp,
            };
            let mut st = status.write();
            st.mappings.retain(|m| {
                !(m.transport == MappingTransport::Upnp
                    && m.protocol == protocol
                    && m.internal_port == ev.port)
            });
            match ev.result {
                Ok(()) => {
                    st.upnp_error = None;
                    st.mappings.push(PortMapping {
                        transport: MappingTransport::Upnp,
                        protocol,
                        internal_port: ev.port,
                        external_port: ev.port,
                        lifetime_secs: u32::try_from(ev.lease_duration.as_secs())
                            .unwrap_or(u32::MAX),
                    });
                    st.mappings.sort_by_key(|m| {
                        (m.transport, m.protocol, m.internal_port, m.external_port)
                    });
                }
                Err(e) => st.upnp_error = Some(e),
            }
        });
        let forwarder = match UpnpPortForwarder::new_with_protocols(
            ports.iter().map(|(p, port)| (to_upnp(*p), *port)).collect(),
            Some(opts),
            self.bind_device.read().clone(),
        ) {
            Ok(f) => f
                .with_status_callback(cb)
                .with_external_ip_callback(Arc::new(move |r: Result<IpAddr, String>| {
                    // Failures are only logged (by the forwarder): the
                    // mapping errors are what the user needs to see.
                    if let Ok(ip) = r
                        && ip_current.load(Ordering::Relaxed) == generation
                    {
                        ip_status.write().external_ip = Some(ip);
                        voter.cast_vote(ip, IpVoteSource::Router);
                    }
                })),
            Err(e) => {
                self.status.write().upnp_error = Some(format!("{e:#}"));
                return;
            }
        };
        info!(?ports, "starting UPnP port forwarding");
        spawn_with_cancel(
            debug_span!("upnp_forward"),
            "upnp_forward",
            token,
            run_upnp(forwarder),
        );
    }

    fn spawn_natpmp(
        &self,
        config: &PortMappingConfig,
        ports: Vec<(MappingProtocol, u16)>,
        stop: watch::Receiver<bool>,
        generation: u64,
    ) {
        let gateway = config.natpmp_gateway;
        let lifetime = config.lease_secs;
        let status = self.status.clone();
        let current = self.generation.clone();
        let parent = self.parent.clone();
        let voter = self.voter.clone();
        info!(?ports, "starting NAT-PMP port mapping");
        spawn_with_cancel(debug_span!("natpmp"), "natpmp", parent, async move {
            let is_current = || current.load(Ordering::Relaxed) == generation;
            let set_error = |e: String| {
                if is_current() {
                    status.write().natpmp_error = Some(e);
                }
            };
            let client = match gateway {
                Some(g) => librqbit_natpmp::Client::with_gateway_addr(g).await,
                None => match librqbit_natpmp::default_gateway().await {
                    Ok(g) => librqbit_natpmp::Client::new(g).await,
                    Err(e) => Err(e),
                },
            };
            let client = match client {
                Ok(c) => c,
                Err(e) => {
                    debug!("NAT-PMP unavailable: {e}");
                    set_error(e.to_string());
                    return Ok::<_, anyhow::Error>(());
                }
            };
            let (status_tx, mut status_rx) = watch::channel(None);
            let mapper = librqbit_natpmp::run_mapper(
                client,
                ports
                    .iter()
                    .map(|(p, port)| (to_natpmp(*p), *port))
                    .collect(),
                lifetime,
                stop,
                status_tx,
            );
            let updater = async {
                while status_rx.changed().await.is_ok() {
                    let v = status_rx.borrow_and_update().clone();
                    if !is_current() {
                        continue;
                    }
                    let mut st = status.write();
                    st.mappings
                        .retain(|m| m.transport != MappingTransport::NatPmp);
                    match v {
                        Some((ext, maps)) => {
                            st.external_ip = Some(IpAddr::V4(ext));
                            voter.cast_vote(IpAddr::V4(ext), IpVoteSource::Router);
                            st.natpmp_error = None;
                            for m in maps {
                                st.mappings.push(PortMapping {
                                    transport: MappingTransport::NatPmp,
                                    protocol: match m.protocol {
                                        NatPmpProtocol::Tcp => MappingProtocol::Tcp,
                                        NatPmpProtocol::Udp => MappingProtocol::Udp,
                                    },
                                    internal_port: m.internal_port,
                                    external_port: m.external_port,
                                    lifetime_secs: m.lifetime,
                                });
                            }
                            st.mappings.sort_by_key(|m| {
                                (m.transport, m.protocol, m.internal_port, m.external_port)
                            });
                        }
                        None => {
                            st.natpmp_error = Some("no response from gateway".to_owned());
                        }
                    }
                }
            };
            tokio::join!(mapper, updater);
            Ok(())
        });
    }
}
