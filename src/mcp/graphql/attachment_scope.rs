use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OnceCell, Semaphore};

use super::SharedClient;
use super::loaders::LoadError;
use crate::error::{Error, Result};

const INPUT_LIMIT: usize = 128 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 256 * 1024 * 1024;
const BLOB_LIMIT: usize = 1024;
static PROCESSING: Semaphore = Semaphore::const_new(2);

type BlobCell = Arc<OnceCell<std::result::Result<Arc<Vec<u8>>, LoadError>>>;

#[derive(Clone)]
pub(super) struct AttachmentScopes(Arc<Mutex<Arc<AttachmentScope>>>);

impl Default for AttachmentScopes {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Arc::new(AttachmentScope::new(
            INPUT_LIMIT,
            OUTPUT_LIMIT,
            BLOB_LIMIT,
        )))))
    }
}

impl AttachmentScopes {
    #[cfg(test)]
    pub(super) fn with_limits(input: usize, output: usize, blobs: usize) -> Self {
        Self(Arc::new(Mutex::new(Arc::new(AttachmentScope::new(
            input, output, blobs,
        )))))
    }

    pub(super) fn current(&self) -> Arc<AttachmentScope> {
        self.0.lock().unwrap().clone()
    }

    pub(super) fn renew(&self) {
        let mut scope = self.0.lock().unwrap();
        *scope = Arc::new(AttachmentScope::new(
            scope.input_limit,
            scope.output_limit,
            scope.blob_limit,
        ));
    }
}

pub(super) struct AttachmentScope {
    input: AtomicUsize,
    output: AtomicUsize,
    input_limit: usize,
    output_limit: usize,
    blob_limit: usize,
    blobs: Mutex<HashMap<String, BlobCell>>,
    downloads: Semaphore,
}

fn exceeded(resource: &str) -> Error {
    Error::Server(format!(
        "Attachment {resource} budget exceeded; select fewer payload fields or a smaller page"
    ))
}

impl AttachmentScope {
    fn new(input_limit: usize, output_limit: usize, blob_limit: usize) -> Self {
        Self {
            input: AtomicUsize::new(0),
            output: AtomicUsize::new(0),
            input_limit,
            output_limit,
            blob_limit,
            blobs: Mutex::new(HashMap::new()),
            downloads: Semaphore::new(4),
        }
    }

    fn charge_input(&self, bytes: usize) -> Result<()> {
        let used = self
            .input
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                // A failed transfer consumes the remaining budget too.
                Some(used.saturating_add(bytes))
            })
            .unwrap();
        if used <= self.input_limit && bytes <= self.input_limit.saturating_sub(used) {
            Ok(())
        } else {
            Err(exceeded("download"))
        }
    }

    pub(super) async fn bytes(
        &self,
        client: &SharedClient,
        blob_id: &str,
    ) -> std::result::Result<Arc<Vec<u8>>, LoadError> {
        let cell = {
            let mut blobs = self.blobs.lock().unwrap();
            if let Some(cell) = blobs.get(blob_id) {
                cell.clone()
            } else {
                if blobs.len() >= self.blob_limit {
                    return Err(Arc::new(exceeded("unique blob")));
                }
                blobs.entry(blob_id.into()).or_default().clone()
            }
        };
        cell.get_or_init(|| async {
            let _permit =
                self.downloads.acquire().await.map_err(|_| {
                    Arc::new(Error::Server("Attachment downloads unavailable".into()))
                })?;
            let client = client.lock().await.clone();
            client
                .download_blob_charged(blob_id, |bytes| self.charge_input(bytes))
                .await
                .map(Arc::new)
                .map_err(Arc::new)
        })
        .await
        .clone()
    }

    pub(super) fn reserve(self: &Arc<Self>, maximum: usize, minimum: usize) -> Result<Reservation> {
        let mut capacity = 0;
        self.output
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                capacity = maximum.min(self.output_limit.saturating_sub(used));
                (capacity >= minimum).then_some(used + capacity)
            })
            .map_err(|_| exceeded("payload"))?;
        Ok(Reservation {
            scope: self.clone(),
            capacity,
            retained: 0,
        })
    }
}

pub(super) struct Reservation {
    scope: Arc<AttachmentScope>,
    pub(super) capacity: usize,
    retained: usize,
}

impl Reservation {
    pub(super) fn commit(mut self, bytes: usize) -> Result<()> {
        if bytes > self.capacity {
            return Err(exceeded("payload"));
        }
        self.retained = bytes;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.scope
            .output
            .fetch_sub(self.capacity - self.retained, Ordering::Relaxed);
    }
}

pub(super) fn base64_size(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4 + 2
}

pub(super) async fn process<T: Send + 'static>(
    work: impl FnOnce() -> async_graphql::Result<T> + Send + 'static,
) -> async_graphql::Result<T> {
    let permit = PROCESSING.acquire().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| async_graphql::Error::new("Attachment processing failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_charges_remain_after_failure() {
        let scope = AttachmentScope::new(10, 20, 2);
        assert!(scope.charge_input(5).is_ok());
        assert!(scope.charge_input(6).is_err());
        assert!(scope.charge_input(1).is_err());
        assert_eq!(scope.input.load(Ordering::Relaxed), 12);
    }

    #[test]
    fn reservations_bound_concurrent_producers_and_charge_each_alias() {
        let scope = Arc::new(AttachmentScope::new(10, 20, 2));
        let a = scope.reserve(12, 2).unwrap();
        let b = scope.reserve(12, 2).unwrap();
        assert_eq!(b.capacity, 8);
        assert!(scope.reserve(1, 1).is_err());
        a.commit(10).unwrap();
        b.commit(8).unwrap();
        assert!(scope.reserve(3, 3).is_err());
        scope.reserve(2, 2).unwrap().commit(2).unwrap();
        assert_eq!(scope.output.load(Ordering::Relaxed), 20);
    }

    #[test]
    fn renewing_does_not_reset_inflight_scope() {
        let scopes = AttachmentScopes(Arc::new(Mutex::new(Arc::new(AttachmentScope::new(
            10, 20, 2,
        )))));
        let first = scopes.current();
        first.charge_input(10).unwrap();
        scopes.renew();
        scopes.current().charge_input(10).unwrap();
        assert!(first.charge_input(1).is_err());
    }

    #[tokio::test]
    async fn processing_permits_survive_cancelled_waiters() {
        let mut jobs = Vec::new();
        let mut releases = Vec::new();
        let mut completions = Vec::new();
        for _ in 0..2 {
            let (started, start) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let (completed, completion) = tokio::sync::oneshot::channel();
            jobs.push(tokio::spawn(process(move || {
                started.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                let _ = completed.send(());
                Ok(())
            })));
            tokio::time::timeout(std::time::Duration::from_secs(10), start)
                .await
                .unwrap()
                .unwrap();
            releases.push(release);
            completions.push(completion);
        }
        for job in jobs {
            job.abort();
            assert!(job.await.unwrap_err().is_cancelled());
        }
        assert_eq!(PROCESSING.available_permits(), 0);
        // Tokio remains responsive while both blocking jobs wait.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        for release in releases {
            release.send(()).unwrap();
        }
        for completion in completions {
            tokio::time::timeout(std::time::Duration::from_secs(10), completion)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
