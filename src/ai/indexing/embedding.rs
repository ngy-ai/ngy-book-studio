//! The embedding pass: turns canonical search chunks into vectors and records
//! one row per batch so a run can resume instead of restarting.

use super::*;

use std::{sync::Arc, time::Instant};

use anyhow::{Result, ensure};

use crate::{
    ai::EmbeddingRequest,
    ai::job_diagnostics::{JobLogEvent, JobLogMetrics},
    db,
    document::deterministic_id,
};

pub(crate) async fn run_embedding(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    models: &ModelServices,
) -> Result<RunOutcome> {
    let mut cursor = JobCursor::from_job(
        job,
        &models.config.embedding_model,
        Some(&models.config.embedding_execution_identity),
    )?;
    ensure!(
        cursor.encode()? == job.cursor_json,
        "embedding 任务未绑定当前执行身份"
    );
    if !source_is_current(inner, job, cursor.revision).await? {
        return Ok(RunOutcome::Cancelled(
            cursor,
            Some("图书已发布更新版本，任务结果已丢弃".to_string()),
        ));
    }
    // 本次运行实际使用的 provider：默认是领取时的那份快照，用户保存过新配置之后换成新的
    // （见下面每一批的重读）。
    let mut provider = Arc::clone(&models.embedding_provider);
    loop {
        if let Some(outcome) = requested_outcome(inner, job, &cursor).await? {
            return Ok(outcome);
        }
        if !source_is_current(inner, job, cursor.revision).await? {
            return Ok(RunOutcome::Cancelled(
                cursor,
                Some("图书已发布更新版本，任务结果已丢弃".to_string()),
            ));
        }

        let source_id = cursor.source_id.clone();
        let offset = cursor.next_ordinal;
        let db_path = inner.db_path.clone();
        let chunks = run_db(db_path, move |conn| {
            db::search_chunks::list_batch_for_source(
                &conn,
                &source_id,
                offset,
                EMBEDDING_BATCH_SIZE,
            )
        })
        .await?;
        if chunks.is_empty() {
            return Ok(RunOutcome::Succeeded(cursor));
        }

        let batch_started = Instant::now();
        record_index_event(
            inner,
            job,
            JobLogEvent::ItemStarted,
            JobLogMetrics {
                ordinal: Some(cursor.next_ordinal as u64),
                expected_count: Some(chunks.len() as u64),
                ..JobLogMetrics::default()
            },
        );

        let mut inputs = Vec::with_capacity(chunks.len());
        let mut total_bytes = 0usize;
        for chunk in &chunks {
            let input = bounded_embedding_input(&chunk.heading, &chunk.body);
            total_bytes = total_bytes.saturating_add(input.len());
            ensure!(
                total_bytes <= MAX_EMBEDDING_BATCH_BYTES,
                "embedding 批次超过资源上限"
            );
            inputs.push(input);
        }
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelRequested,
            JobLogMetrics {
                ordinal: Some(cursor.next_ordinal as u64),
                expected_count: Some(chunks.len() as u64),
                ..JobLogMetrics::default()
            },
        );
        // 用户在这次运行期间保存过 Endpoint/密钥时，`inner.models` 已经是新的，而这次运行
        // 手里还是领取时的快照（现场 2026-09-24：只换 key 时任务行不会被重置，运行中的执行
        // 器会一直用旧值）。本批起换用新的 provider：模型、执行身份与维度都一致才换 —— 换了
        // 身份的那次重配置会把任务行整个重置，本次运行随后会被判成已被替换。
        let live = inner.models_snapshot();
        let same_engine = live.config.embedding_model == models.config.embedding_model
            && live.config.embedding_execution_identity
                == models.config.embedding_execution_identity
            && live.config.embedding_dimensions == models.config.embedding_dimensions;
        if same_engine && !Arc::ptr_eq(&live.embedding_provider, &provider) {
            tracing::info!(
                target: "ngy_ai",
                stage = "embedding_provider_refreshed",
                next_ordinal = cursor.next_ordinal,
                model = %crate::ai::ai_diagnostics::safe_label(&models.config.embedding_model),
                "Embedding requests will use the reconfigured endpoint and key"
            );
            provider = Arc::clone(&live.embedding_provider);
        }
        let request = provider.embeddings(EmbeddingRequest {
            model: models.config.embedding_model.clone(),
            input: inputs,
            dimensions: Some(models.config.embedding_dimensions),
        });
        let batch = match await_provider_step(inner, job, &cursor, request).await? {
            Controlled::Value(batch) => batch,
            Controlled::Interrupted(outcome) => return Ok(outcome),
        };
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelCompleted,
            JobLogMetrics {
                ordinal: Some(cursor.next_ordinal as u64),
                actual_count: Some(batch.vectors.len() as u64),
                duration_ms: Some(batch_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );
        ensure!(
            batch.vectors.len() == chunks.len(),
            "embedding 返回数量与请求分块不一致"
        );
        let dimensions = batch.vectors.first().map(Vec::len).unwrap_or(0);
        ensure!(
            dimensions != 0 && dimensions <= MAX_EMBEDDING_DIMENSIONS,
            "embedding 维度不在允许范围内"
        );
        ensure!(
            batch.vectors.iter().all(|vector| {
                vector.len() == dimensions
                    && vector.iter().all(|value| value.is_finite())
                    && vector.iter().any(|value| *value != 0.0)
            }),
            "embedding 返回了无效、零值或维度不一致的向量"
        );

        let model = models.config.embedding_model.clone();
        let now = unix_timestamp()?;
        let embeddings = chunks
            .iter()
            .zip(batch.vectors)
            .map(|(chunk, vector)| {
                let vector = db::embeddings::encode_f32(&vector)?;
                Ok(db::embeddings::Embedding {
                    id: deterministic_id("embedding", format!("{}\0{model}", chunk.id).as_bytes()),
                    search_chunk_id: chunk.id.clone(),
                    model: model.clone(),
                    dimensions: vector.len() / std::mem::size_of::<f32>(),
                    vector,
                    created_at: now,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let expected_cursor_json = cursor.encode()?;
        let mut next_cursor = cursor.clone();
        next_cursor.next_ordinal += chunks.len();
        let cursor_json = next_cursor.encode()?;
        let job_id = job.id.clone();
        let db_path = inner.db_path.clone();
        let published = run_db_mut(db_path, move |mut conn| {
            db::transactions::publish_embedding_batch_for_running_job(
                &mut conn,
                &job_id,
                &expected_cursor_json,
                &cursor_json,
                &embeddings,
                now,
            )
        })
        .await?;
        if !published {
            if let Some(outcome) = requested_outcome(inner, job, &cursor).await? {
                return Ok(outcome);
            }
            return settle_interrupted_or_abandoned(inner, job).await;
        }
        record_index_event(
            inner,
            job,
            JobLogEvent::ItemSaved,
            JobLogMetrics {
                ordinal: Some(cursor.next_ordinal as u64),
                actual_count: Some(chunks.len() as u64),
                duration_ms: Some(batch_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );
        cursor = next_cursor;
    }
}

pub(crate) fn embedding_job_matches_current_execution(
    conn: &rusqlite::Connection,
    job: &db::index_jobs::IndexJob,
    model: &str,
    execution_identity: &str,
    vision_execution_identity: &str,
) -> Result<bool> {
    let Some(source_id) = job.source_id.as_deref() else {
        return Ok(false);
    };
    if job.kind != "embedding" {
        return Ok(false);
    }
    let Ok(cursor) = serde_json::from_str::<JobCursor>(&job.cursor_json) else {
        return Ok(false);
    };
    if cursor.schema_version != 1
        || cursor.book_id != job.book_id
        || cursor.source_id != source_id
        || cursor.kind != "embedding"
        || cursor.model.as_deref() != Some(model)
        || cursor.execution_identity.as_deref() != Some(execution_identity)
        || if job.id == format!("embedding:{source_id}") {
            cursor.input_execution_identity.is_some()
        } else {
            cursor.input_execution_identity.as_deref() != Some(vision_execution_identity)
        }
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

pub(crate) fn bounded_embedding_input(heading: &str, body: &str) -> String {
    let joined = if heading.trim().is_empty() {
        body.to_string()
    } else {
        format!("{}\n\n{}", heading.trim(), body)
    };
    truncate_utf8_bytes(&joined, MAX_EMBEDDING_INPUT_BYTES)
}
