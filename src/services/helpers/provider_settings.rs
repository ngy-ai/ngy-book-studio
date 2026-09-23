//! Loading, validating and saving the provider settings.

use super::*;

pub(crate) fn load_provider_settings(db_path: &Path) -> Result<ProviderSettings> {
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法读取 AI 设置快照")?;
    let mut settings = match db::settings::get(&tx, PROVIDER_SETTINGS_KEY)? {
        Some(row) => {
            let value = serde_json::from_str::<serde_json::Value>(&row.value_json)
                .context("stored AI provider settings are invalid")?;
            let mut settings = serde_json::from_value::<ProviderSettings>(value.clone())
                .context("stored AI provider settings are invalid")?;
            // 迁移：翻译模型是后加的键。老行的翻译任务本来就用问答模型，缺失时按当时保存的
            // 问答模型补齐，翻译执行身份因此逐字不变 —— 升级不会把已翻完的图书重翻一遍。
            if !value
                .get("translation_model")
                .is_some_and(serde_json::Value::is_string)
            {
                settings.translation_model = settings.chat_model.clone();
            }
            settings
        }
        None => ProviderSettings::default(),
    };
    settings.chat_generation = match db::settings::get(&tx, CHAT_GENERATION_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<ChatGenerationSettings>(&row.value_json)
            .context("保存的问答生成参数无效")?,
        None => ChatGenerationSettings::default(),
    };
    // 翻译的生成参数也是后加的行。缺失时用翻译自己的默认值（温度 0、输出上限
    // 4096），与这次改动之前写死的取值逐字相同，所以升级不会改变老库的翻译请求。
    settings.translation_generation =
        match db::settings::get(&tx, TRANSLATION_GENERATION_SETTINGS_KEY)? {
            Some(row) => serde_json::from_str::<TranslationGenerationSettings>(&row.value_json)
                .context("保存的翻译生成参数无效")?,
            None => TranslationGenerationSettings::default(),
        };
    settings.endpoint_routing = match db::settings::get(&tx, ENDPOINT_ROUTING_SETTINGS_KEY)? {
        Some(row) => {
            let value = serde_json::from_str::<serde_json::Value>(&row.value_json)
                .context("保存的 Endpoint 与模型绑定设置无效")?;
            let mut routing = serde_json::from_value::<EndpointRoutingSettings>(value.clone())
                .context("保存的 Endpoint 与模型绑定设置无效")?;
            // 与翻译模型同一套迁移：老绑定行没有翻译 Endpoint，按当时的问答 Endpoint 补齐。
            if !value
                .get("translation_endpoint_id")
                .is_some_and(serde_json::Value::is_string)
            {
                routing.translation_endpoint_id = routing.chat_endpoint_id.clone();
            }
            routing
        }
        None => EndpointRoutingSettings::default(),
    };
    let background_jobs = match db::settings::get(&tx, BACKGROUND_JOB_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<PersistedBackgroundJobSettings>(&row.value_json)
            .context("保存的后台任务设置无效")?,
        None => PersistedBackgroundJobSettings::default(),
    };
    settings.auto_run_background_jobs = background_jobs.auto_run;
    // Clamp instead of failing: a hand-edited or truncated row must not prevent
    // the library from opening.
    settings.background_job_concurrency = background_jobs.concurrency.clamp(
        MIN_BACKGROUND_JOB_CONCURRENCY,
        MAX_BACKGROUND_JOB_CONCURRENCY,
    );
    settings.background_job_interval_ms = background_jobs
        .interval_ms
        .min(MAX_BACKGROUND_JOB_INTERVAL_MS);
    settings.pdf_compact_reading = match db::settings::get(&tx, PDF_READER_SETTINGS_KEY)? {
        Some(row) => {
            serde_json::from_str::<PersistedPdfReaderSettings>(&row.value_json)
                .context("保存的 PDF 阅读设置无效")?
                .compact_reading
        }
        None => default_pdf_compact_reading(),
    };
    settings.appearance = match db::settings::get(&tx, APPEARANCE_SETTINGS_KEY)? {
        Some(row) => {
            serde_json::from_str::<PersistedAppearanceSettings>(&row.value_json)
                .context("保存的外观设置无效")?
                .theme
        }
        None => Appearance::default(),
    };
    let translation_settings = match db::settings::get(&tx, TRANSLATION_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<PersistedTranslationSettings>(&row.value_json)
            .context("保存的翻译设置无效")?,
        None => PersistedTranslationSettings::default(),
    };
    settings.default_language = translation_settings.default_language;
    settings.translation_display_mode = translation_settings.display_mode;
    settings.use_proxy = match db::settings::get(&tx, NETWORK_SETTINGS_KEY)? {
        Some(row) => {
            serde_json::from_str::<PersistedNetworkSettings>(&row.value_json)
                .context("保存的网络设置无效")?
                .use_proxy
        }
        None => default_ai_use_proxy(),
    };
    settings.validate()?;
    tx.commit().context("无法完成 AI 设置快照读取")?;
    Ok(settings)
}

