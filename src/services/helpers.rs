//! Free helpers: page loading, job projections and the AI wiring that is
//! not part of the service handle itself.

use super::*;

#[derive(Debug)]
pub(crate) struct PublishedVisualPageRow {
    pub(crate) object_key: String,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) hash: String,
    pub(crate) content_unit_id: Option<String>,
    pub(crate) locator: crate::document::DocumentLocator,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Expected renderer identity and page semantics for one visual page set.
pub(crate) struct PublishedPageExpectation {
    pub(crate) renderer: &'static str,
    pub(crate) fidelity: crate::preview::RenderFidelity,
    /// Source formats whose originals this pipeline may render.
    pub(crate) formats: &'static [&'static str],
    /// Whether the renderer may publish pages without a content unit
    /// (Office Word/Excel repagination previews).
    pub(crate) allow_preview_only: bool,
}

pub(crate) fn published_page_expectation(
    kind: VisualPageSourceKind,
) -> Result<PublishedPageExpectation> {
    match kind {
        #[cfg(target_os = "windows")]
        VisualPageSourceKind::OfficeEnhanced => Ok(PublishedPageExpectation {
            renderer: OFFICE_ENHANCED_RENDERER_NAME,
            fidelity: crate::preview::RenderFidelity::OfficeEnhanced,
            formats: &["doc", "docx", "pptx", "xlsx"],
            allow_preview_only: true,
        }),
        #[cfg(not(target_os = "windows"))]
        VisualPageSourceKind::OfficeEnhanced => {
            anyhow::bail!("Office 增强预览仅支持 Windows")
        }
        VisualPageSourceKind::Djvu => Ok(PublishedPageExpectation {
            renderer: DJVU_RENDERER_NAME,
            fidelity: crate::preview::RenderFidelity::Structural,
            formats: &["djvu"],
            allow_preview_only: false,
        }),
    }
}

/// The persisted `visual_pages.fidelity` string for a render fidelity.
pub(crate) fn render_fidelity_key(fidelity: crate::preview::RenderFidelity) -> &'static str {
    match fidelity {
        crate::preview::RenderFidelity::Normalized => "normalized",
        crate::preview::RenderFidelity::Structural => "structural",
        crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
    }
}

pub(crate) fn visual_page_kind_label(kind: VisualPageSourceKind) -> &'static str {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => "Office 增强",
        VisualPageSourceKind::Djvu => "DjVu",
    }
}

