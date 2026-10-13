//! Tests for provider settings, endpoint routing and credentials.

use super::*;

#[test]
fn projected_mutations_follow_commit_order_and_reads_do_not_advance_generation() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();
    assert_eq!(services.library_generation(), 0);

    let initial_count = runtime
        .block_on(async {
            services
                .spawn_library_read(|library| Ok(library.books().len()))
                .await
                .context("library read worker stopped")?
        })
        .unwrap();
    assert_eq!(initial_count, 0);
    assert_eq!(services.library_generation(), 0);

    let first = runtime
        .block_on(async {
            services
                .spawn_library_projected(|library| library.create_book("Projected one", "Author"))
                .await
                .context("first projected mutation stopped")?
        })
        .unwrap();
    assert_eq!(first.generation, 1);
    assert_eq!(first.snapshot.books().len(), 1);
    assert_eq!(services.library_generation(), 1);

    let error = runtime
        .block_on(async {
            services
                .spawn_library_projected::<(), _>(|_| anyhow::bail!("expected failure"))
                .await
                .context("failed projected mutation stopped")?
        })
        .unwrap_err();
    assert!(error.to_string().contains("expected failure"));
    assert_eq!(services.library_generation(), 1);

    let second = runtime
        .block_on(async {
            services
                .spawn_library_projected(|library| library.create_book("Projected two", "Author"))
                .await
                .context("second projected mutation stopped")?
        })
        .unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(second.snapshot.books().len(), 2);
    assert_eq!(services.library_generation(), 2);
}

#[test]
fn provider_settings_survive_restart_without_persisting_the_api_key() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let settings = ProviderSettings {
        base_url: "https://models.example.test/v1".to_string(),
        chat_model: "chat-test".to_string(),
        chat_generation: ChatGenerationSettings {
            temperature: None,
            top_p: Some(0.8),
            max_output_tokens: 512,
            presence_penalty: Some(0.3),
            frequency_penalty: Some(-0.4),
        },
        translation_generation: TranslationGenerationSettings {
            temperature: Some(0.7),
            top_p: None,
            max_output_tokens: 8192,
            presence_penalty: None,
            frequency_penalty: Some(0.2),
        },
        auto_run_background_jobs: false,
        background_job_concurrency: 3,
        background_job_interval_ms: 250,
        pdf_compact_reading: true,
        use_proxy: false,
        embedding_model: "embed-test".to_string(),
        vision_model: "vision-test".to_string(),
        remote_content_confirmed: true,
        allow_insecure_remote_http: false,
        confirmed_remote_endpoint: "https://models.example.test/v1/".to_string(),
        request_timeout_secs: 30,
        ..Default::default()
    };
    block_on_without_tokio(services.configure_provider(
        settings.clone(),
        ApiKeyUpdate::Set("credential-only-secret".to_string()),
    ))
    .unwrap();
    assert_eq!(
        credentials
            .api_key(
                normalize_provider_base_url(&settings.base_url)
                    .unwrap()
                    .as_str()
            )
            .unwrap()
            .as_deref(),
        Some("credential-only-secret")
    );

    let row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        PROVIDER_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert!(!row.value_json.contains("credential-only-secret"));
    assert!(!row.value_json.to_ascii_lowercase().contains("api_key"));
    assert!(!row.value_json.contains("chat_generation"));
    assert!(!row.value_json.contains("auto_run_background_jobs"));
    assert!(!row.value_json.contains("pdf_compact_reading"));
    assert!(!row.value_json.contains("use_proxy"));
    assert!(!row.value_json.contains("endpoint_routing"));
    let generation_row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        CHAT_GENERATION_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<ChatGenerationSettings>(&generation_row.value_json).unwrap(),
        settings.chat_generation
    );
    // 翻译的生成参数有自己的行，两组不会互相覆盖。
    let translation_generation_row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        TRANSLATION_GENERATION_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<TranslationGenerationSettings>(
            &translation_generation_row.value_json
        )
        .unwrap(),
        settings.translation_generation
    );
    let background_job_row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        BACKGROUND_JOB_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<PersistedBackgroundJobSettings>(&background_job_row.value_json)
            .unwrap(),
        PersistedBackgroundJobSettings {
            auto_run: false,
            concurrency: 3,
            interval_ms: 250,
        }
    );
    let pdf_reader_row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        PDF_READER_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<PersistedPdfReaderSettings>(&pdf_reader_row.value_json).unwrap(),
        PersistedPdfReaderSettings {
            compact_reading: true
        }
    );
    let network_row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        NETWORK_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<PersistedNetworkSettings>(&network_row.value_json).unwrap(),
        PersistedNetworkSettings { use_proxy: false }
    );
    drop(services);

    let reopened = AppServices::open_with_credentials(temp.path(), credentials).unwrap();
    assert_eq!(reopened.provider_settings().unwrap(), settings);
    assert_eq!(reopened.search().unwrap().embedding_model(), "embed-test");
}