pub(crate) fn save_provider_settings(db_path: &Path, settings: &ProviderSettings) -> Result<()> {
    settings.validate()?;
    let value_json = serde_json::to_string(settings).context("failed to serialize AI settings")?;
    // A structural assertion guards future accidental additions to the
    // persisted type as well as today's explicit absence of an API-key field.
    let object = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&value_json)
        .context("AI settings did not serialize to an object")?;
    ensure!(
        !object.keys().any(|key| {
            let key = key.to_ascii_lowercase();
            key.contains("api_key") || key.contains("password") || key.contains("secret")
        }),
        "refusing to persist a secret-bearing AI setting"
    );
    let updated_at = unix_timestamp()?;
    let setting = db::settings::Setting {
        key: PROVIDER_SETTINGS_KEY.to_string(),
        value_json,
        updated_at,
    };
    let generation = db::settings::Setting {
        key: CHAT_GENERATION_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&settings.chat_generation)
            .context("无法序列化问答生成参数")?,
        updated_at,
    };
    let translation_generation = db::settings::Setting {
        key: TRANSLATION_GENERATION_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&settings.translation_generation)
            .context("无法序列化翻译生成参数")?,
        updated_at,
    };
    let background_jobs = db::settings::Setting {
        key: BACKGROUND_JOB_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedBackgroundJobSettings {
            auto_run: settings.auto_run_background_jobs,
            concurrency: settings.background_job_concurrency,
            interval_ms: settings.background_job_interval_ms,
        })
        .context("无法序列化后台任务设置")?,
        updated_at,
    };
    let pdf_reader = db::settings::Setting {
        key: PDF_READER_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedPdfReaderSettings {
            compact_reading: settings.pdf_compact_reading,
        })
        .context("无法序列化 PDF 阅读设置")?,
        updated_at,
    };
    let appearance = db::settings::Setting {
        key: APPEARANCE_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedAppearanceSettings {
            theme: settings.appearance,
        })
        .context("无法序列化外观设置")?,
        updated_at,
    };
    let translation = db::settings::Setting {
        key: TRANSLATION_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedTranslationSettings {
            default_language: settings.default_language.clone(),
            display_mode: settings.translation_display_mode,
        })
        .context("无法序列化翻译设置")?,
        updated_at,
    };
    let routing = db::settings::Setting {
        key: ENDPOINT_ROUTING_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&settings.endpoint_routing)
            .context("无法序列化 Endpoint 与模型绑定设置")?,
        updated_at,
    };
    let network = db::settings::Setting {
        key: NETWORK_SETTINGS_KEY.to_string(),
        value_json: serde_json::to_string(&PersistedNetworkSettings {
            use_proxy: settings.use_proxy,
        })
        .context("无法序列化网络设置")?,
        updated_at,
    };
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法开始保存 AI 设置")?;
    ensure!(
        db::settings::upsert(&tx, &setting)? == 1,
        "AI provider settings were not stored"
    );
    ensure!(
        db::settings::upsert(&tx, &generation)? == 1,
        "问答生成参数未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &translation_generation)? == 1,
        "翻译生成参数未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &background_jobs)? == 1,
        "后台任务设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &pdf_reader)? == 1,
        "PDF 阅读设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &appearance)? == 1,
        "外观设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &translation)? == 1,
        "翻译设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &routing)? == 1,
        "Endpoint 与模型绑定设置未能保存"
    );
    ensure!(
        db::settings::upsert(&tx, &network)? == 1,
        "网络设置未能保存"
    );
    tx.commit().context("无法提交 AI 设置")?;
    Ok(())
}

pub(crate) fn validate_model_name(kind: &str, model: &str) -> Result<()> {
    ensure!(
        !model.trim().is_empty()
            && model.trim() == model
            && model.chars().count() <= MAX_MODEL_NAME_CHARS
            && !model.chars().any(char::is_control),
        "{kind} model is invalid"
    );
    Ok(())
}

pub(crate) fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_secs())
}