pub(crate) fn load_published_visual_page_rows(
    db_path: &Path,
    book_id: &str,
    kind: VisualPageSourceKind,
) -> Result<Vec<PublishedVisualPageRow>> {
    let expectation = published_page_expectation(kind)?;
    let label = visual_page_kind_label(kind);
    let conn = db::open_conn(db_path)?;
    let book = db::books::get(&conn, book_id)?.context("图书不存在")?;
    let source = db::book_sources::get_revision(&conn, book_id, book.revision)?
        .context("当前图书来源不存在")?;
    if kind == VisualPageSourceKind::OfficeEnhanced {
        ensure!(
            db::office_enhancements::is_enabled(&conn, book_id)?,
            "当前图书尚未启用 Office 增强预览"
        );
    }
    ensure!(
        source.source_kind == "original" && expectation.formats.contains(&source.format.as_str()),
        "当前图书来源不再是可渲染的 {label} 原件"
    );

    let job_id = format!("visual-render:{}", source.id);
    let job =
        db::index_jobs::get(&conn, &job_id)?.with_context(|| format!("{label} 视觉任务不存在"))?;
    ensure!(
        job.book_id == book.id
            && job.source_id.as_deref() == Some(source.id.as_str())
            && job.kind == "visual_render",
        "{label} 视觉任务不属于当前图书来源"
    );
    ensure!(
        job.status == db::index_jobs::IndexJobStatus::Succeeded,
        "{label} 视觉任务尚未成功完成"
    );
    let (spec, completed_pages) = crate::preview::decode_persisted_visual_job(&job.cursor_json)
        .with_context(|| format!("{label} 视觉任务游标无效"))?;
    ensure!(
        spec.id == job_id
            && spec.book_id == book.id
            && spec.source_id == source.id
            && spec.document_revision.get() == book.revision
            && spec.renderer == expectation.renderer
            && spec.fidelity == expectation.fidelity,
        "{label} 视觉任务不是当前图书版本的产物"
    );
    let units = db::content_units::list_for_source(&conn, &source.id)?
        .into_iter()
        .map(|unit| (unit.id, unit.revision))
        .collect::<BTreeMap<_, _>>();
    ensure!(!units.is_empty(), "当前 {label} 来源没有内容单元");
    ensure!(
        spec.unit_ids.is_empty()
            || (spec.unit_ids.len() == units.len()
                && spec
                    .unit_ids
                    .iter()
                    .all(|unit_id| units.contains_key(unit_id))),
        "{label} 视觉任务只包含部分内容单元"
    );
    let pages = db::visual_pages::list_for_source(&conn, &source.id)?;
    ensure!(
        !pages.is_empty() && pages.len() <= MAX_LOADED_VISUAL_PAGES,
        "{label} 页面数量无效或超过支持上限"
    );
    ensure!(
        completed_pages == pages.len(),
        "{label} 视觉任务完成进度与已发布页面不一致"
    );

    let profile_id = spec.profile.stable_id();
    let mut total_bytes = 0_u64;
    let mut rows = Vec::with_capacity(pages.len());
    for (expected_index, page) in pages.into_iter().enumerate() {
        ensure!(page.page_index == expected_index, "{label} 页面序号不连续");
        ensure!(
            page.book_id == book.id
                && page.source_id == source.id
                && page.document_revision == book.revision
                && page.renderer == spec.renderer
                && page.renderer_version == spec.renderer_version
                && page.profile_id == profile_id
                && page.render_scale == spec.profile.scale()
                && page.fidelity == render_fidelity_key(expectation.fidelity),
            "{label} 页面元数据与当前视觉任务不一致"
        );
        ensure!(
            page.width > 0 && page.height > 0 && page.render_scale.is_finite(),
            "{label} 页面尺寸或缩放无效"
        );
        let locator = serde_json::from_str::<crate::document::DocumentLocator>(&page.locator_json)
            .context("视觉页面定位信息无效")?;
        locator
            .validate()
            .context("视觉页面定位信息不符合统一模型约束")?;
        ensure!(locator.book_id == book.id, "视觉页面定位不属于当前图书");
        ensure!(
            locator.block_id.is_none() && locator.text_range.is_none() && locator.region.is_none(),
            "视觉页面定位不能携带块、选区或区域坐标"
        );
        match locator.source.as_ref() {
            Some(crate::document::SourceLocator::DjvuPage { page: source_page }) => {
                let expected_page = u32::try_from(expected_index)
                    .ok()
                    .and_then(|index| index.checked_add(1));
                ensure!(
                    !expectation.allow_preview_only
                        && page.content_unit_id.as_deref() == Some(locator.unit_id.as_str())
                        && expected_page == Some(*source_page),
                    "DjVu 页面与内容单元或源页码不一致"
                );
                let current_unit_revision = units.get(&locator.unit_id).with_context(|| {
                    format!("DjVu 页面引用了不存在的内容单元：{}", locator.unit_id)
                })?;
                ensure!(
                    page.unit_revision == *current_unit_revision,
                    "DjVu 页面内容单元版本已过期"
                );
            }
            Some(crate::document::SourceLocator::Slide { .. })
                if expectation.allow_preview_only =>
            {
                let content_unit_id = page
                    .content_unit_id
                    .as_deref()
                    .context("PowerPoint 增强页面缺少内容单元 ID")?;
                let current_unit_revision = units.get(content_unit_id).with_context(|| {
                    format!("Office 增强页面引用了不存在的内容单元：{content_unit_id}")
                })?;
                ensure!(
                    locator.unit_id == content_unit_id
                        && page.unit_revision == *current_unit_revision,
                    "PowerPoint 增强页面内容单元映射或版本已过期"
                );
            }
            Some(crate::document::SourceLocator::OfficeRenderedPage { page: source_page })
                if expectation.allow_preview_only =>
            {
                let expected_page = u32::try_from(expected_index)
                    .ok()
                    .and_then(|index| index.checked_add(1));
                ensure!(
                    page.content_unit_id.is_none()
                        && matches!(source.format.as_str(), "doc" | "docx" | "xlsx")
                        && page.unit_revision == crate::document::Revision::INITIAL.get()
                        && locator.unit_id == office_preview_unit_id(&book.id, &source.id)
                        && expected_page == Some(*source_page),
                    "Word/Excel 增强页面错误关联了内容单元或页面定位无效"
                );
            }
            _ => anyhow::bail!("{label} 页面缺少可验证的页面定位"),
        }
        let blob = db::blobs::get(&conn, &page.object_key)?.context("视觉页面对象元数据不存在")?;
        visual_page_image_format(&blob.media_type)?;
        BlobKey::parse(&blob.object_key).context("视觉页面对象键无效")?;
        total_bytes = total_bytes
            .checked_add(blob.byte_len)
            .context("视觉页面总大小溢出")?;
        ensure!(
            total_bytes <= MAX_LOADED_VISUAL_PAGE_BYTES,
            "视觉页面总大小超过支持上限"
        );
        rows.push(PublishedVisualPageRow {
            object_key: blob.object_key,
            media_type: blob.media_type,
            byte_len: blob.byte_len,
            hash: blob.hash,
            content_unit_id: page.content_unit_id,
            locator,
            width: page.width,
            height: page.height,
        });
    }
    Ok(rows)
}

