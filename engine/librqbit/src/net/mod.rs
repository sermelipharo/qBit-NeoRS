//! engine-net: session networking settings that can change at runtime
//! (protocol encryption, proxies, anonymous mode, outgoing socket tuning,
//! connection speed, PeX), peer IP filtering and port mapping.
//!
//! The settings are applied with the `Session::set_*` methods; see
//! `docs/ENGINE.md` ("engine-net").

pub(crate) mod encryption;
pub(crate) mod external_ip;
pub(crate) mod filter;
pub(crate) mod port_mapping;
pub(crate) mod rate;
pub(crate) mod socket;

pub use filter::{BlockedPeer, BlockedPeerCallback, PeerBlockReason};
pub use librqbit_mse::EncryptionPolicy;
pub use librqbit_proxy::{ProxyConfig, ProxyKind};
pub use port_mapping::{
    MappingProtocol, MappingTransport, PortMapping, PortMappingConfig, PortMappingStatus,
};

/// Proxy used by the session (qBittorrent's connection proxy for BitTorrent
/// purposes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxySettings {
    pub config: ProxyConfig,
    /// Connect to peers through the proxy (libtorrent `proxy_peer_connections`).
    /// While on, uTP is not used, incoming connections are refused and web
    /// seeds are fetched through the proxy too.
    pub peer_connections: bool,
    /// Contact HTTP(S) trackers through the proxy (libtorrent
    /// `proxy_tracker_connections`). UDP trackers are skipped while on (no
    /// UDP relaying through proxies).
    pub tracker_connections: bool,
}

/// Runtime networking settings of a session (see `Session::set_net_settings`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetSettings {
    /// Protocol encryption (MSE/PE) for incoming and outgoing peer connections.
    pub encryption: EncryptionPolicy,
    pub proxy: Option<ProxySettings>,
    /// libtorrent `anonymous_mode`: a generic random peer id, no client name
    /// (`v`) or listen port (`p`) in the extended handshake, and the tracker
    /// side of anonymous mode (`TrackerSettings::anonymous_mode`).
    pub anonymous_mode: bool,
    /// Outgoing peer connections over TCP.
    pub enable_tcp: bool,
    /// Outgoing peer connections over uTP (needs a uTP listener socket).
    pub enable_utp: bool,
    /// Local port range (inclusive) for outgoing TCP connections; `None` lets
    /// the OS choose.
    pub outgoing_ports: Option<(u16, u16)>,
    /// IP type of service / traffic class byte of peer TCP sockets (the DSCP
    /// value shifted left by 2); 0 leaves the OS default.
    pub peer_tos: u8,
    /// `SO_SNDBUF` of peer TCP sockets in bytes; 0 = OS default.
    pub send_buffer_size: u32,
    /// `SO_RCVBUF` of peer TCP sockets in bytes; 0 = OS default.
    pub recv_buffer_size: u32,
    /// Outgoing connection attempts per second, session-wide (libtorrent
    /// `connection_speed`); 0 = unlimited.
    pub connection_speed: u32,
    /// Peer exchange (ut_pex): advertise, send and accept PEX messages.
    pub pex: bool,
}

impl Default for NetSettings {
    fn default() -> Self {
        Self {
            encryption: EncryptionPolicy::Enabled,
            proxy: None,
            anonymous_mode: false,
            enable_tcp: true,
            enable_utp: true,
            outgoing_ports: None,
            peer_tos: 0,
            send_buffer_size: 0,
            recv_buffer_size: 0,
            connection_speed: 0,
            pex: true,
        }
    }
}

impl NetSettings {
    /// The proxy for peer connections, if any.
    pub fn peer_proxy(&self) -> Option<&ProxyConfig> {
        self.proxy
            .as_ref()
            .filter(|p| p.peer_connections)
            .map(|p| &p.config)
    }

    /// The proxy for tracker connections, if any.
    pub fn tracker_proxy(&self) -> Option<&ProxyConfig> {
        self.proxy
            .as_ref()
            .filter(|p| p.tracker_connections)
            .map(|p| &p.config)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if let Some((lo, hi)) = self.outgoing_ports
            && (lo == 0 || lo > hi)
        {
            anyhow::bail!("invalid outgoing port range {lo}-{hi}");
        }
        if let Some(p) = &self.proxy {
            if p.config.host.is_empty() || p.config.port == 0 {
                anyhow::bail!("invalid proxy address");
            }
            if p.config.host.contains(['/', '@', ' ', '\r', '\n']) {
                anyhow::bail!("invalid proxy host");
            }
            // Fail here, before anything is applied, rather than when the
            // tracker or web seed client is built.
            anyhow::Context::context(reqwest_proxy(&p.config), "invalid proxy")?;
        }
        Ok(())
    }
}

