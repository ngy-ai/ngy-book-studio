//! Integration tests of the configurable worker scheduling.

use super::*;

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use crate::db;

#[test]
fn configured_concurrency_runs_two_queued_jobs_at_once() {
    let fixture = Fixture::new();
    fixture.add_second_book();
    let provider = Arc::new(MockProvider::default());
    provider.block_embeddings.store(true, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    coordinator.configure_scheduling(2, Duration::ZERO);

    // The provider is blocked, so two in-flight calls can only exist when
    // two jobs run at the same time.
    fixture.wait_until(
        "two concurrent provider calls",
        Duration::from_secs(10),
        |_| provider.embedding_calls.load(Ordering::SeqCst) >= 2,
    );
    provider.block_embeddings.store(false, Ordering::SeqCst);
    fixture.wait_until("both embedding jobs", Duration::from_secs(10), |fixture| {
        fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Succeeded) == 2
    });
}

#[test]
fn configured_interval_pauses_between_two_queued_jobs() {
    let fixture = Fixture::new();
    fixture.add_second_book();
    let provider = Arc::new(MockProvider::default());
    provider.block_embeddings.store(true, Ordering::SeqCst);
    let coordinator = fixture.coordinator(provider.clone());
    fixture.wait_until(
        "one running embedding job",
        Duration::from_secs(10),
        |fixture| fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Running) == 1,
    );
    // Configured while the first job is claimed, so the single worker reads
    // the pause after that job finishes.
    coordinator.configure_scheduling(1, Duration::from_millis(1_500));
    provider.block_embeddings.store(false, Ordering::SeqCst);
    fixture.wait_until(
        "the first embedding job",
        Duration::from_secs(10),
        |fixture| fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Succeeded) == 1,
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fixture.embedding_status_count(db::index_jobs::IndexJobStatus::Succeeded),
        1,
        "the configured pause must keep the worker idle between jobs"
    );
}
