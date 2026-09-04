//! Dedicated asynchronous runtime for storage, parsing, indexing and model I/O.

use std::{future::Future, sync::Arc};

use anyhow::{Context as _, Result};
use tokio::runtime::{Builder, Runtime};

#[derive(Clone)]
pub struct IoRuntime {
    inner: Arc<RuntimeOwner>,
}

struct RuntimeOwner {
    runtime: Option<Runtime>,
}

impl RuntimeOwner {
    fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("I/O runtime is available until its final owner is dropped")
    }
}

impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        // Tokio deliberately panics when a blocking Runtime destructor runs
        // inside any asynchronous runtime. AppServices may be released by a
        // GPUI callback or an async test, so select its non-blocking shutdown
        // path in that context. Outside a runtime, normal Drop waits for worker
        // shutdown and remains the preferred path.
        if tokio::runtime::Handle::try_current().is_ok() {
            runtime.shutdown_background();
        } else {
            drop(runtime);
        }
    }
}

impl std::fmt::Debug for IoRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("IoRuntime").finish_non_exhaustive()
    }
}

impl IoRuntime {
    pub fn new(worker_threads: usize) -> Result<Self> {
        if worker_threads == 0 {
            anyhow::bail!("I/O runtime needs at least one worker thread");
        }
        let inner = Builder::new_multi_thread()
            .worker_threads(worker_threads)
            .thread_name("moye-io")
            .enable_all()
            .build()
            .context("failed to create the application I/O runtime")?;
        Ok(Self {
            inner: Arc::new(RuntimeOwner {
                runtime: Some(inner),
            }),
        })
    }

    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.inner.runtime().spawn(future)
    }

    /// Intended for process startup, tests and background worker threads only.
    /// GPUI render/entity update callbacks must use [`Self::spawn`] instead.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.inner.runtime().block_on(future)
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.inner.runtime().handle().clone()
    }
}

impl Default for IoRuntime {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2)
            .clamp(2, 8);
        Self::new(workers).expect("the Tokio I/O runtime should initialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_io_on_a_dedicated_runtime() {
        let runtime = IoRuntime::new(1).unwrap();
        let answer = runtime.block_on(async {
            tokio::task::yield_now().await;
            42
        });
        assert_eq!(answer, 42);
    }

    #[tokio::test]
    async fn can_release_last_owner_inside_an_async_runtime() {
        let runtime = IoRuntime::new(1).unwrap();
        assert_eq!(runtime.spawn(async { 7 }).await.unwrap(), 7);
        drop(runtime);
    }
}
