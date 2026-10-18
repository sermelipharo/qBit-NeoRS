//! Peers tab helpers: flag letters and descriptions
//! (`PeerInfo::determineFlags`), per-peer rate estimation from counters, and
//! a small bounded reverse-DNS cache ("Resolve peer host names").

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, Default)]
pub struct PeerFlags {
    pub interesting: bool,
    pub remote_choked: bool,
    pub remote_interested: bool,
    pub choked: bool,
    pub optimistic_unchoke: bool,
    pub snubbed: bool,
    pub incoming: bool,
    pub from_dht: bool,
    pub from_pex: bool,
    pub from_lsd: bool,
    pub rc4_encrypted: bool,
    pub plaintext_encrypted: bool,
    pub utp: bool,
    pub holepunched: bool,
}

/// Flag letters (space separated) and their descriptions (one per line),
/// in qBittorrent's order.
pub fn determine_flags(f: &PeerFlags) -> (String, String) {
    let mut letters: Vec<&str> = Vec::new();
    let mut desc: Vec<String> = Vec::new();
    let mut add = |cond: bool, letter: &'static str, text: &str| {
        if cond {
            letters.push(letter);
            desc.push(format!("{letter} = {text}"));
        }
    };
    add(f.interesting && f.remote_choked, "d", "Interested (local) and choked (peer)");
    add(f.interesting && !f.remote_choked, "D", "Interested (local) and unchoked (peer)");
    add(f.remote_interested && f.choked, "u", "Interested (peer) and choked (local)");
    add(f.remote_interested && !f.choked, "U", "Interested (peer) and unchoked (local)");
    add(!f.remote_choked && !f.interesting, "K", "Not interested (local) and unchoked (peer)");
    add(!f.choked && !f.remote_interested, "?", "Not interested (peer) and unchoked (local)");
    add(f.optimistic_unchoke, "O", "Optimistic unchoke");
    add(f.snubbed, "S", "Peer snubbed");
    add(f.incoming, "I", "Incoming connection");
    add(f.from_dht, "H", "Peer from DHT");
    add(f.from_pex, "X", "Peer from PEX");
    add(f.from_lsd, "L", "Peer from LSD");
    add(f.rc4_encrypted, "E", "Encrypted traffic");
    add(f.plaintext_encrypted, "e", "Encrypted handshake");
    add(f.utp, "P", "μTP");
    add(f.holepunched, "h", "Peer is using NAT hole punching");
    (letters.join(" "), desc.join("\n"))
}

/// "Peer ID Client": the first 8 bytes of the peer id as ASCII; empty for
/// an all-zero id, `Unknown` if any byte is not printable.
pub fn peer_id_client(id: &[u8; 20]) -> String {
    if id.iter().all(|&b| b == 0) {
        return String::new();
    }
    let head = &id[..8];
    if head.iter().any(|&b| !(32..=126).contains(&b)) {
        return "Unknown".to_string();
    }
    head.iter().map(|&b| b as char).collect()
}

const HOST_CACHE_CAP: usize = 512;

/// Bounded FIFO cache of reverse lookups.
#[derive(Default)]
pub struct HostCache {
    names: HashMap<IpAddr, String>,
    order: VecDeque<IpAddr>,
    pending: HashSet<IpAddr>,
}

impl HostCache {
    pub fn get(&self, ip: &IpAddr) -> Option<&str> {
        self.names.get(ip).map(String::as_str)
    }

    /// True if a lookup should be started for `ip`.
    pub fn should_resolve(&mut self, ip: IpAddr) -> bool {
        if self.names.contains_key(&ip) || self.pending.contains(&ip) || self.pending.len() >= 32 {
            return false;
        }
        self.pending.insert(ip);
        true
    }

    pub fn insert(&mut self, ip: IpAddr, name: String) {
        self.pending.remove(&ip);
        if self.names.contains_key(&ip) {
            return;
        }
        if self.order.len() >= HOST_CACHE_CAP
            && let Some(old) = self.order.pop_front()
        {
            self.names.remove(&old);
        }
        self.order.push_back(ip);
        self.names.insert(ip, name);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Blocking reverse DNS lookup; a result equal to the address is ignored.
pub fn reverse_lookup(ip: IpAddr) -> Option<String> {
    let addr = SocketAddr::new(ip, 0);
    let (storage, len) = sockaddr_of(&addr);
    let mut host = [0 as libc::c_char; 1025];
    // SAFETY: `storage` holds a valid sockaddr of `len` bytes and `host` is a
    // writable buffer of the given size.
    let rc = unsafe {
        libc::getnameinfo(
            &storage as *const libc::sockaddr_storage as *const libc::sockaddr,
            len,
            host.as_mut_ptr(),
            host.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: getnameinfo NUL-terminates the buffer on success.
    let name = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }.to_string_lossy().into_owned();
    let name: String = name.chars().filter(|c| !c.is_control()).take(253).collect();
    (!name.is_empty() && name != ip.to_string()).then_some(name)
}

fn sockaddr_of(addr: &SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: an all-zero sockaddr_storage is a valid value.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let len = match addr {
        SocketAddr::V4(a) => {
            let sin = libc::sockaddr_in {
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: a.port().to_be(),
                sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(a.ip().octets()) },
                sin_zero: [0; 8],
            };
            // SAFETY: sockaddr_storage is large enough for sockaddr_in.
            unsafe { std::ptr::write(&mut storage as *mut _ as *mut libc::sockaddr_in, sin) };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(a) => {
            // SAFETY: an all-zero sockaddr_in6 is valid; fields are set below.
            let mut sin6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
            {
                sin6.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
            }
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = a.port().to_be();
            sin6.sin6_addr = libc::in6_addr { s6_addr: a.ip().octets() };
            // SAFETY: sockaddr_storage is large enough for sockaddr_in6.
            unsafe { std::ptr::write(&mut storage as *mut _ as *mut libc::sockaddr_in6, sin6) };
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, len as libc::socklen_t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_letters_and_descriptions() {
        let f = PeerFlags { interesting: true, remote_choked: false, choked: true, incoming: true, utp: true, ..Default::default() };
        let (letters, desc) = determine_flags(&f);
        assert_eq!(letters, "D I P");
        assert_eq!(desc, "D = Interested (local) and unchoked (peer)\nI = Incoming connection\nP = μTP");
        let idle = PeerFlags { remote_choked: true, choked: true, ..Default::default() };
        assert_eq!(determine_flags(&idle).0, "");
        let open = PeerFlags::default();
        assert_eq!(determine_flags(&open).0, "K ?");
    }

    #[test]
    fn peer_id_clients() {
        let mut id = [0u8; 20];
        assert_eq!(peer_id_client(&id), "");
        id[..8].copy_from_slice(b"-qB5240-");
        assert_eq!(peer_id_client(&id), "-qB5240-");
        id[3] = 0xff;
        assert_eq!(peer_id_client(&id), "Unknown");
    }
    #[test]
    fn host_cache_is_bounded() {
        let mut h = HostCache::default();
        for i in 0..(HOST_CACHE_CAP as u32 + 10) {
            let ip = IpAddr::V4(i.into());
            h.insert(ip, format!("h{i}"));
        }
        assert_eq!(h.names.len(), HOST_CACHE_CAP);
        assert!(h.get(&IpAddr::V4(0u32.into())).is_none());
        let ip: IpAddr = "9.9.9.9".parse().unwrap();
        assert!(h.should_resolve(ip));
        assert!(!h.should_resolve(ip));
    }
}
