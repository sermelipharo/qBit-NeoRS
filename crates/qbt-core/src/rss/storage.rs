//! Ordered asynchronous writer for the RSS files (qBittorrent's
//! `AsyncFileStorage`): writes are atomic, superseded writes to the same
//! file are coalesced, and `flush` waits until everything queued is on disk.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::logger::Logger;

enum Op {
    Write(PathBuf, Vec<u8>),
    Remove(PathBuf),
    Flush(oneshot::Sender<()>),
}

pub(crate) struct Store {
    tx: mpsc::UnboundedSender<Op>,
}

impl Store {
    pub fn spawn(rt: &tokio::runtime::Handle, logger: Arc<Logger>) -> (Self, JoinHandle<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let task = rt.spawn(writer(rx, logger));
        (Self { tx }, task)
    }

    pub fn write(&self, path: PathBuf, data: Vec<u8>) {
        let _ = self.tx.send(Op::Write(path, data));
    }

    pub fn remove(&self, path: PathBuf) {
        let _ = self.tx.send(Op::Remove(path));
    }

    /// Resolves when every operation queued before the call is done.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Op::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

async fn writer(mut rx: mpsc::UnboundedReceiver<Op>, logger: Arc<Logger>) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(op) = rx.try_recv() {
            batch.push(op);
        }
        // Keep only the last file operation per path, in order.
        let mut last: HashMap<PathBuf, usize> = HashMap::new();
        for (i, op) in batch.iter().enumerate() {
            if let Op::Write(p, _) | Op::Remove(p) = op {
                last.insert(p.clone(), i);
            }
        }
        let mut waiters = Vec::new();
        let mut jobs = Vec::new();
        for (i, op) in batch.into_iter().enumerate() {
            match op {
                Op::Write(p, data) if last.get(&p) == Some(&i) => jobs.push((p, Some(data))),
                Op::Remove(p) if last.get(&p) == Some(&i) => jobs.push((p, None)),
                Op::Flush(tx) => waiters.push(tx),
                _ => {}
            }
        }
        if !jobs.is_empty() {
            let result = tokio::task::spawn_blocking(move || {
                let mut errors = Vec::new();
                for (path, data) in jobs {
                    let res = match data {
                        Some(d) => crate::utils::fs::write_atomic(&path, &d).map_err(|e| e.to_string()),
                        None => match std::fs::remove_file(&path) {
                            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                            _ => Ok(()),
                        },
                    };
                    if let Err(e) = res {
                        errors.push(format!("Couldn't save RSS session data. File: \"{}\". Error: \"{e}\"", path.display()));
                    }
                }
                errors
            })
            .await;
            for e in result.unwrap_or_default() {
                logger.warning(e);
            }
        }
        for w in waiters {
            let _ = w.send(());
        }
    }
}
