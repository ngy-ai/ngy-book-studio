//! Integration tests of the embedding pass, the vision pipeline and the
//! Office preview gates.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use crate::{
    db,
    document::{DocumentLocator, NormalizedRect, SourceLocator},
    storage::BlobStore,
};

use std::io::Cursor;

fn running_embedding_job(
    fixture: &Fixture,
    model: &str,
    execution_identity: &str,
) -> db::index_jobs::IndexJob {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job_id = format!("embedding:{}", fixture.source_id);
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

/// 保存了新的 Endpoint/密钥（Embedding 模型、执行身份与维度都不变）之后，正在跑的 embedding
/// 任务必须换用新 provider，而不是把领取时的快照用到任务结束。
///
/// 现场 2026-09-24（翻译同类问题）：只换 API key 时任务行不会被重置，运行中的执行器却一直
/// 握着旧 provider —— embedding 与 vision 的运行原本也在领取时固定这份快照。
mod jobs_test;

mod office_test;

mod providers_test;
