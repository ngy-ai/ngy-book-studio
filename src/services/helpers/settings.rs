//! Snapshotting and restoring the AI settings, and deriving the execution
//! identities from them.

use super::*;

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
