//! Reference validation, freezing and loading the persisted session state.

use super::*;

pub(crate) async fn start_new_ui_session(
    services: Arc<AppServices>,
    conversation: AgentConversation,
    allowed_book_ids: Vec<String>,
) -> std::result::Result<UiSessionState, String> {
    conversation
        .start_new_session()
        .await
        .map_err(|error| format!("{error:#}"))?;
    Ok(load_ui_session_state(services, &conversation, None, &allowed_book_ids).await)
}

pub(crate) fn merge_editor_reference(
    mut references: Vec<AiReferenceHint>,
    editor_reference: Option<AiReferenceHint>,
) -> Vec<AiReferenceHint> {
    let Some(editor_reference) = editor_reference else {
        return references;
    };
    if let Some(reference) = references.iter_mut().find(|reference| {
        reference.book_id == editor_reference.book_id
            && reference.unit_id == editor_reference.unit_id
    }) {
        *reference = editor_reference;
    }
    references
}

pub(crate) fn validate_reference_request(
    references: &[AiReferenceHint],
    allowed_book_ids: &[String],
) -> Result<()> {
    if references.len() > MAX_SELECTED_REFERENCES {
        bail!("一次最多引用 {MAX_SELECTED_REFERENCES} 个章节或页面");
    }
    let allowed = allowed_book_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut frozen_bytes = 0usize;
    for reference in references {
        if !allowed.contains(reference.book_id.as_str()) {
            bail!("引用的图书已不在当前窗口授权范围内");
        }
        validate_reference_locator(reference)?;
        let locator_json = reference
            .locator
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("无法序列化引用定位信息")?;
        if !seen.insert((
            reference.book_id.as_str(),
            reference.unit_id.as_str(),
            locator_json,
        )) {
            bail!("引用列表包含重复的章节或页面");
        }
        if let Some(text) = reference.frozen_text.as_ref() {
            frozen_bytes = checked_reference_bytes(frozen_bytes, text.len())?;
        }
    }
    Ok(())
}

pub(crate) fn validate_reference_locator(reference: &AiReferenceHint) -> Result<()> {
    let Some(locator) = reference.locator.as_ref() else {
        return Ok(());
    };
    locator.validate().context("引用定位信息无效")?;
    if locator.book_id != reference.book_id || locator.unit_id != reference.unit_id {
        bail!("引用定位与图书或内容单元不一致");
    }
    if matches!(
        locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    ) {
        bail!("Office 增强预览页是可重建派生数据，不能作为 AI 引用定位");
    }
    Ok(())
}

pub(crate) fn checked_reference_bytes(current: usize, additional: usize) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .context("引用内容大小溢出")?;
    if total > MAX_REFERENCE_TOTAL_BYTES {
        bail!(
            "所选引用内容总大小超过 {} KiB",
            MAX_REFERENCE_TOTAL_BYTES / 1024
        );
    }
    Ok(total)
}

pub(crate) fn validate_explanation_reference(
    reference: &AiReferenceHint,
    allowed_book_ids: &[String],
) -> Result<()> {
    validate_reference_request(std::slice::from_ref(reference), allowed_book_ids)?;
    // Reader hints deliberately carry no editor revision. The conversation
    // authorizes their stable locator against current persisted metadata;
    // explicit revisions remain available for Editor snapshot barriers.
    let snapshot = capture_frozen_reference(reference)?.context("请先选择需要解释的文本")?;
    validate_snapshot_payload(&[snapshot])
}

pub(crate) fn capture_frozen_reference(
    reference: &AiReferenceHint,
) -> Result<Option<SelectionSnapshot>> {
    let Some(text) = reference.frozen_text.as_ref() else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let snapshot = SelectionSnapshot::capture(
        reference.book_id.clone(),
        reference.unit_id.clone(),
        reference.revision.unwrap_or(0),
        text.clone(),
    )
    .map_err(anyhow::Error::new)?;
    let snapshot = match reference.locator.clone() {
        Some(locator) => snapshot.with_locator(locator).map_err(anyhow::Error::new)?,
        None => snapshot,
    };
    Ok(Some(snapshot))
}

pub(crate) fn validate_snapshot_payload(snapshots: &[SelectionSnapshot]) -> Result<()> {
    if snapshots.len() > MAX_SELECTED_REFERENCES {
        bail!("一次最多引用 {MAX_SELECTED_REFERENCES} 个章节或页面");
    }
    let mut text_bytes = 0usize;
    for snapshot in snapshots {
        text_bytes = checked_reference_bytes(text_bytes, snapshot.text.len())?;
    }
    let encoded_bytes = serde_json::to_vec(snapshots)
        .context("无法计算引用快照大小")?
        .len();
    if encoded_bytes > MAX_REFERENCE_TOTAL_BYTES {
        bail!(
            "所选引用快照总大小超过 {} KiB",
            MAX_REFERENCE_TOTAL_BYTES / 1024
        );
    }
    Ok(())
}

