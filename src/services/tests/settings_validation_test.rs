//! Tests for the validation and rollback of saved settings.

use super::*;

#[test]
fn malformed_or_invalid_saved_generation_is_reported_and_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    let mut out_of_bounds = serde_json::to_value(ChatGenerationSettings::default()).unwrap();
    out_of_bounds["max_output_tokens"] = serde_json::json!(0);
    let mut unknown_field = serde_json::to_value(ChatGenerationSettings::default()).unwrap();
    unknown_field["extra"] = serde_json::json!(true);
    for value_json in [
        "{".to_string(),
        "{}".to_string(),
        out_of_bounds.to_string(),
        unknown_field.to_string(),
    ] {
        let row = db::settings::Setting {
            key: CHAT_GENERATION_SETTINGS_KEY.into(),
            value_json,
            updated_at: 1,
        };
        db::settings::upsert(&conn, &row).unwrap();
        assert!(load_provider_settings(&db_path).is_err());
        assert_eq!(
            db::settings::get(&conn, CHAT_GENERATION_SETTINGS_KEY).unwrap(),
            Some(row)
        );
    }
}

#[test]
fn malformed_or_invalid_saved_translation_generation_is_reported_and_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    // 翻译的温度上限是 2（问答一样），0 个输出 token 同样不接受。
    let mut out_of_bounds = serde_json::to_value(TranslationGenerationSettings::default()).unwrap();
    out_of_bounds["temperature"] = serde_json::json!(2.5);
    let mut zero_output = serde_json::to_value(TranslationGenerationSettings::default()).unwrap();
    zero_output["max_output_tokens"] = serde_json::json!(0);
    for value_json in [
        "{".to_string(),
        "{}".to_string(),
        out_of_bounds.to_string(),
        zero_output.to_string(),
    ] {
        let row = db::settings::Setting {
            key: TRANSLATION_GENERATION_SETTINGS_KEY.into(),
            value_json,
            updated_at: 1,
        };
        db::settings::upsert(&conn, &row).unwrap();
        assert!(load_provider_settings(&db_path).is_err());
        assert_eq!(
            db::settings::get(&conn, TRANSLATION_GENERATION_SETTINGS_KEY).unwrap(),
            Some(row)
        );
    }
}

#[test]
fn malformed_saved_background_job_setting_is_reported_and_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    for value_json in [
        "{".to_string(),
        "{}".to_string(),
        serde_json::json!({ "auto_run": "yes" }).to_string(),
        serde_json::json!({ "auto_run": true, "extra": false }).to_string(),
    ] {
        let row = db::settings::Setting {
            key: BACKGROUND_JOB_SETTINGS_KEY.into(),
            value_json,
            updated_at: 1,
        };
        db::settings::upsert(&conn, &row).unwrap();
        assert!(load_provider_settings(&db_path).is_err());
        assert_eq!(
            db::settings::get(&conn, BACKGROUND_JOB_SETTINGS_KEY).unwrap(),
            Some(row)
        );
    }
}

