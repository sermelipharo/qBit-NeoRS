//! Glue between the MSE/PE handshake (`librqbit-mse`) and the engine's split
//! peer streams: the handshake runs on a [`Duplex`] of the read and write
//! halves, then [`split_mse`] turns the result back into halves that decrypt
//! and encrypt with RC4.

use std::{
    collections::HashMap,
    io::IoSliceMut,
    pin::Pin,
    task::{Context, Poll},
};

use librqbit_core::hash_id::Id20;
use librqbit_mse::{MseParts, MseStream, Rc4};
use parking_lot::RwLock;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectored,
};

/// A read half and a write half used as one stream (for the handshake).
pub(crate) struct Duplex<R, W> {
    pub read: R,
    pub write: W,
}

impl<R: AsyncRead + Unpin, W: Unpin> AsyncRead for Duplex<R, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().read).poll_read(cx, buf)
    }
}

impl<R: Unpin, W: AsyncWrite + Unpin> AsyncWrite for Duplex<R, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().write).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_shutdown(cx)
    }
}

/// Read half after the MSE handshake: first returns the bytes read ahead
/// during the handshake, then decrypts (if RC4 was negotiated) what the
/// inner reader returns.
pub(crate) struct MseReader<R> {
    inner: R,
    dec: Option<Box<Rc4>>,
    prefix: Vec<u8>,
    prefix_pos: usize,
}

impl<R> MseReader<R> {
    pub fn new(inner: R, dec: Option<Rc4>, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            dec: dec.map(Box::new),
            prefix,
            prefix_pos: 0,
        }
    }

    fn take_prefix(&mut self, out: &mut [u8]) -> usize {
        let n = (self.prefix.len() - self.prefix_pos).min(out.len());
        out[..n].copy_from_slice(&self.prefix[self.prefix_pos..self.prefix_pos + n]);
        self.prefix_pos += n;
        if self.prefix_pos == self.prefix.len() {
            self.prefix = Vec::new();
            self.prefix_pos = 0;
        }
        n
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for MseReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.prefix_pos < this.prefix.len() {
            let n = this.take_prefix(buf.initialize_unfilled());
            buf.advance(n);
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        if let Some(dec) = this.dec.as_mut() {
            dec.apply(&mut buf.filled_mut()[before..]);
        }
        Poll::Ready(Ok(()))
    }
}

impl<R: AsyncReadVectored + Unpin> AsyncReadVectored for MseReader<R> {
    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        vec: &mut [IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.prefix_pos < this.prefix.len() {
            let mut total = 0;
            for slice in vec.iter_mut() {
                let n = this.take_prefix(slice);
                total += n;
                if n < slice.len() {
                    break;
                }
            }
            return Poll::Ready(Ok(total));
        }
        let Some(dec) = this.dec.as_mut() else {
            return Pin::new(&mut this.inner).poll_read_vectored(cx, vec);
        };
        // Some readers (uTP) advance the caller's slices in place, so the
        // bytes to decrypt can't be found through `vec` afterwards: read into
        // the first non-empty slice only, then decrypt exactly that.
        let Some(first) = vec.iter_mut().find(|s| !s.is_empty()) else {
            return Poll::Ready(Ok(0));
        };
        let mut rbuf = ReadBuf::new(first);
        std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, &mut rbuf))?;
        let n = rbuf.filled().len();
        dec.apply(rbuf.filled_mut());
        Poll::Ready(Ok(n))
    }
}

/// Upper bound of plaintext encrypted per write call (peer messages are at
/// most ~16 KiB; this only bounds the buffer).
const MAX_WRITE_CHUNK: usize = 32 * 1024;

/// Write half after an encrypted handshake.
///
/// RC4 is a stream cipher, so bytes are encrypted exactly once: a write
/// encrypts the caller's data into an internal buffer and only reports it as
/// written once all of the ciphertext reached the inner writer. While
/// returning `Pending` it keeps that ciphertext; the caller must retry with
/// the same data (as `write_all` does). This way nothing is ever left
/// unflushed between writes, so no explicit `flush` is needed.
pub(crate) struct MseWriter<W> {
    inner: W,
    enc: Box<Rc4>,
    buf: Vec<u8>,
    pos: usize,
    pending_len: usize,
}

impl<W> MseWriter<W> {
    pub fn new(inner: W, enc: Rc4) -> Self {
        Self {
            inner,
            enc: Box::new(enc),
            buf: Vec::new(),
            pos: 0,
            pending_len: 0,
        }
    }
}

