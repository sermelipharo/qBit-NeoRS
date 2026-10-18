//! I2P support through a router's SAM v3.1 bridge (qBittorrent's
//! "I2P (experimental)": host/port, mixed mode, tunnel quantity/length).
//!
//! A [`Session`] owns the SAM control connection (the I2P session lives as
//! long as it stays open). Each outgoing peer connection or accept uses a new
//! SAM connection which, after the `STREAM` command succeeds, carries raw
//! peer-wire bytes.

use std::time::Duration;

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
use rand::RngExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// SAM line replies are short; anything longer is a protocol error.
const MAX_LINE: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamOptions {
    pub host: String,
    pub port: u16,
    pub inbound_quantity: u8,
    pub outbound_quantity: u8,
    pub inbound_length: u8,
    pub outbound_length: u8,
}

impl Default for SamOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 7656,
            inbound_quantity: 3,
            outbound_quantity: 3,
            inbound_length: 3,
            outbound_length: 3,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum I2pError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SAM error: {result}{}", message.as_deref().map(|m| format!(" ({m})")).unwrap_or_default())]
    Sam { result: String, message: Option<String> },
    #[error("unexpected SAM reply: {0}")]
    Protocol(String),
    #[error("timed out talking to the SAM bridge")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, I2pError>;

/// I2P's base64 variant (`-` and `~` instead of `+` and `/`).
pub fn i2p_base64() -> GeneralPurpose {
    let alphabet = Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-~").expect("valid alphabet");
    GeneralPurpose::new(&alphabet, GeneralPurposeConfig::new())
}

/// `<base32(sha256(destination))>.b32.i2p` for a base64 destination.
pub fn b32_address(destination_b64: &str) -> Option<String> {
    let raw = i2p_base64().decode(destination_b64).ok()?;
    let hash = Sha256::digest(&raw);
    Some(format!("{}.b32.i2p", base32_lower(&hash)))
}

