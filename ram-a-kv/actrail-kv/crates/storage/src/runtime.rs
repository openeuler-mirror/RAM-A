//! 本文件将阻塞存储操作桥接到有背压的异步扫描，并管理周期清理。

use std::{sync::Arc, time::Duration};

use actrail_kv_artifacts::CapturedRequest;
use futures_util::stream;
use tokio::{
    sync::{mpsc, oneshot},
    task,
    time::{self, Instant, MissedTickBehavior},
};

use crate::{CaptureStream, RetentionConfig, StorageError};

pub(crate) struct ScanWriter {
    ready: Option<oneshot::Sender<()>>,
    rows: mpsc::Sender<CapturedRequest>,
}

impl ScanWriter {
    pub(crate) fn start(&mut self) -> bool {
        self.ready
            .take()
            .is_some_and(|ready| ready.send(()).is_ok())
    }

    pub(crate) fn send(&self, record: CapturedRequest) -> bool {
        self.rows.blocking_send(record).is_ok()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.rows.is_closed()
    }
}

pub(crate) async fn scan_blocking(
    run: impl FnOnce(ScanWriter) -> Result<(), StorageError> + Send + 'static,
) -> Result<CaptureStream, StorageError> {
    let (ready, started) = oneshot::channel();
    let (rows, receiver) = mpsc::channel(2);
    let worker = task::spawn_blocking(move || {
        run(ScanWriter {
            ready: Some(ready),
            rows,
        })
    });
    if started.await.is_err() {
        worker.await??;
        return Err(StorageError::SnapshotExpired);
    }

    let stream = stream::unfold(
        (receiver, Some(worker)),
        |(mut receiver, mut worker)| async move {
            if let Some(record) = receiver.recv().await {
                return Some((Ok(record), (receiver, worker)));
            }
            match worker
                .take()?
                .await
                .map_err(StorageError::from)
                .and_then(|result| result)
            {
                Ok(()) => None,
                Err(error) => Some((Err(error), (receiver, worker))),
            }
        },
    );
    Ok(Box::pin(stream))
}

pub(crate) fn schedule_retention<T: Send + Sync + 'static>(
    store: &Arc<T>,
    config: RetentionConfig,
    cleanup: fn(&T) -> Result<(), StorageError>,
) {
    let store = Arc::downgrade(store);
    let period = Duration::from_secs(config.cleanup_interval_seconds.get().into());
    tokio::spawn(async move {
        let mut interval = time::interval_at(Instant::now() + period, period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(store) = store.upgrade() else { break };
            let result = task::spawn_blocking(move || cleanup(&store)).await;
            if let Err(error) = result.map_err(StorageError::from).and_then(|result| result) {
                eprintln!("capture retention cleanup failed: {error}");
            }
        }
    });
}