pub(crate) fn visual_page_image_format(media_type: &str) -> Result<image::ImageFormat> {
    match media_type {
        "image/png" => Ok(image::ImageFormat::Png),
        "image/jpeg" => Ok(image::ImageFormat::Jpeg),
        _ => anyhow::bail!("视觉页面使用了不支持的图片媒体类型：{media_type}"),
    }
}

pub(crate) fn visual_page_file_name(page_index: usize, media_type: &str) -> Result<String> {
    let extension = match visual_page_image_format(media_type)? {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        _ => unreachable!("visual page media types are exhaustively checked"),
    };
    Ok(format!("Page{:05}.{extension}", page_index + 1))
}

pub(crate) fn build_ai_services(
    db_path: &Path,
    settings: ProviderSettings,
    api_keys: &BTreeMap<String, Option<String>>,
) -> Result<AiServices> {
    settings.validate()?;
    let build_role = |role| -> Result<Arc<dyn OpenAiCompatibleProvider>> {
        let endpoint = settings.endpoint_for(role)?;
        let api_key = api_keys.get(&endpoint.id).cloned().flatten();
        Ok(Arc::new(OpenAiHttpProvider::new_with_proxy(
            endpoint.provider_config(api_key),
            settings.use_proxy,
        )?))
    };
    let provider = build_role(ModelRole::Chat)?;
    let embedding_provider = build_role(ModelRole::Embedding)?;
    let vision_provider = build_role(ModelRole::Vision)?;
    let search = Arc::new(SearchService::new_with_execution_identity(
        db_path,
        Arc::clone(&embedding_provider),
        &settings.embedding_model,
        settings.embedding_dimensions,
        embedding_execution_identity(&settings)?,
    )?);
    Ok(AiServices {
        settings,
        provider,
        embedding_provider,
        vision_provider,
        search,
    })
}