#[test]
fn background_job_scheduling_setting_is_validated_and_clamped_on_load() {
    for settings in [
        ProviderSettings {
            background_job_concurrency: 0,
            ..Default::default()
        },
        ProviderSettings {
            background_job_concurrency: MAX_BACKGROUND_JOB_CONCURRENCY + 1,
            ..Default::default()
        },
        ProviderSettings {
            background_job_interval_ms: MAX_BACKGROUND_JOB_INTERVAL_MS + 1,
            ..Default::default()
        },
    ] {
        assert!(settings.validate().is_err());
    }

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    let out_of_range = db::settings::Setting {
        key: BACKGROUND_JOB_SETTINGS_KEY.into(),
        value_json: serde_json::json!({
            "auto_run": true,
            "concurrency": 0,
            "interval_ms": u64::MAX,
        })
        .to_string(),
        updated_at: 1,
    };
    db::settings::upsert(&conn, &out_of_range).unwrap();
    // A damaged value must not make the library unopenable.
    let settings = load_provider_settings(&db_path).unwrap();
    assert!(settings.auto_run_background_jobs);
    assert_eq!(
        settings.background_job_concurrency,
        MIN_BACKGROUND_JOB_CONCURRENCY
    );
    assert_eq!(
        settings.background_job_interval_ms,
        MAX_BACKGROUND_JOB_INTERVAL_MS
    );

    // A row written before the scheduling options existed still loads with
    // the documented defaults.
    let legacy = db::settings::Setting {
        key: BACKGROUND_JOB_SETTINGS_KEY.into(),
        value_json: serde_json::json!({ "auto_run": false }).to_string(),
        updated_at: 2,
    };
    db::settings::upsert(&conn, &legacy).unwrap();
    let settings = load_provider_settings(&db_path).unwrap();
    assert_eq!(
        settings.background_job_concurrency,
        DEFAULT_BACKGROUND_JOB_CONCURRENCY
    );
    assert_eq!(
        settings.background_job_interval_ms,
        DEFAULT_BACKGROUND_JOB_INTERVAL_MS
    );
}

/// 翻译执行身份只由翻译模型与它的 Endpoint 派生：换问答模型（或换问答 Endpoint）
/// 不该让已翻完的图书重翻，换翻译模型或翻译 Endpoint 才该。
#[test]
fn translation_identity_tracks_only_the_translation_model_and_its_endpoint() {
    let mut base = ProviderSettings::default();
    base.endpoint_routing.additional_endpoints = vec![endpoint_fixture("translate", 21438)];
    let expected = translation_execution_identity(&base).unwrap();

    let mut unrelated = base.clone();
    unrelated.chat_model = "chat-other".to_string();
    unrelated.chat_generation.max_output_tokens = 128;
    unrelated.translation_display_mode = TranslationDisplayMode::Bilingual;
    unrelated.endpoint_routing.chat_endpoint_id = "translate".into();
    assert_eq!(
        translation_execution_identity(&unrelated).unwrap(),
        expected
    );

    let mut different_model = base.clone();
    different_model.translation_model = "translate-other".to_string();
    assert_ne!(
        translation_execution_identity(&different_model).unwrap(),
        expected
    );

    let mut different_endpoint = base;
    different_endpoint.endpoint_routing.translation_endpoint_id = "translate".into();
    assert_ne!(
        translation_execution_identity(&different_endpoint).unwrap(),
        expected
    );
}

/// 老版本保存的行里没有翻译模型与翻译 Endpoint：加载时按当时的问答配置补齐，
/// 因此升级后翻译执行身份逐字不变，已翻完的图书不会因为升级被重翻。
#[test]
fn rows_written_before_translation_was_split_follow_the_question_model() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    let mut legacy = ProviderSettings::default();
    legacy.chat_model = "legacy-chat-model".to_string();
    let mut provider = serde_json::to_value(&legacy).unwrap();
    provider
        .as_object_mut()
        .unwrap()
        .remove("translation_model");
    let mut routing = serde_json::to_value(&legacy.endpoint_routing).unwrap();
    routing
        .as_object_mut()
        .unwrap()
        .remove("translation_endpoint_id");
    for (key, value_json) in [
        (PROVIDER_SETTINGS_KEY, provider.to_string()),
        (ENDPOINT_ROUTING_SETTINGS_KEY, routing.to_string()),
    ] {
        db::settings::upsert(
            &conn,
            &db::settings::Setting {
                key: key.into(),
                value_json,
                updated_at: 1,
            },
        )
        .unwrap();
    }

    let loaded = load_provider_settings(&db_path).unwrap();
    assert_eq!(loaded.translation_model, "legacy-chat-model");
    assert_eq!(
        loaded.endpoint_routing.translation_endpoint_id,
        DEFAULT_ENDPOINT_ID
    );
    // 旧公式就是「问答 Endpoint + 问答模型」，所以补齐后的身份与旧版相同。
    let mut as_before_the_split = ProviderSettings::default();
    as_before_the_split.chat_model = "legacy-chat-model".to_string();
    as_before_the_split.translation_model = "legacy-chat-model".to_string();
    assert_eq!(
        translation_execution_identity(&loaded).unwrap(),
        translation_execution_identity(&as_before_the_split).unwrap()
    );
}

