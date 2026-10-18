//! Proxy environment for the Python processes
//! (`SearchPluginManager::applyProxySettings()`).

use crate::prefs::{ConnectionPrefs, ProxyType};

/// For Python's `urllib` (`ProxyHandler`).
pub(crate) const HTTP_PROXY: &str = "http_proxy";
pub(crate) const HTTPS_PROXY: &str = "https_proxy";
/// Read by `helpers.enable_socks_proxy()`.
pub(crate) const SOCKS_PROXY: &str = "qbt_socks_proxy";

/// Variables to set on top of the inherited environment; the three proxy
/// variables not listed here are removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProxyEnv {
    pub vars: Vec<(&'static str, String)>,
}

impl ProxyEnv {
    /// Applies the variables to a command (always clearing inherited values
    /// of the three proxy variables first, like qBittorrent).
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        for key in [HTTP_PROXY, HTTPS_PROXY, SOCKS_PROXY] {
            cmd.env_remove(key);
        }
        for (k, v) in &self.vars {
            cmd.env(k, v);
        }
    }
}

/// Builds the proxy variables from the Connection page. Only applies when
/// "Use proxy for general purposes" (`proxy_misc`) is on.
pub(crate) fn proxy_env(c: &ConnectionPrefs) -> ProxyEnv {
    if !c.proxy_misc {
        return ProxyEnv::default();
    }
    let host = if c.proxy_ip.contains(':') && !c.proxy_ip.starts_with('[') {
        // IPv6 literal: bracket it so the URL stays parseable.
        format!("[{}]", c.proxy_ip)
    } else {
        c.proxy_ip.clone()
    };
    let credential = if c.proxy_auth_enabled { format!("{}:{}@", c.proxy_username, c.proxy_password) } else { String::new() };
    let vars = match c.proxy_type {
        ProxyType::None => Vec::new(),
        ProxyType::Http => {
            let url = format!("http://{credential}{host}:{}", c.proxy_port);
            vec![(HTTP_PROXY, url.clone()), (HTTPS_PROXY, url)]
        }
        ProxyType::Socks5 => {
            let scheme = if c.proxy_hostname_lookup { "socks5h" } else { "socks5" };
            vec![(SOCKS_PROXY, format!("{scheme}://{credential}{host}:{}", c.proxy_port))]
        }
        ProxyType::Socks4 => {
            // SOCKS4 has no authentication.
            let scheme = if c.proxy_hostname_lookup { "socks4a" } else { "socks4" };
            vec![(SOCKS_PROXY, format!("{scheme}://{host}:{}", c.proxy_port))]
        }
    };
    ProxyEnv { vars }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(t: ProxyType) -> ConnectionPrefs {
        ConnectionPrefs {
            proxy_type: t,
            proxy_ip: "10.0.0.1".into(),
            proxy_port: 3128,
            proxy_misc: true,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_for_general_purposes() {
        let mut c = prefs(ProxyType::Http);
        c.proxy_misc = false;
        assert!(proxy_env(&c).vars.is_empty());
        assert!(proxy_env(&prefs(ProxyType::None)).vars.is_empty());
    }

    #[test]
    fn http_proxy() {
        let env = proxy_env(&prefs(ProxyType::Http));
        assert_eq!(
            env.vars,
            vec![(HTTP_PROXY, "http://10.0.0.1:3128".to_string()), (HTTPS_PROXY, "http://10.0.0.1:3128".to_string())]
        );
        let mut c = prefs(ProxyType::Http);
        c.proxy_auth_enabled = true;
        c.proxy_username = "u".into();
        c.proxy_password = "p@ss".into();
        assert_eq!(proxy_env(&c).vars[0].1, "http://u:p@ss@10.0.0.1:3128");
    }

    #[test]
    fn socks_proxies() {
        let mut c = prefs(ProxyType::Socks5);
        assert_eq!(proxy_env(&c).vars, vec![(SOCKS_PROXY, "socks5h://10.0.0.1:3128".to_string())]);
        c.proxy_hostname_lookup = false;
        c.proxy_auth_enabled = true;
        c.proxy_username = "u".into();
        c.proxy_password = "p".into();
        assert_eq!(proxy_env(&c).vars, vec![(SOCKS_PROXY, "socks5://u:p@10.0.0.1:3128".to_string())]);

        let mut c = prefs(ProxyType::Socks4);
        c.proxy_auth_enabled = true;
        c.proxy_username = "ignored".into();
        assert_eq!(proxy_env(&c).vars, vec![(SOCKS_PROXY, "socks4a://10.0.0.1:3128".to_string())]);
        c.proxy_hostname_lookup = false;
        assert_eq!(proxy_env(&c).vars, vec![(SOCKS_PROXY, "socks4://10.0.0.1:3128".to_string())]);
    }

    #[test]
    fn ipv6_host_is_bracketed() {
        let mut c = prefs(ProxyType::Http);
        c.proxy_ip = "::1".into();
        assert_eq!(proxy_env(&c).vars[0].1, "http://[::1]:3128");
    }
}
