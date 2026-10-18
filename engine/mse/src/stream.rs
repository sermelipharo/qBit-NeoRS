use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::rc4::Rc4;

/// A stream after the MSE handshake: RC4 in both directions when encryption
/// was negotiated, or a pass-through for plaintext peers. Bytes that were read
/// ahead during the handshake (`prefix`) are returned first.
pub struct MseStream<S> {
    inner: S,
    enc: Option<Rc4>,
    dec: Option<Rc4>,
    prefix: Vec<u8>,
    prefix_pos: usize,
    /// Encrypted bytes accepted from the caller but not yet written.
    out: Vec<u8>,
    out_pos: usize,
}

/// Upper bound of buffered ciphertext; callers get `Pending` beyond it.
const MAX_OUT: usize = 256 * 1024;

impl<S> MseStream<S> {
    pub(crate) fn new(inner: S, enc: Option<Rc4>, dec: Option<Rc4>, prefix: Vec<u8>) -> Self {
        Self { inner, enc, dec, prefix, prefix_pos: 0, out: Vec::new(), out_pos: 0 }
    }

    /// True when RC4 is applied to the payload.
    pub fn is_encrypted(&self) -> bool {
        self.enc.is_some()
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    /// Splits the stream after the handshake into the inner stream, the
    /// outgoing and incoming RC4 states (`None` for plaintext peers) and the
    /// bytes already read but not yet returned (they come before anything
    /// read from the inner stream). Callers that need separate read and
    /// write halves (e.g. a TCP stream split in two) build their own
    /// wrappers from these parts.
    pub fn into_parts(self) -> MseParts<S> {
        debug_assert!(self.out_pos >= self.out.len(), "unwritten ciphertext");
        let mut prefix = self.prefix;
        prefix.drain(..self.prefix_pos);
        MseParts { inner: self.inner, enc: self.enc, dec: self.dec, prefix }
    }
}

/// See [`MseStream::into_parts`].
pub struct MseParts<S> {
    pub inner: S,
    /// Encrypts what we send (RC4 keystream already past the discarded 1024 bytes).
    pub enc: Option<Rc4>,
    /// Decrypts what we receive.
    pub dec: Option<Rc4>,
    /// Already decrypted payload bytes read ahead during the handshake.
    pub prefix: Vec<u8>,
}

impl<S: AsyncRead + Unpin> AsyncRead for MseStream<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.prefix_pos < this.prefix.len() {
            let n = (this.prefix.len() - this.prefix_pos).min(buf.remaining());
            buf.put_slice(&this.prefix[this.prefix_pos..this.prefix_pos + n]);
            this.prefix_pos += n;
            if this.prefix_pos == this.prefix.len() {
                this.prefix = Vec::new();
                this.prefix_pos = 0;
            }
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if let Some(dec) = this.dec.as_mut() {
                    dec.apply(&mut buf.filled_mut()[before..]);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: AsyncWrite + Unpin> MseStream<S> {
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.out_pos < self.out.len() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.out[self.out_pos..]) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                Poll::Ready(Ok(n)) => self.out_pos += n,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        self.out.clear();
        self.out_pos = 0;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for MseStream<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.enc.is_none() {
            return Pin::new(&mut this.inner).poll_write(cx, data);
        }
        // Keep the ciphertext buffer bounded: drain before accepting more.
        if this.out.len() - this.out_pos >= MAX_OUT {
            match this.poll_drain(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        let take = data.len().min(MAX_OUT);
        let start = this.out.len();
        this.out.extend_from_slice(&data[..take]);
        if let Some(enc) = this.enc.as_mut() {
            enc.apply(&mut this.out[start..]);
        }
        // Opportunistically push bytes out; errors surface on the next call.
        if let Poll::Ready(Err(e)) = this.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(take))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.poll_drain(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut this.inner).poll_flush(cx),
            other => other,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.poll_drain(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut this.inner).poll_shutdown(cx),
            other => other,
        }
    }
}