/// 阅读窗口的语速下拉走的是只写 `speech.preferences.v1` 的轻量路径：它要立刻作用于
/// 内存快照（下一次朗读读的就是它）、保留同一条设置里的音色，并在重启后仍然存在。
#[test]
fn changing_the_speech_rate_updates_the_snapshot_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let mut settings = services.provider_settings().unwrap();
    settings.speech_voice_id = "voice-keep".to_string();
    settings.speech_rate = 0.75;
    block_on_without_tokio(services.configure_providers(settings, BTreeMap::new())).unwrap();

    block_on_without_tokio(services.set_speech_rate(1.5)).unwrap();
    let snapshot = services.provider_settings().unwrap();
    assert_eq!(snapshot.speech_rate, 1.5);
    assert_eq!(snapshot.speech_voice_id, "voice-keep");

    // 越界值按 `SpeechRequest` 的范围钳制，与读取设置时的口径一致。
    block_on_without_tokio(services.set_speech_rate(99.0)).unwrap();
    let clamped = crate::speech::SpeechRequest::RATE_MAX;
    let row = db::settings::get(
        &db::open_conn(services.database_path()).unwrap(),
        SPEECH_SETTINGS_KEY,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<PersistedSpeechSettings>(&row.value_json).unwrap(),
        PersistedSpeechSettings {
            voice_id: "voice-keep".to_string(),
            rate: clamped,
        }
    );
    drop(services);

    let reopened = AppServices::open_with_credentials(temp.path(), credentials).unwrap();
    let saved = reopened.provider_settings().unwrap();
    assert_eq!(saved.speech_rate, clamped);
    assert_eq!(saved.speech_voice_id, "voice-keep");
}

#[test]
fn endpoint_routing_rejects_missing_duplicate_and_unconfirmed_endpoints() {
    let settings = routed_settings();
    settings.validate().unwrap();
    assert_eq!(
        settings.endpoint_for(ModelRole::Chat).unwrap().id,
        DEFAULT_ENDPOINT_ID
    );
    assert_eq!(
        settings.endpoint_for(ModelRole::Embedding).unwrap().id,
        "embed"
    );
    assert_eq!(
        settings.endpoint_for(ModelRole::Vision).unwrap().id,
        "vision"
    );

    let mut dangling = settings.clone();
    dangling.endpoint_routing.embedding_endpoint_id = "removed".into();
    assert!(dangling.validate().is_err());
    let mut duplicate_id = settings.clone();
    duplicate_id.endpoint_routing.additional_endpoints[0].id = DEFAULT_ENDPOINT_ID.into();
    assert!(duplicate_id.validate().is_err());
    let mut duplicate_url = settings.clone();
    duplicate_url.endpoint_routing.additional_endpoints[0].base_url =
        settings.base_url.trim_end_matches('/').into();
    assert!(
        duplicate_url
            .validate()
            .unwrap_err()
            .to_string()
            .contains("地址重复")
    );
    let mut remote = settings.clone();
    let endpoint = &mut remote.endpoint_routing.additional_endpoints[0];
    endpoint.base_url = "https://models.example.test/v1".into();
    endpoint.remote_content_confirmed = true;
    endpoint.confirmed_remote_endpoint = "https://previous.example.test/v1/".into();
    assert!(remote.validate().is_err());
    remote.endpoint_routing.additional_endpoints[0].confirmed_remote_endpoint =
        "https://models.example.test/v1/".into();
    remote.validate().unwrap();
}

