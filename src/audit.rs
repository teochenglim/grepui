//! The audit trail (R-C5): one row per request that reads data or changes state, plus sign-in
//! events. Rows go through a bounded queue to a writer task that inserts them in batches through
//! `AppState::write` (so the same allowlist applies); the table has no update or delete path in
//! the API. With `--no-auth` there is no one to attribute to and nothing is recorded.

use crate::{
    api::AppState,
    store::build::{self, AuditRow},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock, Weak},
};
use tokio::sync::{mpsc, oneshot};

const QUEUE: usize = 4096;
const BATCH: usize = 100;

enum Msg {
    Row(String, AuditRow),
    Flush(oneshot::Sender<()>),
}

#[derive(Default)]
pub struct Audit {
    tx: OnceLock<mpsc::Sender<Msg>>,
}

pub fn new_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

impl Audit {
    fn sender(&self, s: &Arc<AppState>) -> &mpsc::Sender<Msg> {
        self.tx.get_or_init(|| {
            let (tx, mut rx) = mpsc::channel::<Msg>(QUEUE);
            let weak: Weak<AppState> = Arc::downgrade(s);
            tokio::spawn(async move {
                while let Some(first) = rx.recv().await {
                    let mut rows: BTreeMap<String, Vec<AuditRow>> = BTreeMap::new();
                    let mut flushes = vec![];
                    let take = |m: Msg, rows: &mut BTreeMap<String, Vec<AuditRow>>, fl: &mut Vec<oneshot::Sender<()>>| match m {
                        Msg::Row(o, r) => rows.entry(o).or_default().push(r),
                        Msg::Flush(f) => fl.push(f),
                    };
                    take(first, &mut rows, &mut flushes);
                    while rows.values().map(Vec::len).sum::<usize>() < BATCH {
                        match rx.try_recv() {
                            Ok(m) => take(m, &mut rows, &mut flushes),
                            Err(_) => break,
                        }
                    }
                    let Some(state) = weak.upgrade() else { return };
                    for (org, batch) in rows {
                        for chunk in batch.chunks(BATCH) {
                            match build::audit_put(state.meta_db(), &org, chunk) {
                                Ok(q) => {
                                    if let Err(e) = state.write(q).await {
                                        tracing::error!(error = %e.1, rows = chunk.len(), "could not write audit rows");
                                    }
                                }
                                Err(e) => tracing::error!(error = %e, "audit batch"),
                            }
                        }
                    }
                    for f in flushes {
                        let _ = f.send(());
                    }
                }
            });
            tx
        })
    }

    /// Queue a row. Waits when the queue is full rather than dropping it.
    pub async fn record(&self, s: &Arc<AppState>, org: &str, row: AuditRow) {
        if self
            .sender(s)
            .send(Msg::Row(org.to_string(), row))
            .await
            .is_err()
        {
            tracing::error!("the audit writer has stopped");
        }
    }

    /// Wait until everything queued so far has been written (tests, shutdown).
    pub async fn flush(&self, s: &Arc<AppState>) {
        let (tx, rx) = oneshot::channel();
        if self.sender(s).send(Msg::Flush(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// A sign-in or administration event outside the request layer.
pub async fn event(
    s: &Arc<AppState>,
    org: &str,
    user: &str,
    action: &str,
    outcome: &str,
    detail: &str,
) {
    let row = AuditRow {
        id: new_id(),
        username: user.chars().take(100).collect(),
        action: action.into(),
        outcome: outcome.into(),
        detail: detail.chars().take(500).collect(),
        ts_ms: s.now_ms(),
        ..Default::default()
    };
    if !s.cfg.no_auth {
        s.audit.record(s, org, row).await;
    }
}