fn base32_lower(data: &[u8]) -> String {
    const ALPHA: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(ALPHA[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHA[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Parses `TOPIC SUBTOPIC KEY=VALUE KEY="quoted value"` into key/value pairs.
pub fn parse_reply(line: &str) -> (Vec<&str>, Vec<(String, String)>) {
    let mut words = Vec::new();
    let mut pairs = Vec::new();
    let mut rest = line.trim_end();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let end = rest.find([' ', '=']).unwrap_or(rest.len());
        if rest[end..].starts_with('=') {
            let key = rest[..end].to_string();
            let after = &rest[end + 1..];
            let (value, consumed) = if let Some(q) = after.strip_prefix('"') {
                let close = q.find('"').unwrap_or(q.len());
                (q[..close].to_string(), 1 + close + usize::from(close < q.len()))
            } else {
                let e = after.find(' ').unwrap_or(after.len());
                (after[..e].to_string(), e)
            };
            pairs.push((key, value));
            rest = &after[consumed..];
        } else {
            words.push(&rest[..end]);
            rest = &rest[end..];
        }
    }
    (words, pairs)
}

fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

struct SamConn {
    reader: BufReader<TcpStream>,
}

impl SamConn {
    async fn open(opts: &SamOptions) -> Result<Self> {
        let s = TcpStream::connect((opts.host.as_str(), opts.port)).await?;
        let mut c = Self { reader: BufReader::new(s) };
        let reply = c.command("HELLO VERSION MIN=3.1 MAX=3.1").await?;
        Self::check(&reply, "HELLO", "REPLY")?;
        Ok(c)
    }

    async fn command(&mut self, cmd: &str) -> Result<String> {
        self.reader.get_mut().write_all(format!("{cmd}\n").as_bytes()).await?;
        self.read_line().await
    }

    async fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let n = (&mut self.reader).take(MAX_LINE as u64).read_line(&mut line).await?;
        if n == 0 || !line.ends_with('\n') {
            return Err(I2pError::Protocol(line));
        }
        Ok(line.trim_end().to_string())
    }

    fn check(reply: &str, topic: &str, sub: &str) -> Result<Vec<(String, String)>> {
        let (words, pairs) = parse_reply(reply);
        if words.first() != Some(&topic) || words.get(1) != Some(&sub) {
            return Err(I2pError::Protocol(reply.to_string()));
        }
        match get(&pairs, "RESULT") {
            Some("OK") => Ok(pairs),
            None if topic == "HELLO" => Ok(pairs),
            Some(r) => Err(I2pError::Sam { result: r.to_string(), message: get(&pairs, "MESSAGE").map(str::to_string) }),
            None => Err(I2pError::Protocol(reply.to_string())),
        }
    }

    fn into_stream(self) -> Result<TcpStream> {
        // Bytes the router sent after the status line belong to the peer;
        // refuse to drop them silently.
        if !self.reader.buffer().is_empty() {
            return Err(I2pError::Protocol("unexpected data after STREAM STATUS".into()));
        }
        Ok(self.reader.into_inner())
    }
}

/// An incoming stream (see [`Session::accept_with_prefix`]).
pub struct AcceptedStream {
    pub stream: TcpStream,
    /// The peer's base64 destination.
    pub destination: String,
    /// Peer bytes read ahead with the destination line.
    pub prefix: Vec<u8>,
}

/// An I2P streaming session.
pub struct Session {
    opts: SamOptions,
    id: String,
    // Kept open: the I2P session lives as long as this connection.
    _control: SamConn,
    /// Our public destination (base64).
    pub destination: String,
}

impl Session {
    pub async fn create(opts: SamOptions, timeout: Duration) -> Result<Self> {
        tokio::time::timeout(timeout, Self::create_inner(opts)).await.map_err(|_| I2pError::Timeout)?
    }

    async fn create_inner(opts: SamOptions) -> Result<Self> {
        let mut control = SamConn::open(&opts).await?;
        let id: String = {
            let mut rng = rand::rng();
            (0..12).map(|_| (b'a' + rng.random_range(0..26u8)) as char).collect()
        };
        let cmd = format!(
            "SESSION CREATE STYLE=STREAM ID={id} DESTINATION=TRANSIENT SIGNATURE_TYPE=7 \
             inbound.quantity={} outbound.quantity={} inbound.length={} outbound.length={}",
            opts.inbound_quantity, opts.outbound_quantity, opts.inbound_length, opts.outbound_length
        );
        let reply = control.command(&cmd).await?;
        SamConn::check(&reply, "SESSION", "STATUS")?;
        let reply = control.command("NAMING LOOKUP NAME=ME").await?;
        let pairs = SamConn::check(&reply, "NAMING", "REPLY")?;
        let destination = get(&pairs, "VALUE").ok_or_else(|| I2pError::Protocol(reply.clone()))?.to_string();
        Ok(Self { opts, id, _control: control, destination })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Our `.b32.i2p` address.
    pub fn b32(&self) -> Option<String> {
        b32_address(&self.destination)
    }

    /// Opens a stream to `destination` (base64 destination or `*.i2p` name).
    pub async fn connect(&self, destination: &str, timeout: Duration) -> Result<TcpStream> {
        if destination.contains(char::is_whitespace) {
            return Err(I2pError::Protocol("invalid destination".into()));
        }
        tokio::time::timeout(timeout, async {
            let mut c = SamConn::open(&self.opts).await?;
            let reply = c.command(&format!("STREAM CONNECT ID={} DESTINATION={destination} SILENT=false", self.id)).await?;
            SamConn::check(&reply, "STREAM", "STATUS")?;
            c.into_stream()
        })
        .await
        .map_err(|_| I2pError::Timeout)?
    }

    /// Waits for one incoming stream; returns it with the peer's destination.
    pub async fn accept(&self) -> Result<(TcpStream, String)> {
        let mut c = SamConn::open(&self.opts).await?;
        let reply = c.command(&format!("STREAM ACCEPT ID={} SILENT=false", self.id)).await?;
        SamConn::check(&reply, "STREAM", "STATUS")?;
        let peer = c.read_line().await?;
        let peer_dest = peer.split_whitespace().next().unwrap_or_default().to_string();
        if peer_dest.is_empty() {
            return Err(I2pError::Protocol("empty peer destination".into()));
        }
        Ok((c.into_stream()?, peer_dest))
    }

    /// Like [`Session::accept`], but bytes the peer sent right after the
    /// destination line (read ahead with it) are returned instead of being an
    /// error; they come before anything read from the stream.
    pub async fn accept_with_prefix(&self) -> Result<AcceptedStream> {
        let mut c = SamConn::open(&self.opts).await?;
        let reply = c.command(&format!("STREAM ACCEPT ID={} SILENT=false", self.id)).await?;
        SamConn::check(&reply, "STREAM", "STATUS")?;
        let peer = c.read_line().await?;
        let destination = peer.split_whitespace().next().unwrap_or_default().to_string();
        if destination.is_empty() {
            return Err(I2pError::Protocol("empty peer destination".into()));
        }
        let prefix = c.reader.buffer().to_vec();
        Ok(AcceptedStream { stream: c.reader.into_inner(), destination, prefix })
    }

    /// Resolves a name (`*.i2p`, `*.b32.i2p`) to a base64 destination. Uses
    /// its own SAM connection, so lookups never wait for other commands.
    pub async fn lookup(&self, name: &str) -> Result<String> {
        if name.contains(char::is_whitespace) {
            return Err(I2pError::Protocol("invalid name".into()));
        }
        let mut c = SamConn::open(&self.opts).await?;
        let reply = c.command(&format!("NAMING LOOKUP NAME={name}")).await?;
        let pairs = SamConn::check(&reply, "NAMING", "REPLY")?;
        get(&pairs, "VALUE").map(str::to_string).ok_or(I2pError::Protocol(reply))
    }
}

#[cfg(test)]
mod tests;