#[test]
fn generation_save_failure_rolls_back_provider_generation_and_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let previous = services.provider_settings().unwrap();
    block_on_without_tokio(services.configure_provider(previous.clone(), ApiKeyUpdate::Keep))
        .unwrap();
    let conn = db::open_conn(services.database_path()).unwrap();
    let previous_rows = db::settings::list(&conn).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER test_chat_generation_save_failure
             BEFORE INSERT ON settings
             WHEN NEW.key = 'ai.openai_compatible.chat_generation.v1'
             BEGIN
                 SELECT RAISE(ABORT, 'injected chat generation save failure');
             END;",
    )
    .unwrap();
    let mut next = previous.clone();
    next.chat_model = "must-not-persist".into();
    next.chat_generation.max_output_tokens = 128;
    assert!(
        block_on_without_tokio(services.configure_provider(
            next,
            ApiKeyUpdate::Set("must-rollback-generation-test".into())
        ))
        .is_err()
    );
    assert_eq!(services.provider_settings().unwrap(), previous);
    assert_eq!(
        load_provider_settings(services.database_path()).unwrap(),
        previous
    );
    assert_eq!(db::settings::list(&conn).unwrap(), previous_rows);
    assert!(credentials.api_key(&previous.base_url).unwrap().is_none());
    conn.execute_batch("DROP TRIGGER test_chat_generation_save_failure")
        .unwrap();
}

#[test]
fn embedding_identity_uses_only_canonical_endpoint_and_embedding_model() {
    let base = ProviderSettings::default();
    let expected = embedding_execution_identity(&base).unwrap();
    let mut unrelated = base.clone();
    unrelated.base_url = format!("{}/", unrelated.base_url.trim_end_matches('/'));
    unrelated.chat_model = "chat-other".to_string();
    unrelated.chat_generation.max_output_tokens = 128;
    unrelated.chat_generation.temperature = None;
    unrelated.vision_model = "vision-other".to_string();
    unrelated.request_timeout_secs = 1;
    unrelated.remote_content_confirmed = true;
    unrelated.allow_insecure_remote_http = true;
    unrelated.confirmed_remote_endpoint = "ignored-for-identity".to_string();
    assert_eq!(embedding_execution_identity(&unrelated).unwrap(), expected);

    let mut different_model = base.clone();
    different_model.embedding_model = "embed-other".to_string();
    assert_ne!(
        embedding_execution_identity(&different_model).unwrap(),
        expected
    );
    let mut different_endpoint = base;
    different_endpoint.base_url = "http://127.0.0.1:11435/v1".to_string();
    assert_ne!(
        embedding_execution_identity(&different_endpoint).unwrap(),
        expected
    );
}

#[test]
fn embedding_identity_changes_when_dimensions_change() {
    let base = ProviderSettings::default();
    let expected = embedding_execution_identity(&base).unwrap();
    let mut different_dimensions = base.clone();
    different_dimensions.embedding_dimensions = 768;
    assert_ne!(
        embedding_execution_identity(&different_dimensions).unwrap(),
        expected
    );
}

