//! Integration tests of translation retries: the durable cursor rewind and
//! the per-block protocol retry budget.

use super::*;

use std::sync::{Arc, Mutex, atomic::Ordering};

use crate::{
    ai::OpenAiCompatibleProvider, ai::job_diagnostics::JobLogEvent,
    ai::translation::StoredTranslation, db,
};

/// 这条任务在队列里的那一行：`run_queued_job` 要的形状与 `list_queued` 给出的一致。
fn queued_job(fixture: &Fixture, job_id: &str) -> db::index_jobs::IndexJob {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::list_queued(&conn, 8)
        .unwrap()
        .into_iter()
        .find(|job| job.id == job_id)
        .unwrap_or_else(|| panic!("任务 {job_id} 必须回到队列"))
}

fn job_row(fixture: &Fixture, job_id: &str) -> db::index_jobs::IndexJob {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    db::index_jobs::get(&conn, job_id).unwrap().unwrap()
}

/// Retrying named blocks rewinds the durable cursor to the first requested
/// ordinal and records the bounded set. Reproduces the state a finished run
/// leaves behind (cursor at the end of the book) instead of driving a whole
/// translation, because that end state is exactly what makes the retry the
mod chapter_narrowing_test;

mod per_block_rerun_test;

mod protocol_retry_test;

mod retry_planning_test;
