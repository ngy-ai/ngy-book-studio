//! The per-object publication gate that serializes writers.

use super::*;

/// Serializes the two halves of publishing and reclaiming content-addressed
/// objects for one canonical store root.
///
/// A publisher holds this gate from the first immutable `put` through the
/// SQLite transaction that installs its references. A reclaimer holds it while
/// re-checking SQLite immediately before deleting bytes. The process registry
/// makes independently opened [`LocalBlobStore`] handles for the same root use
/// the same gate, while `Clone` remains a cheap `Arc` clone.
#[derive(Clone)]
pub(crate) struct BlobPublicationLock {
    pub(crate) gate: Arc<PublicationGate>,
    #[cfg(test)]
    pub(crate) next_acquire_observer: Arc<StdMutex<Option<std::sync::mpsc::Sender<()>>>>,
}

impl fmt::Debug for BlobPublicationLock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlobPublicationLock")
            .finish_non_exhaustive()
    }
}

impl BlobPublicationLock {
    pub(crate) fn for_store(store: &LocalBlobStore) -> Result<Self> {
        let registry = PUBLICATION_GATES.get_or_init(|| StdMutex::new(HashMap::new()));
        let mut registry = registry
            .lock()
            .map_err(|_| anyhow::anyhow!("Blob publication lock registry is poisoned"))?;
        registry.retain(|_, gate| gate.strong_count() != 0);
        let gate = match registry.get(&store.canonical_root).and_then(Weak::upgrade) {
            Some(gate) => gate,
            None => {
                let gate = Arc::new(PublicationGate::new(()));
                registry.insert(store.canonical_root.clone(), Arc::downgrade(&gate));
                gate
            }
        };
        Ok(Self {
            gate,
            #[cfg(test)]
            next_acquire_observer: Arc::new(StdMutex::new(None)),
        })
    }

    pub(crate) async fn acquire(&self) -> tokio::sync::OwnedMutexGuard<()> {
        #[cfg(test)]
        if let Ok(mut observer) = self.next_acquire_observer.lock()
            && let Some(observer) = observer.take()
        {
            let _ = observer.send(());
        }
        Arc::clone(&self.gate).lock_owned().await
    }

    #[cfg(test)]
    pub(crate) fn observe_next_acquire(&self, observer: std::sync::mpsc::Sender<()>) {
        *self
            .next_acquire_observer
            .lock()
            .expect("publication acquire observer lock is available") = Some(observer);
    }
}