#[test]
fn routed_endpoint_identity_ignores_unrelated_chat_and_tracks_its_own_url() {
    let settings = routed_settings();
    let embedding = embedding_execution_identity(&settings).unwrap();
    let vision = vision_execution_identity(&settings).unwrap();
    let mut changed = settings.clone();
    changed.base_url = "http://127.0.0.1:21437/v1/".into();
    changed.chat_model = "changed-chat".into();
    changed.endpoint_routing.default_endpoint_name = "Renamed".into();
    assert_eq!(embedding_execution_identity(&changed).unwrap(), embedding);
    assert_eq!(vision_execution_identity(&changed).unwrap(), vision);
    changed.endpoint_routing.additional_endpoints[0].base_url = "http://127.0.0.1:21438/v1".into();
    assert_ne!(embedding_execution_identity(&changed).unwrap(), embedding);
    assert_eq!(vision_execution_identity(&changed).unwrap(), vision);
    changed.endpoint_routing.additional_endpoints[1].base_url = "http://127.0.0.1:21439/v1".into();
    assert_ne!(vision_execution_identity(&changed).unwrap(), vision);
}

#[test]
fn routing_and_independent_keys_survive_restart_without_copying_changed_urls() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let settings = routed_settings();
    let updates = BTreeMap::from([
        (
            DEFAULT_ENDPOINT_ID.into(),
            ApiKeyUpdate::Set("default-secret".into()),
        ),
        ("embed".into(), ApiKeyUpdate::Set("embed-secret".into())),
        ("vision".into(), ApiKeyUpdate::Set("vision-secret".into())),
    ]);
    block_on_without_tokio(services.configure_providers(settings.clone(), updates)).unwrap();
    for (id, expected) in [
        (DEFAULT_ENDPOINT_ID, "default-secret"),
        ("embed", "embed-secret"),
        ("vision", "vision-secret"),
    ] {
        let endpoint = settings.endpoint_by_id(id).unwrap();
        let target = normalize_provider_base_url(&endpoint.base_url).unwrap();
        assert_eq!(
            credentials.api_key(target.as_str()).unwrap().as_deref(),
            Some(expected)
        );
    }
    let rows = db::settings::list(&db::open_conn(services.database_path()).unwrap()).unwrap();
    assert!(rows.iter().all(|row| !row.value_json.contains("-secret")));
    assert!(
        rows.iter()
            .any(|row| row.key == ENDPOINT_ROUTING_SETTINGS_KEY)
    );
    drop(services);
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    assert_eq!(services.provider_settings().unwrap(), settings);
    let mut changed = settings.clone();
    changed.endpoint_routing.additional_endpoints[0].base_url = "http://127.0.0.1:21438/v1".into();
    block_on_without_tokio(services.configure_providers(changed.clone(), BTreeMap::new())).unwrap();
    assert!(
        credentials
            .api_key("http://127.0.0.1:21435/v1/")
            .unwrap()
            .is_none()
    );
    assert!(
        credentials
            .api_key("http://127.0.0.1:21438/v1/")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        credentials
            .api_key("http://127.0.0.1:21436/v1/")
            .unwrap()
            .as_deref(),
        Some("vision-secret")
    );
    assert_eq!(
        load_provider_settings(services.database_path()).unwrap(),
        changed
    );
}

