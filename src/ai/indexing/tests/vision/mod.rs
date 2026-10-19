//! Integration tests of the visual-understanding pass and of endpoint
//! reconciliation.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use crate::{
    ai::OpenAiCompatibleProvider,
    db,
    document::{DocumentLocator, SourceLocator, deterministic_id},
    storage::BlobStore,
};

fn running_vision_job(
    fixture: &Fixture,
    model: &str,
    execution_identity: &str,
) -> db::index_jobs::IndexJob {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job_id = format!("vision:{}", fixture.source_id);
    let mut job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    let mut cursor = JobCursor::from_job(&job, model, Some(execution_identity)).unwrap();
    cursor.next_ordinal = 0;
    let cursor_json = cursor.encode().unwrap();
    db::index_jobs::update_state(
        &conn,
        &job_id,
        db::index_jobs::IndexJobStatus::Running,
        &cursor_json,
        None,
        1,
        Some(1),
        None,
    )
    .unwrap();
    job.status = db::index_jobs::IndexJobStatus::Running;
    job.cursor_json = cursor_json;
    job
}

/// 保存了新的 Endpoint/密钥（视觉模型与执行身份不变）之后，正在跑的 vision 任务必须换用新
/// provider，而不是把领取时的快照用完整本图书。
///
/// 同类现场 2026-09-24：只换 API key 时任务行不会被重置，运行中的执行器一直握着旧值。
mod endpoint_test;

mod reconfigure_test;

mod snapshot_test;
