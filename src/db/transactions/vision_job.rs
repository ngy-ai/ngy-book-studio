//! Reconciling the vision job with the stored document.

use super::*;

/// Collapses every legacy vision row for a current source into the stable
/// `vision:<source_id>` identity. Only one canonical row whose endpoint/model
/// execution identity is explicitly current may retain its state and output.
/// Duplicate, unknown-provenance, or stale-identity rows make the shared
/// visual chunks ambiguous, so they are removed and rebuilt from ordinal zero
/// in the same SQLite transaction.
///
/// A running row may be replaced here without letting its former worker
/// publish late output: every vision publication path validates the canonical
/// running row and its exact cursor under an IMMEDIATE transaction.
pub(crate) fn reconcile_current_vision_job(
    conn: &mut Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    now: u64,
) -> Result<VisionJobReconciliation> {
    validate_index_model(model, "视觉")?;
    validate_index_model(execution_identity, "视觉任务执行身份")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动视觉任务去重事务")?;
    let outcome =
        reconcile_current_vision_job_in_transaction(&tx, source, model, execution_identity, now)?;
    tx.commit().context("无法提交视觉任务去重事务")?;
    Ok(outcome)
}

/// Atomically changes the derived-index contract for every currently
/// published source. The coordinator publishes the matching in-memory model
/// handles only after this transaction succeeds, so a failed settings update
/// cannot leave a mixture of old and new source identities. Embedding jobs are
/// left byte-for-byte untouched when their endpoint/model execution contract
/// did not change; post-vision embedding jobs remain independently managed.
pub(crate) fn reconfigure_current_index_jobs(
    conn: &mut Connection,
    embedding_model: &str,
    embedding_execution_identity: &str,
    embedding_execution_changed: bool,
    vision_model: &str,
    vision_execution_identity: &str,
    now: u64,
) -> Result<()> {
    validate_index_model(embedding_model, "embedding")?;
    validate_index_model(embedding_execution_identity, "embedding 任务执行身份")?;
    validate_index_model(vision_model, "视觉")?;
    validate_index_model(vision_execution_identity, "视觉任务执行身份")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动索引模型重配置事务")?;
    for source in book_sources::list_current(&tx)? {
        reconcile_current_vision_job_in_transaction(
            &tx,
            &source,
            vision_model,
            vision_execution_identity,
            now,
        )?;
        reconcile_current_embedding_job_in_transaction(
            &tx,
            &source,
            embedding_model,
            embedding_execution_identity,
            vision_execution_identity,
            embedding_execution_changed,
            now,
        )?;
    }
    tx.commit().context("无法提交索引模型重配置事务")?;
    Ok(())
}

pub(crate) fn reconcile_current_vision_job_in_transaction(
    conn: &Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    now: u64,
) -> Result<VisionJobReconciliation> {
    ensure!(
        !model.trim().is_empty() && model.trim() == model,
        "视觉任务模型无效"
    );
    ensure!(
        !execution_identity.trim().is_empty() && execution_identity.trim() == execution_identity,
        "视觉任务执行身份无效"
    );
    let current = books::get(conn, &source.book_id)?.context("视觉任务图书不存在")?;
    ensure!(
        current.revision == source.revision
            && book_sources::get_revision(conn, &source.book_id, source.revision)?
                .is_some_and(|stored| stored.id == source.id),
        "不能为过期来源投递视觉任务"
    );

    let canonical_id = format!("vision:{}", source.id);
    let jobs = index_jobs::list_for_source_kind(conn, &source.id, "vision")?;
    let canonical = jobs.iter().find(|job| job.id == canonical_id);
    let canonical_is_current = canonical.is_some_and(|job| {
        valid_vision_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
        })
    });
    let only_current_canonical = jobs.len() == 1 && canonical_is_current;
    if only_current_canonical {
        return Ok(VisionJobReconciliation::Unchanged);
    }

    let cancel_intent = jobs.iter().any(|job| job.cancel_requested)
        || canonical.is_some_and(|job| job.status == IndexJobStatus::Cancelled);
    let pause_intent = !cancel_intent
        && (jobs.iter().any(|job| job.pause_requested)
            || canonical.is_some_and(|job| job.status == IndexJobStatus::Paused));
    let status = if cancel_intent {
        IndexJobStatus::Cancelled
    } else if pause_intent {
        IndexJobStatus::Paused
    } else {
        IndexJobStatus::Queued
    };
    let cursor = PersistedIndexCursor {
        schema_version: 1,
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        revision: source.revision,
        kind: "vision".to_string(),
        model: Some(model.to_string()),
        execution_identity: Some(execution_identity.to_string()),
        input_execution_identity: None,
        next_ordinal: 0,
        // 视觉任务的序号是页面序号，与文本块提取规则无关。
        blocks_version: 0,
    };
    let replacement = IndexJob {
        id: canonical_id,
        book_id: source.book_id.clone(),
        source_id: Some(source.id.clone()),
        kind: "vision".to_string(),
        status,
        pause_requested: false,
        cancel_requested: false,
        attempts: 0,
        cursor_json: serde_json::to_string(&cursor).context("无法序列化新视觉任务游标")?,
        error: None,
        created_at: canonical.map_or(now, |job| job.created_at),
        updated_at: now,
        started_at: None,
        finished_at: (status == IndexJobStatus::Cancelled).then_some(now),
    };
    for job in &jobs {
        index_jobs::delete(conn, &job.id)?;
    }
    search_chunks::delete_stale_visual_derived(conn, &source.id, 0)?;
    index_jobs::insert(conn, &replacement)?;
    Ok(VisionJobReconciliation::Requeued)
}
