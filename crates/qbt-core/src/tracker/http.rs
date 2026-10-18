//! Tiny HTTP/1.x request reader and the announce handler.

use std::net::{IpAddr, SocketAddr};

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use super::bencode::Value;
use super::state::{ANNOUNCE_INTERVAL, Peer, TrackerState};

/// Largest request head accepted (request line + headers).
const MAX_HEAD: usize = 8 * 1024;
const DEFAULT_NUMWANT: usize = 50;

pub struct Request {
    pub method: String,
    pub path: String,
    /// Raw, percent-decoded query parameters (first occurrence wins).
    pub query: Vec<(String, Vec<u8>)>,
}

impl Request {
    fn param(&self, name: &str) -> Option<&[u8]> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_slice())
    }
}

pub struct Response {
    status: u16,
    reason: &'static str,
    body: Vec<u8>,
}

impl Response {
    pub fn status(status: u16) -> Self {
        let reason = match status {
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Request Header Fields Too Large",
            _ => "OK",
        };
        Self { status, reason, body: Vec::new() }
    }

    fn ok(body: Vec<u8>) -> Self {
        Self { status: 200, reason: "OK", body }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.status,
            self.reason,
            self.body.len()
        )
        .into_bytes();
        out.extend_from_slice(&self.body);
        out
    }

    #[cfg(test)]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Reads and parses the request head; errors are HTTP status codes.
pub async fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.map_err(|_| 400u16)?;
        if n == 0 {
            return Err(400);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEAD {
            return Err(413);
        }
    }
    parse_head(&buf)
}

pub fn parse_head(buf: &[u8]) -> Result<Request, u16> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(buf) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(400),
    }
    let method = req.method.ok_or(400u16)?.to_string();
    let target = req.path.ok_or(400u16)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Request { method, path: path.to_string(), query: parse_query(query) })
}

fn parse_query(q: &str) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = String::from_utf8_lossy(&percent_decode(k.as_bytes())).into_owned();
        if !out.iter().any(|(existing, _)| *existing == key) {
            out.push((key, percent_decode(v.as_bytes())));
        }
    }
    out
}

fn percent_decode(s: &[u8]) -> Vec<u8> {
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'%' if i + 2 < s.len() => match (hex(s[i + 1]), hex(s[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    out
}

pub fn route(req: &Request, peer: SocketAddr, state: &Mutex<TrackerState>) -> Response {
    if req.method != "GET" {
        return Response::status(405);
    }
    if !req.path.to_ascii_lowercase().starts_with("/announce") {
        return Response::status(404);
    }
    match announce(req, peer, state) {
        Ok(body) => Response::ok(body),
        Err(reason) => Response::ok(Value::dict(vec![("failure reason", Value::str(reason))]).to_bytes()),
    }
}

/// Loopback, RFC 1918, link-local, CGNAT and IPv6 ULA/link-local addresses.
fn is_local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || (a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(a) => {
            a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn announce(req: &Request, peer_addr: SocketAddr, state: &Mutex<TrackerState>) -> Result<Vec<u8>, &'static str> {
    let socket_ip = normalize(peer_addr.ip());

    let info_hash: [u8; 20] = req
        .param("info_hash")
        .ok_or("Missing \"info_hash\" parameter")?
        .try_into()
        .map_err(|_| "Invalid \"info_hash\" parameter")?;

    let peer_id = req.param("peer_id").ok_or("Missing \"peer_id\" parameter")?;
    if peer_id.len() > 20 {
        return Err("Invalid \"peer_id\" parameter");
    }

    let port: u16 = req
        .param("port")
        .ok_or("Missing \"port\" parameter")?
        .iter()
        .try_fold(0u32, |acc, &c| {
            if c.is_ascii_digit() && acc <= 65535 { Some(acc * 10 + (c - b'0') as u32) } else { None }
        })
        .filter(|p| (1..=65535).contains(p))
        .map(|p| p as u16)
        .ok_or("Invalid \"port\" parameter")?;

    let numwant = match req.param("numwant") {
        None => DEFAULT_NUMWANT,
        Some(v) => std::str::from_utf8(v)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n >= 0)
            .ok_or("Invalid \"numwant\" parameter")? as usize,
    };
    let no_peer_id = req.param("no_peer_id") == Some(b"1");
    let is_seeder = req.param("left") == Some(b"0");
    let compact = req.param("compact") != Some(b"0");

    // Self-reported address: honoured only from local networks (hardening).
    let claimed = req
        .param("ip")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|s| s.parse::<IpAddr>().ok())
        .filter(|_| is_local_network(socket_ip))
        .map(normalize);
    let ip = claimed.unwrap_or(socket_ip);

    let peer = Peer { address: ip.to_string(), ip, port, peer_id: peer_id.to_vec(), is_seeder };

    let event = req.param("event").unwrap_or(b"");
    let stopped = match event {
        b"" | b"empty" | b"completed" | b"started" | b"paused" => false,
        b"stopped" => true,
        _ => return Err("Invalid \"event\" parameter"),
    };

    let mut st = state.lock();
    if stopped {
        st.unregister(&info_hash, &peer);
    } else {
        st.register(info_hash, peer);
    }

    let (seeders, leechers, peers) = match st.torrent(&info_hash) {
        Some(t) => (t.seeders(), t.leechers(), if stopped { &[][..] } else { t.peers() }),
        None => (0, 0, &[][..]),
    };

    let external_ip = match socket_ip {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    };
    let mut reply = vec![
        ("interval", Value::Int(ANNOUNCE_INTERVAL)),
        ("complete", Value::Int(seeders as i64)),
        ("incomplete", Value::Int(leechers as i64)),
        ("external ip", Value::Bytes(external_ip)),
    ];
    if compact {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for p in peers.iter().take(numwant) {
            match p.ip {
                IpAddr::V4(_) => v4.extend(p.compact()),
                IpAddr::V6(_) => v6.extend(p.compact()),
            }
        }
        reply.push(("peers", Value::Bytes(v4)));
        if !v6.is_empty() {
            reply.push(("peers6", Value::Bytes(v6)));
        }
    } else {
        let list = peers
            .iter()
            .take(numwant)
            .map(|p| {
                let mut d = vec![("ip", Value::str(&p.address)), ("port", Value::Int(p.port as i64))];
                if !no_peer_id {
                    d.push(("peer id", Value::Bytes(p.peer_id.clone())));
                }
                Value::dict(d)
            })
            .collect();
        reply.push(("peers", Value::List(list)));
    }
    Ok(Value::dict(reply).to_bytes())
}
