//! BitTorrent Message Stream Encryption / Protocol Encryption (MSE/PE).
//!
//! Implements the Vuze/libtorrent obfuscation handshake:
//!
//! ```text
//! 1 A->B: Ya, PadA
//! 2 B->A: Yb, PadB
//! 3 A->B: HASH('req1', S), HASH('req2', SKEY) xor HASH('req3', S),
//!         ENCRYPT(VC, crypto_provide, len(PadC), PadC, len(IA)), ENCRYPT(IA)
//! 4 B->A: ENCRYPT(VC, crypto_select, len(padD), padD), ENCRYPT2(payload)
//! 5 A->B: ENCRYPT2(payload)
//! ```
//!
//! `S` is the 768-bit Diffie-Hellman shared secret, `SKEY` the torrent info
//! hash, keys are `SHA1("keyA"|"keyB", S, SKEY)` with the first 1024 bytes of
//! RC4 keystream discarded. Like libtorrent with `allowed_enc_level = rc4`
//! (qBittorrent's setting) only RC4 is negotiated inside the encrypted
//! handshake; unencrypted peers use the plain BitTorrent handshake, allowed or
//! refused by [`EncryptionPolicy`].
//!
//! This is obfuscation against traffic shaping, not confidentiality: the DH
//! parameters are fixed by the protocol and peers are not authenticated.

mod rc4;
mod stream;

use num_bigint::BigUint;
use rand::{Rng, RngExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub use rc4::Rc4;
pub use stream::{MseParts, MseStream};

/// The plaintext BitTorrent handshake starts with these 20 bytes.
pub const BT_PROTOCOL_PREFIX: &[u8; 20] = b"\x13BitTorrent protocol";

const DH_PRIME_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A63A36210000000000090563";
const KEY_LEN: usize = 96;
const MAX_PAD: usize = 512;
const VC: [u8; 8] = [0; 8];
const CRYPTO_PLAINTEXT: u32 = 0x01;
const CRYPTO_RC4: u32 = 0x02;

/// qBittorrent's "Encryption mode" (libtorrent `pe_enabled` / `pe_forced` / `pe_disabled`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EncryptionPolicy {
    /// Accept and make both plaintext and encrypted connections; outgoing
    /// connections try encryption first ("Allow encryption").
    #[default]
    Enabled,
    /// Only encrypted connections ("Require encryption").
    Forced,
    /// Only plaintext connections ("Disable encryption").
    Disabled,
}

