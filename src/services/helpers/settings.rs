//! Snapshotting and restoring the AI settings, and deriving the execution
//! identities from them.

use super::*;

/// 一次 AI 设置保存会写到的所有行。回滚按这份清单逐键恢复（缺过的行重新删掉），
/// 所以**每个新设置键都必须加进来**，否则失败保存会留下一个半新半旧的行。
pub(crate) const AI_SETTINGS_KEYS: [&str; 11] = [
    PROVIDER_SETTINGS_KEY,
    CHAT_GENERATION_SETTINGS_KEY,
    TRANSLATION_GENERATION_SETTINGS_KEY,
    ENDPOINT_ROUTING_SETTINGS_KEY,
    BACKGROUND_JOB_SETTINGS_KEY,
    PDF_READER_SETTINGS_KEY,
    APPEARANCE_SETTINGS_KEY,
    TRANSLATION_SETTINGS_KEY,
    NETWORK_SETTINGS_KEY,
    SPEECH_SETTINGS_KEY,
    AI_QUESTION_SETTINGS_KEY,
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
/// reading window may replace the translation, never the original text a
/// translation is attached to.
///
/// The request usually carries the stored split one to one. It can carry fewer
/// segments than the row when the chapter's inline structure changed after the
/// row was written: a re-imported source without the inline wrapper the row was
/// split by gives the page fewer leaves for the same text. A request that tiles
/// whole stored segments, in order and without leaving one unused, is accepted
/// as it is; the stored sources still come from the machine row, so only the
/// translated text can ever change. A stored segment the request would have to
/// cut apart, or one that nothing covers, is a changed original text and is
/// refused.
pub(crate) fn manual_translation_segments(
    machine: &crate::ai::translation::StoredTranslation,
    submitted: &[crate::ai::translation::TranslationSegment],
) -> Result<Vec<crate::ai::translation::TranslationSegment>> {
    ensure!(
        submitted.len() <= machine.segments.len(),
        "该文本块的原文结构已改变，无法保存手工译文"
    );
    let mut segments = Vec::with_capacity(submitted.len());
    let mut next = 0usize;
    for (position, request) in submitted.iter().enumerate() {
        let mut source = String::new();
        while source.trim() != request.source.trim() {
            let Some(stored) = machine.segments.get(next) else {
                anyhow::bail!("第 {} 段译文的原文已改变，无法保存手工译文", position + 1);
            };
            source.push_str(&stored.source);
            next += 1;
        }
        segments.push(crate::ai::translation::TranslationSegment {
            source,
            translated: request.translated.trim().to_string(),
        });
    }
    ensure!(
        next == machine.segments.len(),
        "该文本块的原文结构已改变，无法保存手工译文"
    );
    Ok(segments)
}

/// Non-secret identity for deciding whether persisted whole-book translations
/// are stale. A changed translation endpoint or translation model restarts
/// every translation job from the first block; the target language is part of
/// each job's identity. 问答 Endpoint 与问答模型刻意不进这个摘要：换问答模型
/// 不该让已翻完的图书重翻。
pub(crate) fn translation_execution_identity(settings: &ProviderSettings) -> Result<String> {
    let endpoint =
        normalize_provider_base_url(&settings.endpoint_for(ModelRole::Translation)?.base_url)?;
    Ok(format!(
        "{}:{}",
        crate::ai::translation::EXECUTION_IDENTITY_PROTOCOL,
        blake3::hash(format!("{}\0{}", endpoint.as_str(), settings.translation_model).as_bytes())
            .to_hex()
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