/// Builds the `reqwest` proxy for `cfg` (HTTP, SOCKS4/4a, SOCKS5/5h), with
/// credentials. Used for trackers and web seeds.
pub(crate) fn reqwest_proxy(cfg: &ProxyConfig) -> anyhow::Result<reqwest::Proxy> {
    let scheme = match (cfg.kind, cfg.remote_dns) {
        (ProxyKind::Http, _) => "http",
        (ProxyKind::Socks4, false) => "socks4",
        (ProxyKind::Socks4, true) => "socks4a",
        (ProxyKind::Socks5, false) => "socks5",
        (ProxyKind::Socks5, true) => "socks5h",
    };
    let host = if cfg.host.contains(':') && !cfg.host.starts_with('[') {
        format!("[{}]", cfg.host)
    } else {
        cfg.host.clone()
    };
    let mut url = url::Url::parse(&format!("{scheme}://{host}:{}", cfg.port))?;
    if let Some((user, pass)) = &cfg.auth {
        // set_username/set_password percent-encode as needed.
        url.set_username(user)
            .map_err(|_| anyhow::anyhow!("invalid proxy user name"))?;
        url.set_password(Some(pass))
            .map_err(|_| anyhow::anyhow!("invalid proxy password"))?;
    }
    Ok(reqwest::Proxy::all(url.as_str())?)
}

/// Parses the legacy `ConnectionOptions::proxy_url` (`socks5://` or
/// `socks5h://[user:pass@]host:port`) into a proxy for peers and trackers.
pub(crate) fn proxy_from_url(url: &str) -> anyhow::Result<ProxySettings> {
    let url = url::Url::parse(url)?;
    let (kind, remote_dns) = match url.scheme() {
        "socks5" => (ProxyKind::Socks5, false),
        "socks5h" => (ProxyKind::Socks5, true),
        "socks4" => (ProxyKind::Socks4, false),
        "socks4a" => (ProxyKind::Socks4, true),
        "http" => (ProxyKind::Http, true),
        other => anyhow::bail!("unsupported proxy URL scheme \"{other}\""),
    };
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("missing host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url.port().ok_or_else(|| anyhow::anyhow!("missing port"))?;
    let auth = url.password().map(|p| {
        let dec = |s: &str| {
            urlencoding::decode(s)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| s.to_owned())
        };
        (dec(url.username()), dec(p))
    });
    Ok(ProxySettings {
        config: ProxyConfig {
            kind,
            host,
            port,
            auth,
            remote_dns,
        },
        peer_connections: true,
        tracker_connections: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_legacy_proxy_urls() {
        let p = proxy_from_url("socks5://myhost:1234").unwrap();
        assert_eq!(p.config.kind, ProxyKind::Socks5);
        assert_eq!(p.config.host, "myhost");
        assert_eq!(p.config.port, 1234);
        assert!(!p.config.remote_dns);
        assert_eq!(p.config.auth, None);
        assert!(p.peer_connections && p.tracker_connections);

        let p = proxy_from_url("socks5h://us%40r:p%3Ass@[::1]:1080").unwrap();
        assert!(p.config.remote_dns);
        assert_eq!(p.config.host, "::1");
        assert_eq!(p.config.auth, Some(("us@r".into(), "p:ss".into())));

        let p = proxy_from_url("http://proxy:3128").unwrap();
        assert_eq!(p.config.kind, ProxyKind::Http);

        assert!(proxy_from_url("ftp://x:1").is_err());
        assert!(proxy_from_url("socks5://").is_err());
        assert!(proxy_from_url("not a url").is_err());
        assert!(proxy_from_url("socks5://host").is_err());
    }

    #[test]
    fn reqwest_proxy_builds() {
        for kind in [ProxyKind::Http, ProxyKind::Socks4, ProxyKind::Socks5] {
            for remote_dns in [false, true] {
                let cfg = ProxyConfig {
                    kind,
                    host: "::1".into(),
                    port: 8080,
                    auth: Some(("a b".into(), "p@ss:".into())),
                    remote_dns,
                };
                reqwest_proxy(&cfg).unwrap();
            }
        }
    }

    #[test]
    fn validate_settings() {
        let mut s = NetSettings::default();
        s.validate().unwrap();
        s.outgoing_ports = Some((10, 5));
        assert!(s.validate().is_err());
        s.outgoing_ports = Some((0, 5));
        assert!(s.validate().is_err());
        s.outgoing_ports = Some((5, 5));
        s.validate().unwrap();
        s.proxy = Some(ProxySettings {
            config: ProxyConfig {
                kind: ProxyKind::Http,
                host: "a/b".into(),
                port: 1,
                auth: None,
                remote_dns: false,
            },
            peer_connections: true,
            tracker_connections: true,
        });
        assert!(s.validate().is_err());
        // Accepted by the character check but unusable as a proxy URL: must
        // fail validation, not later when the tracker client is rebuilt.
        if let Some(p) = s.proxy.as_mut() {
            p.config.host = "proxy.example.com:1080".into();
        }
        assert!(s.validate().is_err());
    }
}