#[test]
fn routing_save_failure_rolls_back_all_credentials_and_setting_rows() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let previous = services.provider_settings().unwrap();
    let conn = db::open_conn(services.database_path()).unwrap();
    let previous_rows = db::settings::list(&conn).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER test_endpoint_routing_save_failure
             BEFORE INSERT ON settings
             WHEN NEW.key = 'ai.openai_compatible.endpoint_routing.v1'
             BEGIN SELECT RAISE(ABORT, 'injected endpoint routing save failure'); END;",
    )
    .unwrap();
    let next = routed_settings();
    let updates = BTreeMap::from([
        (
            DEFAULT_ENDPOINT_ID.into(),
            ApiKeyUpdate::Set("default-new-secret".into()),
        ),
        ("embed".into(), ApiKeyUpdate::Set("embed-new-secret".into())),
        (
            "vision".into(),
            ApiKeyUpdate::Set("vision-new-secret".into()),
        ),
    ]);
    assert!(block_on_without_tokio(services.configure_providers(next.clone(), updates)).is_err());
    assert_eq!(services.provider_settings().unwrap(), previous);
    assert_eq!(db::settings::list(&conn).unwrap(), previous_rows);
    for endpoint in next.endpoints() {
        let target = normalize_provider_base_url(&endpoint.base_url).unwrap();
        assert!(credentials.api_key(target.as_str()).unwrap().is_none());
    }
    conn.execute_batch("DROP TRIGGER test_endpoint_routing_save_failure")
        .unwrap();
}

#[derive(Default)]
struct FailOnceCredentialStore {
    memory: MemoryCredentialStore,
    fail_next_write: AtomicBool,
}

impl CredentialStore for FailOnceCredentialStore {
    fn set_api_key(&self, target: &str, value: &str) -> Result<()> {
        if target == "http://127.0.0.1:21435/v1/"
            && self.fail_next_write.swap(false, Ordering::SeqCst)
        {
            anyhow::bail!("injected credential backend failure");
        }
        self.memory.set_api_key(target, value)
    }

    fn api_key(&self, target: &str) -> Result<Option<String>> {
        self.memory.api_key(target)
    }

    fn delete_api_key(&self, target: &str) -> Result<()> {
        self.memory.delete_api_key(target)
    }
}

