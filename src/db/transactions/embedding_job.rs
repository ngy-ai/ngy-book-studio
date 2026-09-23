//! Reconciling the embedding job and publishing embedding batches.

use super::*;

pub(crate) fn reconcile_current_embedding_job(
    conn: &mut Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    vision_execution_identity: &str,
    now: u64,
) -> Result<()> {
    validate_index_model(model, "embedding")?;
    validate_index_model(execution_identity, "embedding 任务执行身份")?;
    validate_index_model(vision_execution_identity, "视觉任务执行身份")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动 embedding 任务收敛事务")?;
    reconcile_current_embedding_job_in_transaction(
        &tx,
        source,
        model,
        execution_identity,
        vision_execution_identity,
        true,
        now,
    )?;
    tx.commit().context("无法提交 embedding 任务收敛事务")?;
    Ok(())
}

pub(crate) fn reconcile_current_embedding_job_in_transaction(
    conn: &Connection,
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    vision_execution_identity: &str,
    reconcile_canonical: bool,
    now: u64,
) -> Result<()> {
    let current = books::get(conn, &source.book_id)?.context("embedding 任务图书不存在")?;
    ensure!(
        current.revision == source.revision
            && book_sources::get_revision(conn, &source.book_id, source.revision)?
                .is_some_and(|stored| stored.id == source.id),
        "不能为过期来源投递 embedding 任务"
    );
    let canonical_id = format!("embedding:{}", source.id);
    let jobs = index_jobs::list_for_source_kind(conn, &source.id, "embedding")?;
    let mut config_jobs = Vec::new();
    for job in &jobs {
        if embedding_config_job(job, &source.id) {
            config_jobs.push(job);
            continue;
        }
        let current_phase = valid_embedding_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
                && cursor.input_execution_identity.as_deref() == Some(vision_execution_identity)
        });
        if !current_phase
            && matches!(
                job.status,
                IndexJobStatus::Queued | IndexJobStatus::Running | IndexJobStatus::Paused
            )
        {
            index_jobs::update_state(
                conn,
                &job.id,
                IndexJobStatus::Cancelled,
                &job.cursor_json,
                Some("embedding 执行配置已经变更"),
                now,
                None,
                Some(now),
            )?;
        }
    }

    if !reconcile_canonical {
        return Ok(());
    }

    let canonical = config_jobs
        .iter()
        .copied()
        .find(|job| job.id == canonical_id);
    let canonical_is_current = canonical.is_some_and(|job| {
        valid_embedding_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
                && cursor.input_execution_identity.is_none()
        })
    });
    if canonical_is_current {
        for job in config_jobs {
            if job.id != canonical_id {
                index_jobs::delete(conn, &job.id)?;
            }
        }
        return Ok(());
    }

    let donor = config_jobs.iter().copied().find(|job| {
        valid_embedding_cursor(job, source).is_some_and(|cursor| {
            cursor.model.as_deref() == Some(model)
                && cursor.execution_identity.as_deref() == Some(execution_identity)
                && cursor.input_execution_identity.is_none()
        })
    });
    let cancel_intent = config_jobs.iter().any(|job| job.cancel_requested)
        || canonical.is_some_and(|job| job.status == IndexJobStatus::Cancelled);
    let pause_intent = !cancel_intent
        && (config_jobs.iter().any(|job| job.pause_requested)
            || canonical.is_some_and(|job| job.status == IndexJobStatus::Paused));
    let cursor = PersistedIndexCursor {
        schema_version: 1,
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        revision: source.revision,
        kind: "embedding".to_string(),
        model: Some(model.to_string()),
        execution_identity: Some(execution_identity.to_string()),
        input_execution_identity: None,
        next_ordinal: donor
            .and_then(|job| valid_embedding_cursor(job, source))
            .map_or(0, |cursor| cursor.next_ordinal),
        // 位置从同一身份的捐赠游标继承，块版本只对整本翻译有意义（见
        // `PersistedIndexCursor::blocks_version`），这里没有要带的东西。
        blocks_version: 0,
    };
    let cursor_json = serde_json::to_string(&cursor).context("无法序列化 embedding 任务游标")?;
    let mut row = donor
        .cloned()
        .or_else(|| canonical.cloned())
        .unwrap_or_else(|| IndexJob {
            id: canonical_id.clone(),
            book_id: source.book_id.clone(),
            source_id: Some(source.id.clone()),
            kind: "embedding".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: cursor_json.clone(),
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        });
    if donor.is_none() {
        embeddings::delete_for_source_model(conn, &source.id, model)?;
    }
    row.id = canonical_id;
    row.cursor_json = cursor_json;
    if donor.is_none() {
        row.status = if cancel_intent {
            IndexJobStatus::Cancelled
        } else if pause_intent {
            IndexJobStatus::Paused
        } else {
            IndexJobStatus::Queued
        };
        row.error = None;
        row.started_at = None;
        row.finished_at = (row.status == IndexJobStatus::Cancelled).then_some(now);
    } else if row.status == IndexJobStatus::Running {
        row.status = IndexJobStatus::Queued;
        row.started_at = None;
        row.finished_at = None;
    }
    row.pause_requested = false;
    row.cancel_requested = false;
    row.updated_at = now;
    for job in config_jobs {
        index_jobs::delete(conn, &job.id)?;
    }
    index_jobs::insert(conn, &row)?;
    Ok(())
}