#[derive(Debug, thiserror::Error)]
pub enum MseError {
    #[error("I/O error during MSE handshake: {0}")]
    Io(#[from] std::io::Error),
    #[error("MSE synchronisation marker not found")]
    NoSync,
    #[error("unknown torrent in encrypted handshake")]
    UnknownTorrent,
    #[error("peer offered no acceptable crypto method")]
    NoCommonCrypto,
    #[error("invalid MSE handshake: {0}")]
    Invalid(&'static str),
    #[error("plaintext connection refused by encryption policy")]
    PlaintextRefused,
    #[error("encrypted connection refused by encryption policy")]
    EncryptedRefused,
}

pub type Result<T> = std::result::Result<T, MseError>;

fn prime() -> BigUint {
    BigUint::parse_bytes(DH_PRIME_HEX.as_bytes(), 16).expect("valid prime")
}

fn to_96(n: &BigUint) -> [u8; KEY_LEN] {
    let bytes = n.to_bytes_be();
    let mut out = [0u8; KEY_LEN];
    out[KEY_LEN - bytes.len()..].copy_from_slice(&bytes);
    out
}

struct KeyPair {
    private: BigUint,
    public: [u8; KEY_LEN],
}

impl KeyPair {
    fn generate() -> Self {
        let mut x = [0u8; 20];
        rand::rng().fill_bytes(&mut x);
        let private = BigUint::from_bytes_be(&x);
        let public = to_96(&BigUint::from(2u32).modpow(&private, &prime()));
        Self { private, public }
    }

    fn shared_secret(&self, peer_public: &[u8]) -> Result<[u8; KEY_LEN]> {
        let p = prime();
        let y = BigUint::from_bytes_be(peer_public);
        // Reject degenerate public keys (0, 1, p-1, >= p).
        if y <= BigUint::from(1u32) || y >= &p - 1u32 {
            return Err(MseError::Invalid("bad DH public key"));
        }
        Ok(to_96(&y.modpow(&self.private, &p)))
    }
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = sha1_smol::Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.digest().bytes()
}

/// `HASH('req2', SKEY)` — the receiver keeps this per torrent to identify
/// which torrent an incoming encrypted connection is for.
pub fn req2_hash(info_hash: &[u8; 20]) -> [u8; 20] {
    sha1(&[b"req2", info_hash])
}

fn random_pad() -> Vec<u8> {
    let mut rng = rand::rng();
    let len = rng.random_range(0..=MAX_PAD);
    let mut pad = vec![0u8; len];
    rng.fill_bytes(&mut pad);
    pad
}

/// Reads until `pattern` appears in the stream (searching at most `limit`
/// bytes); returns the bytes that followed it in the last read.
async fn sync_on<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>, pattern: &[u8], limit: usize) -> Result<Vec<u8>> {
    loop {
        if let Some(pos) = buf.windows(pattern.len()).position(|w| w == pattern) {
            let rest = buf.split_off(pos + pattern.len());
            return Ok(rest);
        }
        if buf.len() >= limit + pattern.len() {
            return Err(MseError::NoSync);
        }
        let mut chunk = [0u8; 1024];
        let want = (limit + pattern.len() - buf.len()).min(chunk.len());
        let n = s.read(&mut chunk[..want]).await?;
        if n == 0 {
            return Err(MseError::NoSync);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Reads exactly `n` bytes, first from `pending` then from the stream.
async fn take<S: AsyncRead + Unpin>(s: &mut S, pending: &mut Vec<u8>, n: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    let from_pending = n.min(pending.len());
    out.extend(pending.drain(..from_pending));
    if out.len() < n {
        let mut rest = vec![0u8; n - out.len()];
        s.read_exact(&mut rest).await?;
        out.extend_from_slice(&rest);
    }
    Ok(out)
}

/// Outgoing connection: performs the encrypted handshake as peer A for
/// `info_hash`. Returns an RC4 stream; the caller then sends the normal
/// BitTorrent handshake through it.
pub async fn initiate<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, info_hash: &[u8; 20]) -> Result<MseStream<S>> {
    let keys = KeyPair::generate();
    let mut step1 = keys.public.to_vec();
    step1.extend(random_pad());
    stream.write_all(&step1).await?;

    let mut yb = [0u8; KEY_LEN];
    stream.read_exact(&mut yb).await?;
    let s = keys.shared_secret(&yb)?;

    let mut enc = Rc4::new_mse(&sha1(&[b"keyA", &s, info_hash]));
    let mut dec = Rc4::new_mse(&sha1(&[b"keyB", &s, info_hash]));

    // Step 3.
    let req1 = sha1(&[b"req1", &s]);
    let req2 = req2_hash(info_hash);
    let req3 = sha1(&[b"req3", &s]);
    let mut msg = Vec::with_capacity(40 + 16);
    msg.extend_from_slice(&req1);
    msg.extend(req2.iter().zip(req3.iter()).map(|(a, b)| a ^ b));
    let mut tail = Vec::with_capacity(16);
    tail.extend_from_slice(&VC);
    tail.extend_from_slice(&CRYPTO_RC4.to_be_bytes());
    tail.extend_from_slice(&0u16.to_be_bytes()); // len(PadC)
    tail.extend_from_slice(&0u16.to_be_bytes()); // len(IA)
    enc.apply(&mut tail);
    msg.extend_from_slice(&tail);
    stream.write_all(&msg).await?;
    stream.flush().await?;

    // Step 4: find ENCRYPT(VC) after PadB.
    let mut expect_vc = VC;
    dec.clone().apply(&mut expect_vc);
    let mut buf = Vec::new();
    let mut pending = sync_on(&mut stream, &mut buf, &expect_vc, MAX_PAD).await?;
    let mut vc = VC;
    dec.apply(&mut vc); // advance past VC

    let mut hdr = take(&mut stream, &mut pending, 6).await?;
    dec.apply(&mut hdr);
    let select = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let pad_len = u16::from_be_bytes([hdr[4], hdr[5]]) as usize;
    if pad_len > MAX_PAD {
        return Err(MseError::Invalid("padD too long"));
    }
    let mut pad = take(&mut stream, &mut pending, pad_len).await?;
    dec.apply(&mut pad);
    if select != CRYPTO_RC4 {
        return Err(MseError::NoCommonCrypto);
    }
    // Whatever arrived after padD is already payload.
    dec.apply(&mut pending);
    Ok(MseStream::new(stream, Some(enc), Some(dec), pending))
}

/// Result of accepting an incoming connection.
pub enum Accepted<S> {
    /// The peer sent a plaintext BitTorrent handshake; the stream replays the
    /// bytes already read.
    Plaintext(MseStream<S>),
    /// Encrypted connection for `info_hash`.
    Encrypted { stream: MseStream<S>, info_hash: [u8; 20] },
}

/// Incoming connection: detects plaintext vs encrypted and performs the
/// receiver side of the handshake. `lookup` maps `HASH('req2', SKEY)` to the
/// info hash of a torrent we serve (see [`req2_hash`]).
pub async fn accept<S, F>(mut stream: S, policy: EncryptionPolicy, lookup: F) -> Result<Accepted<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Fn(&[u8; 20]) -> Option<[u8; 20]>,
{
    let mut first = [0u8; 20];
    stream.read_exact(&mut first).await?;
    if &first == BT_PROTOCOL_PREFIX {
        if policy == EncryptionPolicy::Forced {
            return Err(MseError::PlaintextRefused);
        }
        return Ok(Accepted::Plaintext(MseStream::new(stream, None, None, first.to_vec())));
    }
    if policy == EncryptionPolicy::Disabled {
        return Err(MseError::EncryptedRefused);
    }

    let mut ya = [0u8; KEY_LEN];
    ya[..20].copy_from_slice(&first);
    stream.read_exact(&mut ya[20..]).await?;

    let keys = KeyPair::generate();
    let s = keys.shared_secret(&ya)?;
    let mut step2 = keys.public.to_vec();
    step2.extend(random_pad());
    stream.write_all(&step2).await?;
    stream.flush().await?;

    // Step 3: skip PadA up to HASH('req1', S).
    let req1 = sha1(&[b"req1", &s]);
    let mut buf = Vec::new();
    let mut pending = sync_on(&mut stream, &mut buf, &req1, MAX_PAD).await?;

    let xored = take(&mut stream, &mut pending, 20).await?;
    let req3 = sha1(&[b"req3", &s]);
    let mut req2 = [0u8; 20];
    for i in 0..20 {
        req2[i] = xored[i] ^ req3[i];
    }
    let info_hash = lookup(&req2).ok_or(MseError::UnknownTorrent)?;

    let mut dec = Rc4::new_mse(&sha1(&[b"keyA", &s, &info_hash]));
    let mut enc = Rc4::new_mse(&sha1(&[b"keyB", &s, &info_hash]));

    let mut hdr = take(&mut stream, &mut pending, 14).await?;
    dec.apply(&mut hdr);
    if hdr[..8] != VC {
        return Err(MseError::Invalid("bad verification constant"));
    }
    let provide = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    let pad_len = u16::from_be_bytes([hdr[12], hdr[13]]) as usize;
    if pad_len > MAX_PAD {
        return Err(MseError::Invalid("padC too long"));
    }
    let mut pad = take(&mut stream, &mut pending, pad_len + 2).await?;
    dec.apply(&mut pad);
    let ia_len = u16::from_be_bytes([pad[pad_len], pad[pad_len + 1]]) as usize;
    if ia_len > 68 * 4 {
        return Err(MseError::Invalid("initial payload too long"));
    }
    let mut ia = take(&mut stream, &mut pending, ia_len).await?;
    dec.apply(&mut ia);

    if provide & CRYPTO_RC4 == 0 {
        // Only plaintext offered; qBittorrent allows RC4 only (pe_rc4).
        let _ = provide & CRYPTO_PLAINTEXT;
        return Err(MseError::NoCommonCrypto);
    }

    let mut reply = Vec::with_capacity(14);
    reply.extend_from_slice(&VC);
    reply.extend_from_slice(&CRYPTO_RC4.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    enc.apply(&mut reply);
    stream.write_all(&reply).await?;
    stream.flush().await?;

    // IA plus anything already buffered is the start of the payload.
    dec.apply(&mut pending);
    ia.extend_from_slice(&pending);
    Ok(Accepted::Encrypted { stream: MseStream::new(stream, Some(enc), Some(dec), ia), info_hash })
}

#[cfg(test)]
mod tests;
