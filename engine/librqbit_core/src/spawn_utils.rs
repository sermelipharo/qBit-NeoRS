use std::{borrow::Cow, fmt::Display};

use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, trace};

/// Spawns a future with tracing instrumentation.
#[track_caller]
pub fn spawn<E: Display + Send + 'static>(
    span: tracing::Span,
    name: impl Into<Cow<'static, str>>,
    fut: impl std::future::Future<Output = Result<(), E>> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    let name = name.into();
    // No periodic "still running" trace: a 5 s timer per task woke the
    // process several times a second with many torrents and nothing to do.
    let fut = async move {
        trace!("started");
        match fut.await {
            Ok(_) => {
                trace!("finished");
            }
            Err(e) => {
                error!("{name} finished with error: {:#}", e)
            }
        }
    }
    .instrument(span);
    tokio::task::spawn(fut)
}

#[track_caller]
pub fn spawn_with_cancel<E: Display + Send + 'static>(
    span: tracing::Span,
    name: impl Into<Cow<'static, str>>,
    cancellation_token: CancellationToken,
    fut: impl std::future::Future<Output = Result<(), E>> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    spawn(span, name, async move {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                debug!("task cancelled");
                Ok(())
            },
            r = fut => r
        }
    })
}