#[test]
fn partial_credential_failure_restores_every_endpoint_and_keeps_settings() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(FailOnceCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let settings = routed_settings();
    let updates = BTreeMap::from([
        (
            DEFAULT_ENDPOINT_ID.into(),
            ApiKeyUpdate::Set("default-old-secret".into()),
        ),
        ("embed".into(), ApiKeyUpdate::Set("embed-old-secret".into())),
        (
            "vision".into(),
            ApiKeyUpdate::Set("vision-old-secret".into()),
        ),
    ]);
    block_on_without_tokio(services.configure_providers(settings.clone(), updates)).unwrap();
    let before_rows =
        db::settings::list(&db::open_conn(services.database_path()).unwrap()).unwrap();
    credentials.fail_next_write.store(true, Ordering::SeqCst);
    let mut changed = settings.clone();
    changed.chat_model = "must-not-publish".into();
    let updates = BTreeMap::from([
        (
            DEFAULT_ENDPOINT_ID.into(),
            ApiKeyUpdate::Set("default-new-secret".into()),
        ),
        ("embed".into(), ApiKeyUpdate::Set("embed-new-secret".into())),
        ("vision".into(), ApiKeyUpdate::Delete),
    ]);
    assert!(block_on_without_tokio(services.configure_providers(changed, updates)).is_err());
    assert_eq!(services.provider_settings().unwrap(), settings);
    assert_eq!(
        db::settings::list(&db::open_conn(services.database_path()).unwrap()).unwrap(),
        before_rows
    );
    for (id, expected) in [
        (DEFAULT_ENDPOINT_ID, "default-old-secret"),
        ("embed", "embed-old-secret"),
        ("vision", "vision-old-secret"),
    ] {
        let target =
            normalize_provider_base_url(&settings.endpoint_by_id(id).unwrap().base_url).unwrap();
        assert_eq!(
            credentials.api_key(target.as_str()).unwrap().as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn invalid_persisted_routing_is_preserved_and_reported() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    let dangling = EndpointRoutingSettings {
        vision_endpoint_id: "missing".into(),
        ..EndpointRoutingSettings::default()
    };
    let mut unknown = serde_json::to_value(EndpointRoutingSettings::default()).unwrap();
    unknown["extra"] = serde_json::json!(true);
    for value_json in [
        "{".into(),
        "{}".into(),
        serde_json::to_string(&dangling).unwrap(),
        unknown.to_string(),
    ] {
        let row = db::settings::Setting {
            key: ENDPOINT_ROUTING_SETTINGS_KEY.into(),
            value_json,
            updated_at: 1,
        };
        db::settings::upsert(&conn, &row).unwrap();
        assert!(load_provider_settings(&db_path).is_err());
        assert_eq!(
            db::settings::get(&conn, ENDPOINT_ROUTING_SETTINGS_KEY).unwrap(),
            Some(row)
        );
    }
}

#[test]
fn background_job_logs_enforce_book_scope_and_job_existence() {
    use crate::ai::job_diagnostics::{JobLogEvent, JobLogMetrics, record_for_database};

    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let created = block_on_without_tokio(async {
        services
            .spawn_library_projected(|library| library.create_book("日志范围", "作者"))
            .await
            .context("library mutation worker stopped")?
    })
    .unwrap();
    let conn = db::open_conn(services.database_path()).unwrap();
    let jobs = db::index_jobs::list_for_book(&conn, &created.value.id).unwrap();
    let job_id = jobs[0].id.clone();
    record_for_database(
        services.database_path(),
        &job_id,
        JobLogEvent::RunStarted,
        JobLogMetrics::default(),
    );
    let logs = block_on_without_tokio(
        services.background_job_logs(job_id.clone(), vec![created.value.id.clone()]),
    )
    .unwrap();
    assert_eq!(logs.entries.len(), 1);
    for scope in [Vec::new(), vec!["another-book".into()]] {
        assert!(
            block_on_without_tokio(services.background_job_logs(job_id.clone(), scope)).is_err()
        );
    }
    db::index_jobs::delete(&conn, &job_id).unwrap();
    assert!(
        block_on_without_tokio(services.background_job_logs(job_id, vec![created.value.id]))
            .is_err()
    );
}

#[test]
fn configured_background_job_policy_reaches_service_library_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let mut settings = services.provider_settings().unwrap();
    settings.auto_run_background_jobs = false;
    // This test only counts the derived indexing jobs, so it keeps
    // translation off instead of also picking up the paused whole-book
    // translation task a fresh install configures by default.
    settings.default_language = None;
    block_on_without_tokio(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
        .unwrap();

    let created = block_on_without_tokio(async {
        services
            .spawn_library_projected(|library| library.create_book("暂停服务派生任务", "作者"))
            .await
            .context("library mutation worker stopped")?
    })
    .unwrap();
    let jobs = db::index_jobs::list_for_book(
        &db::open_conn(services.database_path()).unwrap(),
        &created.value.id,
    )
    .unwrap();
    assert_eq!(jobs.len(), 3);
    assert!(jobs.iter().all(|job| {
        job.status == db::index_jobs::IndexJobStatus::Paused
            && job.attempts == 0
            && job.started_at.is_none()
            && job.finished_at.is_none()
    }));
    assert_eq!(services.provider_settings().unwrap(), settings);

    settings.auto_run_background_jobs = true;
    block_on_without_tokio(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
        .unwrap();
    let existing_jobs = db::index_jobs::list_for_book(
        &db::open_conn(services.database_path()).unwrap(),
        &created.value.id,
    )
    .unwrap();
    assert_eq!(existing_jobs.len(), 3);
    assert!(existing_jobs.iter().all(|job| {
        job.status == db::index_jobs::IndexJobStatus::Paused
            && job.attempts == 0
            && job.started_at.is_none()
            && job.finished_at.is_none()
    }));
    assert_eq!(services.provider_settings().unwrap(), settings);
}

#[test]
fn existing_provider_settings_start_with_unconfigured_generation_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    let settings = ProviderSettings {
        chat_model: "existing-chat-choice".into(),
        ..Default::default()
    };
    let provider_row = db::settings::Setting {
        key: PROVIDER_SETTINGS_KEY.into(),
        value_json: serde_json::to_string(&settings).unwrap(),
        updated_at: 1,
    };
    db::settings::upsert(&conn, &provider_row).unwrap();
    assert!(
        db::settings::get(&conn, CHAT_GENERATION_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        db::settings::get(&conn, BACKGROUND_JOB_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        db::settings::get(&conn, PDF_READER_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        db::settings::get(&conn, TRANSLATION_GENERATION_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    drop(conn);

    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    assert_eq!(services.provider_settings().unwrap(), settings);
    // 缺失的翻译生成参数用翻译自己的默认值补齐：温度 0、输出上限 4096，与这次改动
    // 之前写死的取值逐字相同，所以升级不会改变老库发出的翻译请求。
    let generation = services.provider_settings().unwrap().translation_generation;
    assert_eq!(generation.temperature, Some(0.0));
    assert_eq!(
        generation.max_output_tokens,
        TranslationGenerationSettings::default().max_output_tokens
    );
    assert!(
        !services
            .provider_settings()
            .unwrap()
            .auto_run_background_jobs
    );
    // A book-less install defaults to the ordinary page gap and does not
    // write the reading preference until the user saves it.
    assert!(!services.provider_settings().unwrap().pdf_compact_reading);
    let conn = db::open_conn(services.database_path()).unwrap();
    assert_eq!(
        db::settings::get(&conn, PROVIDER_SETTINGS_KEY).unwrap(),
        Some(provider_row)
    );
    assert!(
        db::settings::get(&conn, CHAT_GENERATION_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        db::settings::get(&conn, TRANSLATION_GENERATION_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        db::settings::get(&conn, BACKGROUND_JOB_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        db::settings::get(&conn, PDF_READER_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
}

#[test]
fn translation_language_defaults_to_simplified_chinese_and_round_trips_through_its_own_key() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    assert!(
        db::settings::get(&conn, TRANSLATION_SETTINGS_KEY)
            .unwrap()
            .is_none()
    );
    drop(conn);

    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    // A fresh install translates into Simplified Chinese without the user
    // touching the settings window; the jobs it creates stay paused until
    // automatic background execution is enabled.
    assert_eq!(
        services
            .provider_settings()
            .unwrap()
            .default_language
            .as_deref(),
        Some("zh-Hans")
    );

    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("ja".to_string());
    block_on_without_tokio(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
        .unwrap();
    assert_eq!(
        services
            .provider_settings()
            .unwrap()
            .default_language
            .as_deref(),
        Some("ja")
    );

    let invalid = ProviderSettings {
        default_language: Some("not-a-preset".to_string()),
        ..Default::default()
    };
    assert!(invalid.validate().is_err());

    let conn = db::open_conn(services.database_path()).unwrap();
    let stored = db::settings::get(&conn, TRANSLATION_SETTINGS_KEY)
        .unwrap()
        .expect("translation settings must be persisted under their own key");
    assert_eq!(
        serde_json::from_str::<PersistedTranslationSettings>(&stored.value_json)
            .unwrap()
            .default_language
            .as_deref(),
        Some("ja")
    );
    let provider = db::settings::get(&conn, PROVIDER_SETTINGS_KEY)
        .unwrap()
        .expect("provider settings must stay persisted");
    assert!(
        !provider.value_json.contains("default_language"),
        "the translation language must not leak into the provider JSON contract",
    );
}