#[cfg(target_os = "windows")]
#[test]
fn chat_timeout_and_key_changes_do_not_requeue_embedding_work() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let book = block_on_without_tokio(async {
        services
            .spawn_library(|library| library.create_book("Embedding identity", "Author"))
            .await
            .context("library test worker stopped")?
    })
    .unwrap();
    let conn = db::open_conn(services.database_path()).unwrap();
    let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
        .unwrap()
        .unwrap();
    drop(conn);
    services
        .runtime()
        .block_on(stop_indexing_jobs_before_visual_replacement(
            &services.indexing,
            &book.id,
            &source.id,
        ))
        .unwrap();
    let before = db::index_jobs::list_for_source_kind(
        &db::open_conn(services.database_path()).unwrap(),
        &source.id,
        "embedding",
    )
    .unwrap();
    assert!(!before.is_empty());

    let mut next = services.provider_settings().unwrap();
    next.chat_model = "chat-only-change".to_string();
    next.chat_generation.max_output_tokens = 512;
    next.chat_generation.top_p = Some(0.75);
    next.request_timeout_secs = (next.request_timeout_secs + 1).min(600);
    block_on_without_tokio(
        services.configure_provider(next, ApiKeyUpdate::Set("key-only-change".to_string())),
    )
    .unwrap();

    let after = db::index_jobs::list_for_source_kind(
        &db::open_conn(services.database_path()).unwrap(),
        &source.id,
        "embedding",
    )
    .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        credentials
            .api_key(DEFAULT_OLLAMA_OPENAI_BASE_URL)
            .unwrap()
            .as_deref(),
        Some("key-only-change")
    );
}

#[cfg(target_os = "windows")]
#[test]
fn provider_configuration_rolls_back_when_index_reconfigure_fails() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
    let book = block_on_without_tokio(async {
        services
            .spawn_library(|library| library.create_book("Provider rollback", "Author"))
            .await
            .context("library test worker stopped")?
    })
    .unwrap();
    let conn = db::open_conn(services.database_path()).unwrap();
    let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
        .unwrap()
        .unwrap();
    services
        .runtime()
        .block_on(stop_indexing_jobs_before_visual_replacement(
            &services.indexing,
            &book.id,
            &source.id,
        ))
        .unwrap();
    let vision_id = format!("vision:{}", source.id);
    let before_job = db::index_jobs::get(&conn, &vision_id).unwrap().unwrap();
    let previous = services.provider_settings().unwrap();
    let previous_rows = db::settings::list(&conn).unwrap();
    let previous_snapshot = services.ai_request_snapshot().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER test_provider_reconfigure_failure
             BEFORE INSERT ON index_jobs
             WHEN NEW.kind = 'embedding'
              AND instr(NEW.cursor_json, '\"model\":\"embed-rollback-test\"') > 0
             BEGIN
                 SELECT RAISE(ABORT, 'injected provider reconfigure failure');
             END;",
    )
    .unwrap();

    let mut next = routed_settings();
    next.chat_model = "chat-rollback-test".to_string();
    next.chat_generation.max_output_tokens = 128;
    next.chat_generation.temperature = None;
    next.embedding_model = "embed-rollback-test".to_string();
    next.vision_model = "vision-rollback-test".to_string();
    let updates = BTreeMap::from([
        (
            DEFAULT_ENDPOINT_ID.into(),
            ApiKeyUpdate::Set("default-rollback-secret".into()),
        ),
        (
            "embed".into(),
            ApiKeyUpdate::Set("embed-rollback-secret".into()),
        ),
        (
            "vision".into(),
            ApiKeyUpdate::Set("vision-rollback-secret".into()),
        ),
    ]);
    let result = block_on_without_tokio(services.configure_providers(next.clone(), updates));
    assert!(result.is_err());
    assert_eq!(services.provider_settings().unwrap(), previous);
    let persisted = load_provider_settings(services.database_path()).unwrap();
    assert_eq!(persisted, previous);
    assert_eq!(db::settings::list(&conn).unwrap(), previous_rows);
    let restored_snapshot = services.ai_request_snapshot().unwrap();
    assert!(Arc::ptr_eq(
        &restored_snapshot.provider,
        &previous_snapshot.provider
    ));
    assert!(Arc::ptr_eq(
        &restored_snapshot.search,
        &previous_snapshot.search
    ));
    for endpoint in next.endpoints() {
        let target = normalize_provider_base_url(&endpoint.base_url).unwrap();
        assert!(credentials.api_key(target.as_str()).unwrap().is_none());
    }
    assert_eq!(
        db::index_jobs::get(&conn, &vision_id).unwrap().unwrap(),
        before_job
    );

    conn.execute_batch("DROP TRIGGER test_provider_reconfigure_failure")
        .unwrap();
}
