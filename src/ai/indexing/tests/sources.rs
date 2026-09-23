//! Translation sources and recorded events used by the tests.

use super::*;

/// 建好一个处于运行态的翻译任务，让测试可以直接调用 `translate_block`。
pub(crate) fn translation_block_fixture(
    provider: &Arc<MockMergeProvider>,
    identity: &str,
) -> (
    Fixture,
    db::index_jobs::IndexJob,
    JobCursor,
    TranslationServices,
) {
    translation_block_fixture_in(Fixture::new(), provider, identity)
}

/// 同上，但复用调用方已经准备好的 fixture（例如先改过正文、需要更多文本块的用例）。
pub(crate) fn translation_block_fixture_in(
    fixture: Fixture,
    provider: &Arc<MockMergeProvider>,
    identity: &str,
) -> (
    Fixture,
    db::index_jobs::IndexJob,
    JobCursor,
    TranslationServices,
) {
    let fixture = fixture;
    let coordinator = IndexingCoordinator {
        inner: fixture.indexing_inner(),
        workers: Mutex::new(Vec::new()),
    };
    let job_id = format!("translation:{}:zh-Hans", fixture.source_id);
    fixture
        .runtime
        .block_on(coordinator.configure_translation(
            provider.clone(),
            "chat-test".to_string(),
            identity.to_string(),
            TranslationGenerationSettings::default(),
            Some("zh-Hans".to_string()),
            false,
        ))
        .unwrap();
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let mut job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    let mut cursor = JobCursor::from_job(&job, "chat-test", Some(identity)).unwrap();
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
    let translation = TranslationServices {
        provider: provider.clone(),
        model: "chat-test".to_string(),
        execution_identity: identity.to_string(),
        generation: TranslationGenerationSettings::default(),
        target_language: Some("zh-Hans".to_string()),
        auto_run: false,
    };
    (fixture, job, cursor, translation)
}

/// 一个把内联强调切开的段落：首段是短名字，其余是正文。
pub(crate) fn author_bio_source() -> TranslationSource {
    TranslationSource {
        text: "Andrew Stellman is a developer and an author.".to_string(),
        segments: vec![
            "Andrew Stellman".to_string(),
            " is a developer and an author.".to_string(),
        ],
    }
}

/// 直接在单个文本块上跑一次 `translate_block`。
///
/// 作用域必须由一次运行给出：块请求靠运行最后持久化的游标 JSON 判定「这一行还是我的」，
/// 单独造一个 scope 会立刻被判成已被替换。这里用 fixture 里那份游标建运行，块号固定为
/// 0。
pub(crate) fn translate_block_source(
    fixture: &Fixture,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
    translation: &TranslationServices,
    source: &TranslationSource,
) -> Result<BlockWalk> {
    let run = TranslationRun::new(cursor.clone())?;
    let scope = run.scope(0);
    // 这些用例关心的是协议与重试行为，语境不影响它们；带语境的请求体由
    // 文本块明细与请求构造的用例单独固定。
    fixture.runtime.block_on(translate_block(
        &fixture.indexing_inner(),
        job,
        &scope,
        translation,
        "zh-Hans",
        None,
        source,
        TranslationContext::default(),
    ))
}

pub(crate) fn translate_author_bio_block(
    fixture: &Fixture,
    job: &db::index_jobs::IndexJob,
    cursor: &JobCursor,
    translation: &TranslationServices,
) -> Result<BlockWalk> {
    translate_block_source(fixture, job, cursor, translation, &author_bio_source())
}

/// 只有一个片段的文本块：没有逐片段回退可走，块级重试是它唯一的补救手段。
pub(crate) fn single_fragment_source() -> TranslationSource {
    TranslationSource {
        text: "Hello world".to_string(),
        segments: vec!["Hello world".to_string()],
    }
}

pub(crate) fn recorded_events(
    fixture: &Fixture,
    job_id: &str,
) -> Vec<crate::ai::job_diagnostics::BackgroundJobLogEntry> {
    crate::ai::job_diagnostics::JobDiagnosticStore::for_database(&fixture.db_path)
        .unwrap()
        .read(job_id)
        .unwrap()
        .entries
}
