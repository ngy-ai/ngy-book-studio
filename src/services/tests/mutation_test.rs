//! Tests for the mutation turnstile and the default service graph.

use super::*;

#[test]
fn async_library_mutation_turnstile_is_fifo_without_blocking_pool_starvation() {
    const REQUESTS: usize = 128;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let queue = LibraryMutationQueue::new(runtime.handle());
    let reservations = (0..REQUESTS).map(|_| queue.reserve()).collect::<Vec<_>>();
    let order = Arc::new(StdMutex::new(Vec::with_capacity(REQUESTS)));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));

    runtime.block_on(async move {
        let mut handles = Vec::with_capacity(REQUESTS);
        // Polling the consumers in reverse order must not affect the API
        // boundary ticket order established above.
        for reservation in reservations.into_iter().rev() {
            let ticket = reservation.ticket;
            let order = Arc::clone(&order);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            handles.push(tokio::spawn(async move {
                let turn = reservation.enter().await.unwrap();
                tokio::task::spawn_blocking(move || {
                    let now_active = active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                    max_active.fetch_max(now_active, AtomicOrdering::SeqCst);
                    order.lock().unwrap().push(ticket);
                    std::thread::yield_now();
                    active.fetch_sub(1, AtomicOrdering::SeqCst);
                    drop(turn);
                })
                .await
                .unwrap();
            }));
        }

        tokio::time::timeout(Duration::from_secs(5), async {
            for handle in handles {
                handle.await.unwrap();
            }
        })
        .await
        .expect("queued mutations starved a one-thread blocking pool");

        assert_eq!(max_active.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            *order.lock().unwrap(),
            (0..REQUESTS as u64).collect::<Vec<_>>()
        );
    });
}

#[test]
fn composes_default_local_services_in_an_isolated_directory() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials).unwrap();

    let provider_settings = services.provider_settings().unwrap();
    assert_eq!(provider_settings.base_url, DEFAULT_OLLAMA_OPENAI_BASE_URL);
    assert_eq!(provider_settings.chat_model, "qwen3.5:0.8b");
    assert_eq!(provider_settings.embedding_model, "qwen3-embedding:0.6b");
    assert_eq!(provider_settings.vision_model, "qwen3.5:0.8b");
    assert_eq!(
        services.search().unwrap().embedding_model(),
        DEFAULT_EMBEDDING_MODEL
    );
    assert!(services.blob_store().root().starts_with(temp.path()));
    assert!(services.visual_jobs().is_some());

    let title = services
        .runtime()
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Service book", "Author")?.title))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();
    assert_eq!(title, "Service book");
}
