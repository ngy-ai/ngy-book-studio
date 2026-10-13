//! Collecting the vision responses into the index.

use super::*;

pub(crate) async fn collect_vision_response(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
    mut stream: crate::ai::ChatEventStream,
) -> Result<Controlled<String>> {
    let started = Instant::now();
    let mut response = String::new();
    loop {
        tokio::select! {
            event = stream.next() => {
                let Some(event) = event else { break };
                let event = event.context("视觉模型流式响应失败")?;
                ensure!(event.tool_call_deltas.is_empty(), "视觉模型意外请求了工具");
                if let Some(delta) = event.content_delta {
                    ensure!(
                        response.len().saturating_add(delta.len()) <= MAX_VISION_RESPONSE_BYTES,
                        "视觉模型响应超过大小上限"
                    );
                    response.push_str(&delta);
                }
                if event.done {
                    break;
                }
            }
            _ = tokio::time::sleep(CONTROL_POLL_INTERVAL) => {
                if let Some(outcome) = requested_outcome(inner, job, cursor).await? {
                    return Ok(Controlled::Interrupted(outcome));
                }
                ensure!(started.elapsed() < PROVIDER_STEP_TIMEOUT, "视觉模型响应超时");
            }
        }
    }
    ensure!(!response.trim().is_empty(), "视觉模型返回了空响应");
    Ok(Controlled::Value(response))
}

pub(crate) async fn visual_pages_exist(
    inner: &Arc<IndexingInner>,
    source_id: &str,
) -> Result<bool> {
    if source_id.is_empty() {
        return Ok(false);
    }
    let source_id = source_id.to_string();
    let db_path = inner.db_path.clone();
    run_db(db_path, move |conn| {
        Ok(!db::visual_pages::list_batch_for_source(&conn, &source_id, 0, 1)?.is_empty())
    })
    .await
}

pub(crate) async fn enqueue_post_vision_embedding(
    inner: &Arc<IndexingInner>,
    vision_job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
) -> Result<bool> {
    let _transition = inner.transitions.lock().await;
    let config = inner
        .models
        .read()
        .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?
        .config
        .clone();
    let source_id = cursor.source_id.clone();
    let book_id = cursor.book_id.clone();
    let revision = cursor.revision;
    let embedding_model = config.embedding_model;
    let embedding_execution_identity = config.embedding_execution_identity;
    let vision_model = cursor.model.clone().context("视觉任务缺少模型身份")?;
    let vision_execution_identity = cursor
        .execution_identity
        .clone()
        .context("视觉任务缺少执行身份")?;
    let vision_job_id = vision_job.id.clone();
    let expected_vision_cursor_json = cursor.encode()?;
    let db_path = inner.db_path.clone();
    let queued = run_db_mut(db_path, move |mut conn| {
        let source = db::book_sources::get(&conn, &source_id)?.context("视觉任务来源不存在")?;
        ensure!(
            source.book_id == book_id && source.revision == revision,
            "视觉任务来源已经过期"
        );
        let now = unix_timestamp()?;
        let id = deterministic_id(
            "index-job",
            format!(
                "embedding-after-vision\0{source_id}\0{embedding_model}\0{embedding_execution_identity}\0{vision_model}\0{vision_execution_identity}"
            )
            .as_bytes(),
        );
        let job_cursor = JobCursor {
            schema_version: 1,
            book_id: book_id.clone(),
            source_id: source_id.clone(),
            revision,
            kind: "embedding".to_string(),
            model: Some(embedding_model),
            execution_identity: Some(embedding_execution_identity),
            input_execution_identity: Some(vision_execution_identity),
            next_ordinal: 0,
            retry_ordinals: Vec::new(),
            resume_ordinal: None,
            inflight: Vec::new(),
            blocks_version: TRANSLATION_BLOCKS_VERSION,
            priority: None,
        };
        let cursor_json = job_cursor.encode()?;
        let row = db::index_jobs::IndexJob {
            id: id.clone(),
            book_id,
            source_id: Some(source_id.clone()),
            kind: "embedding".to_string(),
            status: db::index_jobs::IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: cursor_json.clone(),
            error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        };
        db::transactions::enqueue_index_job_for_running_vision_job(
            &mut conn,
            &vision_job_id,
            &expected_vision_cursor_json,
            &source_id,
            &row,
        )
    })
    .await?;
    if queued {
        inner.wake.notify_one();
    }
    Ok(queued)
}

pub(crate) fn vision_job_matches_current_execution(
    conn: &rusqlite::Connection,
    job: &db::index_jobs::IndexJob,
    model: &str,
    execution_identity: &str,
) -> Result<bool> {
    let Some(source_id) = job.source_id.as_deref() else {
        return Ok(false);
    };
    if job.kind != "vision" || job.id != format!("vision:{source_id}") {
        return Ok(false);
    }
    let Ok(cursor) = serde_json::from_str::<JobCursor>(&job.cursor_json) else {
        return Ok(false);
    };
    if cursor.schema_version != 1
        || cursor.book_id != job.book_id
        || cursor.source_id != source_id
        || cursor.kind != "vision"
        || cursor.model.as_deref() != Some(model)
        || cursor.execution_identity.as_deref() != Some(execution_identity)
    {
        return Ok(false);
    }
    let Some(source) = db::book_sources::get(conn, source_id)? else {
        return Ok(false);
    };
    let Some(book) = db::books::get(conn, &job.book_id)? else {
        return Ok(false);
    };
    Ok(source.book_id == book.id
        && source.revision == cursor.revision
        && book.revision == cursor.revision
        && db::book_sources::get_revision(conn, &book.id, book.revision)?
            .is_some_and(|current| current.id == source.id))
}
