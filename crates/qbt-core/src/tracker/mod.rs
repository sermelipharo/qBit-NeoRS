//! Embedded HTTP tracker (Advanced → "Enable embedded tracker"), a port of
//! qBittorrent's `BitTorrent::Tracker`: BEP 3 announces with compact (BEP 23)
//! and IPv6 (BEP 7) peer lists, BEP 24 external IP, partial seeds (BEP 21
//! "paused" event). Peers live only in memory.
//!
//! Hardening beyond upstream (see docs/SECURITY.md):
//! * bounded connections, request head size and read timeout;
//! * the self-reported `ip` parameter is only honoured for requests coming
//!   from loopback/private networks, so the tracker can't be used to point
//!   swarms at arbitrary third-party hosts;
//! * peer/torrent tables are capped exactly like upstream (10 000 torrents,
//!   200 peers per torrent).

mod bencode;
mod http;
mod state;

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::context::CoreContext;

pub use state::TrackerState;

const MAX_CONNECTIONS: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs the tracker while the preference is on, restarting on port changes.
pub struct EmbeddedTracker {
    ctx: CoreContext,
    state: Arc<Mutex<TrackerState>>,
    server: Mutex<Option<(u16, JoinHandle<()>)>>,
}

impl EmbeddedTracker {
    pub fn launch(ctx: &CoreContext) -> Arc<Self> {
        let this = Arc::new(Self {
            ctx: ctx.clone(),
            state: Arc::new(Mutex::new(TrackerState::default())),
            server: Mutex::new(None),
        });
        let weak = Arc::downgrade(&this);
        let mut rx = ctx.prefs.subscribe();
        ctx.rt.spawn(async move {
            loop {
                let Some(t) = weak.upgrade() else { return };
                t.apply_preferences().await;
                drop(t);
                if rx.changed().await.is_err() {
                    return;
                }
            }
        });
        this
    }

    async fn apply_preferences(&self) {
        let prefs = self.ctx.prefs.current();
        let enabled = prefs.advanced.enable_embedded_tracker;
        let port = prefs.advanced.embedded_tracker_port.clamp(1, 65535) as u16;

        let running_port = self.server.lock().as_ref().map(|(p, _)| *p);
        if enabled && running_port == Some(port) {
            return;
        }
        self.stop();
        if !enabled {
            self.state.lock().clear();
            return;
        }
        let addr = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
        match bind_dual_stack(addr) {
            Ok(listener) => {
                self.ctx
                    .logger
                    .info(format!("Embedded Tracker: Now listening on IP: 0.0.0.0, port: {port}"));
                let state = self.state.clone();
                let handle = self.ctx.rt.spawn(serve(listener, state));
                *self.server.lock() = Some((port, handle));
            }
            Err(e) => self.ctx.logger.warning(format!(
                "Embedded Tracker: Unable to bind to IP: 0.0.0.0, port: {port}. Reason: {e}"
            )),
        }
    }

    pub fn stop(&self) {
        if let Some((_, h)) = self.server.lock().take() {
            h.abort();
        }
    }

    /// Port currently served, if running.
    pub fn port(&self) -> Option<u16> {
        self.server.lock().as_ref().map(|(p, _)| *p)
    }
}

impl Drop for EmbeddedTracker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn bind_dual_stack(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None)?;
    socket.set_only_v6(false)?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    TcpListener::from_std(socket.into())
}

/// Accept loop; public for tests.
pub async fn serve(listener: TcpListener, state: Arc<Mutex<TrackerState>>) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            // Over capacity: drop the connection instead of queueing it.
            drop(stream);
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(REQUEST_TIMEOUT, handle_connection(stream, peer, state)).await;
        });
    }
}

async fn handle_connection(mut stream: TcpStream, peer: SocketAddr, state: Arc<Mutex<TrackerState>>) {
    let response = match http::read_request(&mut stream).await {
        Ok(req) => http::route(&req, peer, &state),
        Err(status) => http::Response::status(status),
    };
    let _ = stream.write_all(&response.into_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests;
