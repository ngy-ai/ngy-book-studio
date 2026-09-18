//! Provider settings, remote confirmation and search gating.

use super::*;

#[test]
pub(crate) fn web_search_stays_off_until_enabled_and_confirmed() {
    let mut settings = ProviderSettings::default();
    assert!(settings.web_search_config(None).unwrap().is_none());

    // A loopback endpoint needs no acknowledgement.
    settings.web_search_enabled = true;
    settings.web_search_url_template =
        "http://127.0.0.1:8080/search?q={query}&format=json".to_string();
    assert!(settings.validate().is_ok());

    // The key comes from the caller (credential store) and is not persisted.
    let config = settings
        .web_search_config(Some("credential-only".to_string()))
        .unwrap()
        .expect("loopback web search config");
    assert_eq!(config.api_key.as_deref(), Some("credential-only"));

    // A remote endpoint is rejected until it is explicitly confirmed.
    settings.web_search_url_template = "https://api.example.test/search?q={query}".to_string();
    assert!(settings.validate().is_err());

    settings.web_search_confirmed_remote_endpoint =
        normalize_web_endpoint(&settings.web_search_url_template).unwrap();
    settings.web_search_remote_confirmed = true;
    assert!(settings.validate().is_ok());

    // Changing the endpoint invalidates the acknowledgement again.
    settings.web_search_url_template = "https://other.example.test/search?q={query}".to_string();
    assert!(settings.validate().is_err());
}

#[test]
pub(crate) fn remote_confirmation_is_bound_to_the_exact_canonical_endpoint() {
    let mut settings = ProviderSettings {
        base_url: "https://models-a.example.test/v1".to_string(),
        chat_model: "chat-test".to_string(),
        embedding_model: "embed-test".to_string(),
        vision_model: "vision-test".to_string(),
        remote_content_confirmed: true,
        allow_insecure_remote_http: false,
        confirmed_remote_endpoint: "https://models-a.example.test/v1/".to_string(),
        request_timeout_secs: 30,
        ..Default::default()
    };
    settings.validate().unwrap();

    settings.base_url = "https://models-b.example.test/v1".to_string();
    assert!(settings.validate().is_err());

    settings.confirmed_remote_endpoint = "https://models-b.example.test/v1/".to_string();
    settings.validate().unwrap();
}

#[test]
pub(crate) fn persisted_provider_contract_does_not_accept_legacy_unbound_confirmation() {
    let legacy = serde_json::json!({
        "base_url": "https://legacy-remote.example.test/v1",
        "chat_model": "chat-test",
        "embedding_model": "embed-test",
        "vision_model": "vision-test",
        "remote_content_confirmed": true,
        "allow_insecure_remote_http": false,
        "request_timeout_secs": 30
    });
    assert!(serde_json::from_value::<ProviderSettings>(legacy).is_err());
}

#[test]
pub(crate) fn background_job_snapshots_are_scoped_deduplicated_and_include_progress() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();
    let book = runtime
        .block_on(async {
            services
                .spawn_library(|library| library.create_book("Tasks", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    let jobs = runtime
        .block_on(services.background_jobs_for_books(vec![book.id.clone(), book.id.clone()]))
        .unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(
        jobs.iter().map(|job| job.kind.as_str()).collect::<Vec<_>>(),
        vec!["visual_render", "vision", "embedding"]
    );
    let visual = jobs.iter().find(|job| job.kind == "visual_render").unwrap();
    assert_eq!(visual.progress.total, None);
    assert!(
        runtime
            .block_on(services.background_jobs_for_books(Vec::new()))
            .unwrap()
            .is_empty()
    );
}

#[test]
pub(crate) fn background_job_snapshots_use_actual_rendered_pages_instead_of_content_unit_count() {
    use db::index_jobs::{IndexJob, IndexJobStatus};

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for status in [
        IndexJobStatus::Queued,
        IndexJobStatus::Running,
        IndexJobStatus::Paused,
        IndexJobStatus::Failed,
        IndexJobStatus::Cancelled,
        IndexJobStatus::Succeeded,
    ] {
        let job = IndexJob {
            id: "visual-render:three-chapters".into(),
            book_id: "book".into(),
            source_id: Some("source".into()),
            kind: "visual_render".into(),
            status,
            pause_requested: false,
            cancel_requested: false,
            attempts: 1,
            cursor_json: serde_json::json!({
                "completed_pages": 7,
                "spec": { "unit_ids": ["chapter-1", "chapter-2", "chapter-3"] }
            })
            .to_string(),
            error: None,
            created_at: 1,
            updated_at: 2,
            started_at: Some(1),
            finished_at: (status == IndexJobStatus::Succeeded).then_some(2),
        };
        let snapshot = background_job_snapshot(&conn, job).unwrap();
        assert_eq!(snapshot.progress.completed, 7, "{status:?}");
        assert_eq!(
            snapshot.progress.total,
            (status == IndexJobStatus::Succeeded).then_some(7),
            "{status:?} must not treat three chapters as three pages"
        );
    }
}

#[test]
pub(crate) fn configuring_a_default_language_enqueues_translation_for_existing_books() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();

    // This book is created with translation switched off (the shipped
    // default is Simplified Chinese), so nothing is queued for it yet.
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    runtime
        .block_on(services.configure_provider(settings, ApiKeyUpdate::Keep))
        .unwrap();

    let book = runtime
        .block_on(async {
            services
                .spawn_library(|library| library.create_book("Translated", "Author"))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();

    // Translation is off: nothing is queued.
    let jobs = runtime
        .block_on(services.background_jobs_for_books(vec![book.id.clone()]))
        .unwrap();
    assert!(jobs.iter().all(|job| job.kind != "translation"));

    // Saving a target language enqueues the books that were imported while
    // translation was off.
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("zh-Hans".to_string());
    runtime
        .block_on(services.configure_provider(settings, ApiKeyUpdate::Keep))
        .unwrap();

    let jobs = runtime
        .block_on(services.background_jobs_for_books(vec![book.id.clone()]))
        .unwrap();
    let translation = jobs
        .iter()
        .find(|job| job.kind == "translation")
        .expect("saving a default language must enqueue a translation job");
    assert_eq!(
        translation.status,
        BackgroundJobStatus::Paused,
        "auto-run is disabled by default, so the new task starts paused"
    );

    // The durable job survives a restart and is reconciled again on startup.
    drop(services);
    let reopened =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let jobs = reopened
        .runtime()
        .block_on(reopened.background_jobs_for_books(vec![book.id.clone()]))
        .unwrap();
    assert!(
        jobs.iter().any(|job| job.kind == "translation"),
        "startup reconciliation keeps one translation job per book and language"
    );
}