impl<W: AsyncWrite + Unpin> MseWriter<W> {
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while self.pos < self.buf.len() {
            match std::task::ready!(Pin::new(&mut self.inner).poll_write(cx, &self.buf[self.pos..]))
            {
                Ok(0) => return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into())),
                Ok(n) => self.pos += n,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
        self.buf.clear();
        self.pos = 0;
        Poll::Ready(Ok(()))
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for MseWriter<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.buf.is_empty() {
            if data.is_empty() {
                return Poll::Ready(Ok(0));
            }
            let take = data.len().min(MAX_WRITE_CHUNK);
            this.buf.extend_from_slice(&data[..take]);
            this.enc.apply(&mut this.buf);
            this.pos = 0;
            this.pending_len = take;
        }
        std::task::ready!(this.poll_drain(cx))?;
        let n = std::mem::take(&mut this.pending_len);
        if this.buf.capacity() > 2 * MAX_WRITE_CHUNK {
            this.buf = Vec::new();
        }
        Poll::Ready(Ok(n.min(data.len())))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Turns a finished MSE handshake over a [`Duplex`] of boxed halves back into
/// boxed halves. Returns whether the payload is RC4-encrypted.
pub(crate) fn split_mse(
    stream: MseStream<Duplex<BoxAsyncReadVectored, BoxAsyncWrite>>,
) -> (BoxAsyncReadVectored, BoxAsyncWrite, bool) {
    let MseParts {
        inner,
        enc,
        dec,
        prefix,
    } = stream.into_parts();
    let Duplex { read, write } = inner;
    let encrypted = enc.is_some();
    let read: BoxAsyncReadVectored = if dec.is_none() && prefix.is_empty() {
        read
    } else {
        Box::new(MseReader::new(read, dec, prefix))
    };
    let write: BoxAsyncWrite = match enc {
        Some(enc) => Box::new(MseWriter::new(write, enc)),
        None => write,
    };
    (read, write, encrypted)
}

/// `HASH('req2', info_hash)` → info hash of every torrent in the session, to
/// identify incoming encrypted connections.
#[derive(Default)]
pub(crate) struct Req2Index {
    map: RwLock<HashMap<[u8; 20], Id20>>,
}

impl Req2Index {
    pub fn insert(&self, info_hash: Id20) {
        self.map
            .write()
            .insert(librqbit_mse::req2_hash(&info_hash.0), info_hash);
    }

    pub fn remove(&self, info_hash: Id20) {
        self.map
            .write()
            .remove(&librqbit_mse::req2_hash(&info_hash.0));
    }

    pub fn lookup(&self, req2: &[u8; 20]) -> Option<[u8; 20]> {
        self.map.read().get(req2).map(|h| h.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectored_traits::{AsyncReadVectoredExt, AsyncReadVectoredIntoCompat};
    use librqbit_mse::EncryptionPolicy;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn boxed(s: tokio::io::DuplexStream) -> (BoxAsyncReadVectored, BoxAsyncWrite) {
        let (r, w) = tokio::io::split(s);
        (Box::new(r.into_vectored_compat()), Box::new(w))
    }

    async fn pair(
        policy: EncryptionPolicy,
        plaintext_initiator: bool,
    ) -> (
        (BoxAsyncReadVectored, BoxAsyncWrite, bool),
        (BoxAsyncReadVectored, BoxAsyncWrite, bool),
    ) {
        // Small capacity: exercises partial writes and Pending (it must still
        // hold one handshake message, as both sides write at once).
        let (a, b) = tokio::io::duplex(1024);
        let info_hash = Id20::new([7; 20]);
        let index = Req2Index::default();
        index.insert(info_hash);
        let (ar, aw) = boxed(a);
        let (br, bw) = boxed(b);
        let initiator = async move {
            let d = Duplex {
                read: ar,
                write: aw,
            };
            if plaintext_initiator {
                let Duplex { read, write } = d;
                (read, write, false)
            } else {
                split_mse(librqbit_mse::initiate(d, &info_hash.0).await.unwrap())
            }
        };
        let acceptor = async move {
            let d = Duplex {
                read: br,
                write: bw,
            };
            match librqbit_mse::accept(d, policy, |h| index.lookup(h))
                .await
                .unwrap()
            {
                librqbit_mse::Accepted::Plaintext(s) => split_mse(s),
                librqbit_mse::Accepted::Encrypted { stream, info_hash } => {
                    assert_eq!(info_hash, [7; 20]);
                    split_mse(stream)
                }
            }
        };
        if plaintext_initiator {
            // The acceptor needs the first bytes to decide.
            let (mut a, b) = tokio::join!(
                async {
                    let mut a = initiator.await;
                    a.1.write_all(b"\x13BitTorrent protocol").await.unwrap();
                    a
                },
                acceptor
            );
            // Drop what we wrote from the acceptor's point of view later.
            a.2 = false;
            (a, b)
        } else {
            tokio::join!(initiator, acceptor)
        }
    }

    #[tokio::test]
    async fn encrypted_roundtrip_both_ways() {
        let ((mut ar, mut aw, ae), (mut br, mut bw, be)) =
            pair(EncryptionPolicy::Enabled, false).await;
        assert!(ae && be);
        let payload: Vec<u8> = (0..100_000u32)
            .map(|i| u8::try_from(i * 7 % 251).unwrap())
            .collect();
        let p2 = payload.clone();
        let writer = tokio::spawn(async move {
            aw.write_all(&p2).await.unwrap();
            aw.flush().await.unwrap();
            aw
        });
        let mut got = vec![0u8; payload.len()];
        let mut filled = 0;
        while filled < got.len() {
            let end = (filled + 1000).min(got.len());
            let (x, y) = got[filled..end].split_at_mut((end - filled) / 2);
            let n = br
                .read_vectored(&mut [IoSliceMut::new(x), IoSliceMut::new(y)])
                .await
                .unwrap();
            assert!(n > 0);
            filled += n;
        }
        assert_eq!(got, payload);
        let _aw = writer.await.unwrap();

        bw.write_all(b"pong").await.unwrap();
        let mut buf = [0u8; 4];
        ar.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn plaintext_prefix_replayed() {
        let ((_ar, _aw, _), (mut br, _bw, be)) = pair(EncryptionPolicy::Enabled, true).await;
        assert!(!be);
        let mut buf = [0u8; 20];
        br.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"\x13BitTorrent protocol");
    }
}