pub(crate) async fn freeze_references(
    services: Arc<AppServices>,
    references: Vec<AiReferenceHint>,
    allowed_book_ids: &[String],
    cancellation: AgentCancellation,
) -> Result<Vec<SelectionSnapshot>> {
    ensure_reference_freeze_active(&cancellation)?;
    validate_reference_request(&references, allowed_book_ids)?;
    if references.is_empty() {
        return Ok(Vec::new());
    }

    let mut slots = vec![None; references.len()];
    let mut persisted = Vec::new();
    let mut frozen_bytes = 0usize;
    for (index, reference) in references.into_iter().enumerate() {
        ensure_reference_freeze_active(&cancellation)?;
        if reference.frozen_text.is_some() {
            if let Some(snapshot) = capture_frozen_reference(&reference)? {
                frozen_bytes = checked_reference_bytes(frozen_bytes, snapshot.text.len())?;
                slots[index] = Some(snapshot);
            }
        } else {
            persisted.push((index, reference));
        }
    }

    if !persisted.is_empty() {
        ensure_reference_freeze_active(&cancellation)?;
        let read_cancellation = cancellation.clone();
        let resolved = services
            .spawn_library_read(move |library| {
                ensure_reference_freeze_active(&read_cancellation)?;
                let mut resolved = Vec::with_capacity(persisted.len());
                let mut total_bytes = frozen_bytes;
                for (index, reference) in persisted {
                    ensure_reference_freeze_active(&read_cancellation)?;
                    let document = library.document(&reference.book_id)?;
                    let unit = document
                        .units
                        .iter()
                        .find(|unit| unit.id == reference.unit_id)
                        .context("引用的章节或页面已不存在")?;
                    if let Some(unit_index) = reference.unit_index {
                        let indexed = document
                            .units
                            .get(unit_index)
                            .context("引用的内容单元序号已失效")?;
                        if indexed.id != unit.id {
                            bail!("引用的内容单元 ID 与序号不一致");
                        }
                    }
                    validate_persisted_reference(&document, unit, &reference)?;
                    let text = unit.plain_text();
                    total_bytes = checked_reference_bytes(total_bytes, text.len())?;
                    let snapshot = SelectionSnapshot::capture(
                        reference.book_id,
                        unit.id.clone(),
                        unit.revision.get(),
                        text,
                    )
                    .map_err(anyhow::Error::new)?;
                    let snapshot = match reference.locator {
                        Some(locator) => {
                            snapshot.with_locator(locator).map_err(anyhow::Error::new)?
                        }
                        None => snapshot,
                    };
                    resolved.push((index, snapshot));
                }
                ensure_reference_freeze_active(&read_cancellation)?;
                Ok(resolved)
            })
            .await
            .context("引用快照任务已停止")??;
        ensure_reference_freeze_active(&cancellation)?;
        for (index, snapshot) in resolved {
            slots[index] = Some(snapshot);
        }
    }

    ensure_reference_freeze_active(&cancellation)?;
    let snapshots = slots.into_iter().flatten().collect::<Vec<_>>();
    validate_snapshot_payload(&snapshots)?;
    ensure_reference_freeze_active(&cancellation)?;
    Ok(snapshots)
}

pub(crate) fn ensure_reference_freeze_active(cancellation: &AgentCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!(AgentRequestCancelled);
    }
    Ok(())
}

pub(crate) fn validate_persisted_reference(
    document: &BookDocument,
    unit: &ngy_book_studio::document::ContentUnit,
    reference: &AiReferenceHint,
) -> Result<()> {
    if let Some(revision) = reference.revision
        && revision != unit.revision.get()
    {
        bail!("引用的内容单元版本已失效");
    }
    if let Some(locator) = reference.locator.as_ref() {
        document
            .validate_locator(locator)
            .context("引用定位已失效或超出内容范围")?;
        if let Some(source) = locator.source.as_ref()
            && unit.source_locator.as_ref() != Some(source)
        {
            bail!("引用的原始来源位置与当前内容单元不匹配");
        }
    }
    Ok(())
}

pub(crate) fn thread_options(threads: Vec<ChatThread>) -> Vec<AiThreadOption> {
    threads
        .into_iter()
        .map(|thread| AiThreadOption::new(thread.id, thread.title, thread.scope.book_ids))
        .collect()
}

pub(crate) async fn load_ui_session_state(
    services: Arc<AppServices>,
    conversation: &AgentConversation,
    session: Option<ChatSession>,
    allowed_book_ids: &[String],
) -> UiSessionState {
    let active_thread_id = session
        .as_ref()
        .map(|session| session.thread.id.clone())
        .or(conversation.selected_thread_id().await);
    let fallback_thread = session.as_ref().map(|session| session.thread.clone());
    let messages = match session {
        Some(session) => match validated_restored_messages(services, session.clone()).await {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(
                    target: "ngy_ai",
                    stage = "validate_restored_citations",
                    error_kind = error_kind(&error),
                    "cannot validate restored AI citations; restoring message text without sources"
                );
                let mut messages = restored_messages(session);
                for message in &mut messages {
                    message.sources.clear();
                }
                messages
            }
        },
        None => Vec::new(),
    };
    let threads = match conversation.list_sessions(allowed_book_ids).await {
        Ok(threads) => thread_options(threads),
        Err(error) => {
            tracing::warn!(target: "ngy_ai", stage = "list_selected_sessions", error_kind = error_kind(&error), "cannot list AI sessions after selecting a session");
            fallback_thread
                .into_iter()
                .map(|thread| AiThreadOption::new(thread.id, thread.title, thread.scope.book_ids))
                .collect()
        }
    };
    UiSessionState {
        active_thread_id,
        threads,
        messages,
    }
}