pub(crate) fn load_endpoint_api_keys(
    settings: &ProviderSettings,
    credentials: &dyn CredentialStore,
) -> Result<BTreeMap<String, Option<String>>> {
    settings
        .endpoints()
        .into_iter()
        .map(|endpoint| {
            let target = normalize_provider_base_url(&endpoint.base_url)?;
            Ok((endpoint.id, credentials.api_key(target.as_str())?))
        })
        .collect()
}

/// Attempt every entry even when one credential operation fails, so rollback
/// does not abandon independent keys after a single failure.
pub(crate) fn restore_endpoint_keys(
    credentials: &dyn CredentialStore,
    values: &BTreeMap<String, Option<String>>,
) -> Result<()> {
    let mut failure = None;
    for (target, value) in values {
        let result = match value {
            Some(value) => credentials.set_api_key(target, value),
            None => credentials.delete_api_key(target),
        };
        if let Err(error) = result {
            failure = Some(match failure {
                Some(previous) => {
                    error.context(format!("another credential operation failed: {previous:#}"))
                }
                None => error,
            });
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) const AI_SETTINGS_KEYS: [&str; 8] = [
    PROVIDER_SETTINGS_KEY,
    CHAT_GENERATION_SETTINGS_KEY,
    ENDPOINT_ROUTING_SETTINGS_KEY,
    BACKGROUND_JOB_SETTINGS_KEY,
    PDF_READER_SETTINGS_KEY,
    APPEARANCE_SETTINGS_KEY,
    TRANSLATION_SETTINGS_KEY,
    NETWORK_SETTINGS_KEY,
];

pub(crate) fn snapshot_ai_settings(db_path: &Path) -> Result<Vec<Option<db::settings::Setting>>> {
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法读取 AI 设置备份")?;
    let rows = AI_SETTINGS_KEYS
        .iter()
        .map(|key| db::settings::get(&tx, key))
        .collect::<Result<_>>()?;
    tx.commit().context("无法完成 AI 设置备份")?;
    Ok(rows)
}

pub(crate) fn restore_ai_settings(
    db_path: &Path,
    rows: &[Option<db::settings::Setting>],
) -> Result<()> {
    ensure!(rows.len() == AI_SETTINGS_KEYS.len(), "AI 设置备份不完整");
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法开始回滚 AI 设置")?;
    for (key, row) in AI_SETTINGS_KEYS.iter().zip(rows) {
        match row {
            Some(row) => {
                db::settings::upsert(&tx, row)?;
            }
            None => {
                db::settings::delete(&tx, key)?;
            }
        }
    }
    tx.commit().context("无法提交 AI 设置回滚")?;
    Ok(())
}

pub(crate) fn indexing_model_config(settings: &ProviderSettings) -> Result<IndexingModelConfig> {
    IndexingModelConfig::new(
        &settings.embedding_model,
        settings.embedding_dimensions,
        embedding_execution_identity(settings)?,
        &settings.vision_model,
        vision_execution_identity(settings)?,
    )
}

/// Shape check for one manual edit request, applied before any database work so
/// an oversized or empty payload never reaches a translation row.
pub(crate) fn validate_manual_translation_segments(
    segments: &[crate::ai::translation::TranslationSegment],
) -> Result<()> {
    ensure!(!segments.is_empty(), "手工译文不能为空");
    ensure!(
        segments.len() <= MAX_MANUAL_TRANSLATION_SEGMENTS,
        "手工译文的片段数量超出上限"
    );
    for segment in segments {
        ensure!(!segment.source.trim().is_empty(), "手工译文缺少原文片段");
        ensure!(
            !segment.translated.trim().is_empty(),
            "译文不能为空；如需还原请使用「恢复机器译文」"
        );
        ensure!(
            segment.source.len() <= MAX_MANUAL_TRANSLATION_SEGMENT_BYTES,
            "原文片段过长"
        );
        ensure!(
            segment.translated.len() <= MAX_MANUAL_TRANSLATION_SEGMENT_BYTES,
            "译文片段过长"
        );
        ensure!(
            !segment.source.contains('\0') && !segment.translated.contains('\0'),
            "译文不能包含空字符"
        );
    }
    Ok(())
}

/// Pairs the request's translated text with the **stored** segment sources: the
/// reading window may replace the translation, never the segment shape or the
/// original text a translation is attached to.
pub(crate) fn manual_translation_segments(
    machine: &crate::ai::translation::StoredTranslation,
    submitted: &[crate::ai::translation::TranslationSegment],
) -> Result<Vec<crate::ai::translation::TranslationSegment>> {
    ensure!(
        machine.segments.len() == submitted.len(),
        "该文本块的原文结构已改变，无法保存手工译文"
    );
    machine
        .segments
        .iter()
        .zip(submitted)
        .enumerate()
        .map(|(index, (stored, submitted))| {
            ensure!(
                stored.source.trim() == submitted.source.trim(),
                "第 {} 段译文的原文已改变，无法保存手工译文",
                index + 1
            );
            Ok(crate::ai::translation::TranslationSegment {
                source: stored.source.clone(),
                translated: submitted.translated.trim().to_string(),
            })
        })
        .collect()
}

/// Non-secret identity for deciding whether persisted whole-book translations
/// are stale. A changed chat endpoint or model restarts every translation job
/// from the first block; the target language is part of each job's identity.
pub(crate) fn translation_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint = normalize_provider_base_url(&settings.endpoint_for(ModelRole::Chat)?.base_url)?;
    Ok(format!(
        "{}:{}",
        crate::ai::translation::EXECUTION_IDENTITY_PROTOCOL,
        blake3::hash(format!("{}\0{}", endpoint.as_str(), settings.chat_model).as_bytes()).to_hex()
    ))
}

pub(crate) fn vision_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint =
        normalize_provider_base_url(&settings.endpoint_for(ModelRole::Vision)?.base_url)?;
    Ok(format!(
        "vision-v1:{}",
        blake3::hash(format!("{}\0{}", endpoint.as_str(), settings.vision_model).as_bytes())
            .to_hex()
    ))
}