pub(crate) fn embedding_config_job(job: &IndexJob, source_id: &str) -> bool {
    if job.id == format!("embedding:{source_id}") {
        return true;
    }
    let Ok(cursor) = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json) else {
        return false;
    };
    cursor.model.as_deref().is_some_and(|model| {
        job.id
            == deterministic_id(
                "index-job",
                format!("model\0embedding\0{source_id}\0{model}").as_bytes(),
            )
    })
}

pub(crate) fn valid_embedding_cursor(
    job: &IndexJob,
    source: &BookSource,
) -> Option<PersistedIndexCursor> {
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).ok()?;
    (job.kind == "embedding"
        && job.book_id == source.book_id
        && job.source_id.as_deref() == Some(source.id.as_str())
        && cursor.schema_version == 1
        && cursor.book_id == source.book_id
        && cursor.source_id == source.id
        && cursor.revision == source.revision
        && cursor.kind == "embedding")
        .then_some(cursor)
}

/// Publishes one embedding batch and advances its durable cursor in the same
/// write transaction. Reconfiguration replaces the cursor generation, so an
/// old provider response cannot write vectors after an endpoint/model switch.
pub(crate) fn publish_embedding_batch_for_running_job(
    conn: &mut Connection,
    job_id: &str,
    expected_cursor_json: &str,
    cursor_json: &str,
    rows: &[Embedding],
    now: u64,
) -> Result<bool> {
    ensure!(!rows.is_empty(), "embedding 批次不能为空");
    let expected = serde_json::from_str::<PersistedIndexCursor>(expected_cursor_json)
        .context("embedding 任务旧游标无效")?;
    let next = serde_json::from_str::<PersistedIndexCursor>(cursor_json)
        .context("embedding 任务新游标无效")?;
    ensure!(
        expected.schema_version == 1
            && expected.kind == "embedding"
            && expected.model.is_some()
            && expected.execution_identity.is_some()
            && next.schema_version == expected.schema_version
            && next.book_id == expected.book_id
            && next.source_id == expected.source_id
            && next.revision == expected.revision
            && next.kind == expected.kind
            && next.model == expected.model
            && next.execution_identity == expected.execution_identity
            && next.input_execution_identity == expected.input_execution_identity
            && next.next_ordinal == expected.next_ordinal.saturating_add(rows.len()),
        "embedding 批次游标与执行代次不匹配"
    );
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动 embedding 批次发布事务")?;
    let Some(job) = index_jobs::get(&tx, job_id)? else {
        return Ok(false);
    };
    if job.kind != "embedding"
        || job.book_id != expected.book_id
        || job.source_id.as_deref() != Some(expected.source_id.as_str())
        || job.status != IndexJobStatus::Running
        || job.pause_requested
        || job.cancel_requested
        || job.cursor_json != expected_cursor_json
    {
        return Ok(false);
    }
    let source =
        book_sources::get(&tx, &expected.source_id)?.context("embedding 批次来源不存在")?;
    let book = books::get(&tx, &expected.book_id)?.context("embedding 批次图书不存在")?;
    ensure!(
        source.book_id == book.id
            && source.revision == expected.revision
            && book.revision == expected.revision
            && book_sources::get_revision(&tx, &book.id, book.revision)?
                .is_some_and(|current| current.id == source.id),
        "embedding 批次来源已经过期"
    );
    for row in rows {
        ensure!(
            row.model.as_str() == expected.model.as_deref().unwrap_or_default()
                && row.dimensions != 0
                && row.vector.len() == row.dimensions.saturating_mul(std::mem::size_of::<f32>()),
            "embedding 批次向量与任务模型不匹配"
        );
        let chunk = search_chunks::get(&tx, &row.search_chunk_id)?
            .context("embedding 批次搜索分块不存在")?;
        ensure!(
            chunk.book_id == expected.book_id && chunk.source_id == expected.source_id,
            "embedding 批次搜索分块越界"
        );
        embeddings::upsert(&tx, row)?;
    }
    ensure!(
        index_jobs::advance_running_cursor_from(
            &tx,
            job_id,
            expected_cursor_json,
            cursor_json,
            now,
        )?,
        "embedding 批次提交前任务代次发生变化"
    );
    tx.commit().context("无法提交 embedding 批次发布事务")?;
    Ok(true)
}