/// Non-secret identity for deciding whether persisted vector work is stale.
/// Deliberately excludes chat/vision choices, request policy and credentials.
pub(crate) fn embedding_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint =
        normalize_provider_base_url(&settings.endpoint_for(ModelRole::Embedding)?.base_url)?;
    Ok(format!(
        "embedding-v1:{}",
        blake3::hash(
            format!(
                "{}\0{}\0{}",
                endpoint.as_str(),
                settings.embedding_model,
                settings.embedding_dimensions
            )
            .as_bytes()
        )
        .to_hex()
    ))
}

pub(crate) fn load_provider_settings(db_path: &Path) -> Result<ProviderSettings> {
    let mut conn = db::open_conn(db_path)?;
    let tx = conn.transaction().context("无法读取 AI 设置快照")?;
    let mut settings = match db::settings::get(&tx, PROVIDER_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<ProviderSettings>(&row.value_json)
            .context("stored AI provider settings are invalid")?,
        None => ProviderSettings::default(),
    };
    settings.chat_generation = match db::settings::get(&tx, CHAT_GENERATION_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<ChatGenerationSettings>(&row.value_json)
            .context("保存的对话模型参数无效")?,
        None => ChatGenerationSettings::default(),
    };
    settings.endpoint_routing = match db::settings::get(&tx, ENDPOINT_ROUTING_SETTINGS_KEY)? {
        Some(row) => serde_json::from_str::<EndpointRoutingSettings>(&row.value_json)
            .context("保存的 Endpoint 与模型绑定设置无效")?,
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
            .context("无法序列化对话模型参数")?,
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
        "对话模型参数未能保存"
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

#[cfg(target_os = "windows")]
pub(crate) async fn stop_visual_job_before_replacement(
    coordinator: &VisualJobCoordinator,
    job_id: &str,
) -> Result<()> {
    let Some(record) = coordinator.status(job_id).await? else {
        return Ok(());
    };
    if matches!(
        record.state,
        VisualJobState::Queued | VisualJobState::Running | VisualJobState::Paused
    ) {
        let _ = coordinator.cancel(job_id).await?;
    } else {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(125);
    loop {
        let Some(record) = coordinator.status(job_id).await? else {
            return Ok(());
        };
        if matches!(
            record.state,
            VisualJobState::Succeeded | VisualJobState::Failed | VisualJobState::Cancelled
        ) {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待现有视觉任务停止超时"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(target_os = "windows")]
pub(crate) async fn stop_indexing_jobs_before_visual_replacement(
    indexing: &IndexingCoordinator,
    book_id: &str,
    source_id: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let active = indexing
            .jobs_for_book(book_id)
            .await?
            .into_iter()
            .filter(|job| {
                job.source_id.as_deref() == Some(source_id)
                    && matches!(job.kind.as_str(), "embedding" | "vision")
                    && matches!(
                        job.status,
                        IndexingJobStatus::Queued
                            | IndexingJobStatus::Running
                            | IndexingJobStatus::Paused
                    )
            })
            .collect::<Vec<_>>();
        if active.is_empty() {
            return Ok(());
        }
        for job in active {
            if job.status == IndexingJobStatus::Paused {
                // Paused jobs are not scanned by the worker. Put them back in
                // the queue before setting cancellation so they can publish a
                // durable terminal state.
                let _ = indexing.resume(&job.id).await?;
            }
            let _ = indexing.cancel(&job.id).await?;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待旧视觉理解或向量任务停止超时"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[derive(Clone)]
pub(crate) struct LibraryVisualDocumentSource {
    pub(crate) library: Arc<Mutex<LibraryStore>>,
}

impl VisualDocumentSource for LibraryVisualDocumentSource {
    fn load_document(
        &self,
        book_id: String,
        revision: crate::document::Revision,
    ) -> PreviewFuture<'_, crate::document::BookDocument> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let document = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?
                    .document(&book_id)?;
                ensure!(
                    document.revision == revision,
                    "visual request revision is no longer current"
                );
                Ok(document)
            })
            .await
            .context("visual document loader stopped")?
        })
    }

    fn load_asset(
        &self,
        book_id: String,
        asset_id: String,
    ) -> PreviewFuture<'_, VisualAssetPayload> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let (media_type, expected_len) = library.asset_metadata(&book_id, &asset_id)?;
                ensure!(
                    media_type.starts_with("image/"),
                    "视觉任务只能读取图书所属图片"
                );
                let bytes = library.asset_bytes(&book_id, &asset_id)?;
                ensure!(
                    bytes.len() as u64 == expected_len,
                    "视觉图片对象长度与数据库元数据不一致"
                );
                Ok(VisualAssetPayload { media_type, bytes })
            })
            .await
            .context("visual asset loader stopped")?
        })
    }

    fn load_source(
        &self,
        book_id: String,
        source_id: String,
        revision: crate::document::Revision,
    ) -> PreviewFuture<'_, VisualSourcePayload> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let (format, source_kind, media_type, bytes) =
                    library.visual_source_bytes(&book_id, &source_id, revision)?;
                Ok(VisualSourcePayload {
                    format,
                    source_kind,
                    media_type,
                    bytes,
                })
            })
            .await
            .context("visual source loader stopped")?
        })
    }
}

#[derive(Clone)]
pub(crate) struct LocalVisualPageSink {
    pub(crate) db_path: PathBuf,
    pub(crate) blobs: Arc<LocalBlobStore>,
    pub(crate) blob_publication: BlobPublicationLock,
    pub(crate) indexing: Arc<IndexingCoordinator>,
}

impl VisualPageSink for LocalVisualPageSink {
    fn reset_staging(&self, spec: crate::preview::VisualJobSpec) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let reset_db_path = db_path.clone();
            let unreferenced = tokio::task::spawn_blocking(move || {
                db::transactions::clear_visual_page_staging(
                    &mut db::open_conn(&reset_db_path)?,
                    &spec,
                )
            })
            .await
            .context("visual page staging reset worker stopped")??;
            drop(publication_guard);
            reclaim_unreferenced_blobs(
                &db_path,
                &blobs,
                &blob_publication,
                unreferenced,
                "视觉页面断点对象",
            )
            .await;
            Ok(())
        })
    }

    fn checkpoint_page(
        &self,
        spec: crate::preview::VisualJobSpec,
        page: RenderedVisualPage,
        completed_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let now = unix_timestamp()?;
            let key = blobs.put(&page.bytes).await?;
            ensure!(
                key == crate::storage::BlobKey::from_bytes(&page.bytes),
                "object store returned a non-content-addressed key"
            );
            let blob_row = db::blobs::BlobRecord {
                object_key: key.to_string(),
                media_type: page.media_type.clone(),
                byte_len: page.bytes.len() as u64,
                hash: blake3::hash(&page.bytes).to_hex().to_string(),
                created_at: now,
            };
            let page_row = visual_page_row(&spec, page, key.to_string(), now)?;
            let commit_db_path = db_path.clone();
            tokio::task::spawn_blocking(move || {
                db::transactions::checkpoint_visual_page(
                    &mut db::open_conn(&commit_db_path)?,
                    &spec,
                    &blob_row,
                    &page_row,
                    completed_pages,
                    now,
                )
            })
            .await
            .context("visual page checkpoint worker stopped")??;
            drop(publication_guard);
            Ok(())
        })
    }

    fn commit_pages(
        &self,
        spec: crate::preview::VisualJobSpec,
        total_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        let indexing = Arc::clone(&self.indexing);
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let source_id = spec.source_id.clone();
            let commit_db_path = db_path.clone();
            let now = unix_timestamp()?;
            let unreferenced = tokio::task::spawn_blocking(move || {
                db::transactions::publish_staged_visual_pages(
                    &mut db::open_conn(&commit_db_path)?,
                    &spec,
                    total_pages,
                    now,
                )
            })
            .await
            .context("visual page publication worker stopped")??;
            drop(publication_guard);
            reclaim_unreferenced_blobs(
                &db_path,
                &blobs,
                &blob_publication,
                unreferenced,
                "已替换视觉页面对象",
            )
            .await;
            if let Err(error) = indexing.visual_pages_ready(&source_id).await {
                // Final pages and the Succeeded job state were already
                // committed atomically. Vision jobs are durable and will be
                // discovered by the background scanner, so a wake-up failure
                // must not rewrite the completed render as Failed.
                tracing::warn!(source_id, %error, "视觉页面已发布，但视觉索引即时唤醒失败");
            }
            Ok(())
        })
    }
}

pub(crate) fn visual_page_row(
    spec: &crate::preview::VisualJobSpec,
    page: RenderedVisualPage,
    object_key: String,
    created_at: u64,
) -> Result<db::visual_pages::VisualPage> {
    let fidelity = match page.metadata.fidelity {
        crate::preview::RenderFidelity::Normalized => "normalized",
        crate::preview::RenderFidelity::Structural => "structural",
        crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
    }
    .to_string();
    Ok(db::visual_pages::VisualPage {
        id: page.id,
        book_id: spec.book_id.clone(),
        source_id: spec.source_id.clone(),
        content_unit_id: page.content_unit_id,
        page_index: page.page_index,
        object_key,
        width: page.width,
        height: page.height,
        render_scale: spec.profile.scale(),
        renderer: page.metadata.renderer,
        renderer_version: page.metadata.renderer_version,
        document_revision: page.metadata.document_revision.get(),
        unit_revision: page.metadata.unit_revision.get(),
        profile_id: page.metadata.profile_id,
        fidelity,
        locator_json: serde_json::to_string(&page.locator)
            .context("failed to serialize visual page locator")?,
        created_at,
    })
}
