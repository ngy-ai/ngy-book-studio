//! Untranslatable blocks, pause/cancel, cursors and cache upgrades.

use super::*;

#[test]
pub(crate) fn one_untranslatable_block_is_skipped_and_the_run_continues() {
    let endpoint = TranslationEndpoint::start("跳过：", ReplyMode::PoisonedBlock);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("skipped-block.epub");
    write_epub(
        &path,
        &format!(
            "<p>First block</p><p>{POISON_SENTINEL} never validates</p>\
             <p>Third block</p><p>Fourth block</p>"
        ),
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = translate(&services, &book_id).await;

        // 四个文本块：一个始终非法（用满块级重发预算），其余各一次请求。
        assert_eq!(endpoint.requests().len(), 3 + EXHAUSTED_BLOCK_ATTEMPTS);
        let logs = services
            .background_job_logs(job.id.clone(), vec![book_id.clone()])
            .await
            .unwrap();
        let skipped: Vec<_> = logs
            .entries
            .iter()
            .filter(|entry| entry.message.contains("跳过当前文本块"))
            .collect();
        assert_eq!(skipped.len(), 1, "跳过的文本块必须留下可读记录");
        assert_eq!(skipped[0].metrics.ordinal, Some(1));
        assert_eq!(
            skipped[0].level,
            ngy_book_studio::ai::job_diagnostics::JobLogLevel::Warning
        );
        for entry in &logs.entries {
            assert!(
                !entry.format_line().contains(POISON_SENTINEL),
                "任务日志不得包含正文"
            );
        }

        // 跳过不写入译文，但游标继续前进，读者看到的是原文。
        assert_eq!(row_count(&services), 3);
        assert_eq!(job.progress.completed, 4);
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(
            blocks
                .iter()
                .all(|block| !block.source.contains(POISON_SENTINEL))
        );
    });
}

/// 连续失败的文本块不再中止整轮：每个块都会被尝试，其余块照常翻译，任务以
/// `Succeeded` 收尾。跳过是终态——修复路径是明细里按块重试，不是再赌一次整轮。
#[test]
pub(crate) fn consecutive_untranslatable_blocks_are_skipped_until_the_whole_book_is_visited() {
    let endpoint = TranslationEndpoint::start("失败：", ReplyMode::PoisonedBlock);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("consecutive-skips.epub");
    write_epub(
        &path,
        &format!(
            "<p>First block</p><p>{POISON_SENTINEL} one</p><p>{POISON_SENTINEL} two</p>\
             <p>{POISON_SENTINEL} three</p><p>Last block</p>"
        ),
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, _unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        // 一个成功块 + 三个连续失败块 + 一个成功块：两个可译块各一次请求，三个毒块
        // 各自用满块级重发预算，最后一个块仍然被请求 —— 失败不会让运行提前结束。
        let finished = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;

        assert_eq!(endpoint.requests().len(), 2 + 3 * EXHAUSTED_BLOCK_ATTEMPTS);
        assert!(finished.error.is_none());
        // 游标走到整本末尾，跳过与缓存命中一样推进；失败块就是没有译文的那三个。
        assert_eq!(finished.progress.completed, 5);
        assert_eq!(
            finished.progress.total, None,
            "整本翻译的总块数只有文本块明细知道，任务快照不带它"
        );
        assert_eq!(row_count(&services), 2);
    });
}

#[test]
pub(crate) fn pause_and_cancel_interrupt_the_correction_before_it_can_publish() {
    for (action, expected) in [
        (BackgroundJobAction::Pause, BackgroundJobStatus::Paused),
        (BackgroundJobAction::Cancel, BackgroundJobStatus::Cancelled),
    ] {
        let endpoint = TranslationEndpoint::start("修正：", ReplyMode::GatedCorrectionValid);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("controlled-correction.epub");
        write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
            assert!(
                services
                    .control_background_job(job.id.clone(), BackgroundJobAction::Resume)
                    .await
                    .unwrap()
            );
            endpoint.wait_for_requests(2).await;
            assert_one_correction(&endpoint);
            assert_eq!(row_count(&services), 0);
            assert!(
                services
                    .control_background_job(job.id, action)
                    .await
                    .unwrap()
            );
            let controlled = wait_translation(&services, &book_id, expected).await;
            assert_eq!(controlled.progress.completed, 0);
            endpoint.release_correction();
            endpoint.wait_for_responses(2).await;
            assert!(
                services
                    .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(row_count(&services), 0);
            assert_eq!(endpoint.requests().len(), 2);
            if expected == BackgroundJobStatus::Paused {
                translate(&services, &book_id).await;
                assert_eq!(
                    endpoint.requests().len(),
                    3,
                    "resume restarts the uncommitted block"
                );
                let blocks = services
                    .translation_blocks_for_unit(book_id.clone(), unit_id)
                    .await
                    .unwrap();
                assert_translated_segments(&blocks, &endpoint.requests()[2..], "修正：");
            } else {
                wait_translation(&services, &book_id, BackgroundJobStatus::Cancelled).await;
            }
        });
    }
}

#[test]
pub(crate) fn late_invalid_correction_cannot_override_a_reconfigured_endpoint_success() {
    let old = TranslationEndpoint::start("旧：", ReplyMode::GatedCorrectionInvalid);
    let next = TranslationEndpoint::start("新：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("reconfigured-correction.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&old), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        old.wait_for_requests(2).await;
        services
            .configure_providers(settings(&next), BTreeMap::new())
            .await
            .unwrap();
        translate(&services, &book_id).await;
        assert_eq!(next.requests().len(), 1);
        old.release_correction();
        old.wait_for_responses(2).await;
        let current = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert!(current.error.is_none());
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_translated_segments(&blocks, &next.requests(), "新：");
        assert_eq!(row_count(&services), 1);
        assert_one_correction(&old);
    });
}

#[test]
pub(crate) fn failed_cursor_publication_settles_the_owned_execution_and_retry_reuses_saved_output()
{
    let endpoint = TranslationEndpoint::start("保留：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cursor-publication-failure.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        {
            // Fail only cursor advancement in this isolated database. Saving
            // the translated block and settling its old cursor remain legal.
            let conn = open_fixture_conn(services.database_path());
            conn.execute_batch(
                "CREATE TRIGGER fixture_reject_translation_progress
                 BEFORE UPDATE OF cursor_json ON index_jobs
                 WHEN NEW.kind = 'translation'
                   AND json_extract(NEW.cursor_json, '$.next_ordinal')
                     > json_extract(OLD.cursor_json, '$.next_ordinal')
                 BEGIN
                   SELECT RAISE(ABORT, 'fixture rejected translation progress');
                 END;",
            )
            .unwrap();
        }
        assert!(
            services
                .control_background_job(job.id.clone(), BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let failed = wait_translation(&services, &book_id, BackgroundJobStatus::Failed).await;
        assert_eq!(
            failed.progress.completed, 0,
            "a failed write must retain the last committed cursor"
        );
        assert!(failed.error.is_some());
        assert_eq!(endpoint.requests().len(), 1);
        assert_eq!(
            row_count(&services),
            1,
            "the block was committed before progress publication failed"
        );
        {
            let conn = open_fixture_conn(services.database_path());
            let (status, cursor): (String, String) = conn
                .query_row(
                    "SELECT status, cursor_json FROM index_jobs WHERE id = ?1",
                    [&job.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                status, "failed",
                "the failed execution must not remain workerless and running"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&cursor).unwrap()["next_ordinal"],
                0
            );
            conn.execute_batch("DROP TRIGGER fixture_reject_translation_progress;")
                .unwrap();
        }
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Retry)
                .await
                .unwrap()
        );
        let succeeded = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert_eq!(succeeded.progress.completed, 1);
        assert!(succeeded.error.is_none());
        assert_eq!(
            endpoint.requests().len(),
            1,
            "retry must reuse the already committed structured block"
        );
        assert_eq!(row_count(&services), 1);
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_translated_segments(&blocks, &endpoint.requests(), "保留：");
    });
}

/// 端点彻底联系不上时，连续若干个块的请求失败就停下来并说明原因 —— 而不是把整本书逐块
/// 跳过、最后报一个「完成」却一行译文都没有。
///
/// 现场 2026-09-22：端点配错（地址没人监听）时，用户期望「跳过这一块继续翻」，但整天都
/// 连不上时必须停：否则一本三千块的书会被逐块跳过，任务显示完成、内容还是原文。
#[test]
pub(crate) fn consecutive_failed_requests_stop_the_translation_instead_of_skipping_the_book() {
    // 绑一个端口再放掉：地址确实存在，但没有人监听，连接会立刻被拒。
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_url = format!("http://{}/v1/", dead.local_addr().unwrap());
    drop(dead);

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("dead-endpoint.epub");
    write_epub(
        &path,
        "<p>One</p><p>Two</p><p>Three</p><p>Four</p><p>Five</p><p>Six</p><p>Seven</p><p>Eight</p>",
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings_with_base_url(dead_url, 10), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, _unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id.clone(), BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let failed = wait_translation(&services, &book_id, BackgroundJobStatus::Failed).await;
        let error = failed.error.unwrap_or_default();
        assert!(
            error.contains("连续"),
            "停止的原因必须说清楚是「连续失败」而不是随便一条错误：{error}"
        );
        // 上限是 lib 内部的 `TRANSLATION_CONSECUTIVE_REQUEST_FAILURES`（= 5）：约定是
        // 「偶尔一块失败照常继续，连续五块都失败就停」，这里按同一份规则断言读数。
        assert_eq!(
            failed.progress.completed, 5,
            "恰好停在连续失败的第 5 块之后：前面的块都提交过（游标前进），后面的块没动"
        );
        assert_eq!(row_count(&services), 0, "一块都没翻出来");
        // 每一个失败块都留下「请求失败、已跳过」的记录，任务窗口据此列出「重试失败块」。
        let logs = services
            .background_job_logs(job.id, vec![book_id])
            .await
            .unwrap();
        assert_eq!(
            logs.entries
                .iter()
                .filter(|entry| entry.event == JobLogEvent::ProviderSkipped)
                .count(),
            5,
            "五个被跳过的块各自留下一条记录"
        );
    });
}

#[test]
pub(crate) fn opening_a_legacy_plain_text_cache_requires_fresh_structured_translation() {
    let endpoint = TranslationEndpoint::start("新：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.epub");
    write_epub(&path, "<p>Legacy <strong>format</strong>.</p>");
    let data_dir = temp.path().join("library");
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(&data_dir, credentials.clone()).unwrap();
    let (book_id, unit_id, job_id) = services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = translate(&services, &book_id).await;
        (book_id, unit_id, job.id)
    });
    let db_path = services.database_path().to_path_buf();
    drop(services);
    {
        // Only this isolated fixture is changed; its completed old-version
        // cursor and plaintext row reproduce a pre-format-preservation cache.
        let conn = open_fixture_conn(&db_path);
        let cursor: String = conn
            .query_row(
                "SELECT cursor_json FROM index_jobs WHERE id = ?1",
                [&job_id],
                |row| row.get(0),
            )
            .unwrap();
        let mut cursor: Value = serde_json::from_str(&cursor).unwrap();
        cursor["execution_identity"] = json!("translation-v1:legacy-fixture");
        conn.execute(
            "UPDATE index_jobs SET cursor_json = ?1 WHERE id = ?2",
            rusqlite::params![cursor.to_string(), job_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE translations SET translated_text = 'old flat translation'",
            [],
        )
        .unwrap();
    }
    let services = AppServices::open_with_credentials(&data_dir, credentials).unwrap();
    services.runtime().block_on(async {
        wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert_eq!(row_count(&services), 0);
        assert!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap()
                .is_empty()
        );
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(
            endpoint.requests().len(),
            2,
            "legacy success cursor cannot skip new structured output"
        );
        assert_translated_segments(&blocks, &endpoint.requests()[1..], "新：");
    });
}

/// The retry in [`background_job_snapshots`] is only worth having if a real
/// lock conflict is recognised, so this pins the error shape SQLite produces
/// for a second writer: a rusqlite change must not silently turn the retry into
/// dead code that lets the poller fail again.
#[test]
pub(crate) fn a_second_writer_reports_the_lock_conflict_the_poller_retries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("locked.db");
    let first = rusqlite::Connection::open(&path).unwrap();
    first.pragma_update(None, "journal_mode", "WAL").unwrap();
    first
        .execute_batch("CREATE TABLE probe (id TEXT); BEGIN IMMEDIATE;")
        .unwrap();

    let second = rusqlite::Connection::open(&path).unwrap();
    second.busy_timeout(Duration::from_millis(0)).unwrap();
    let error = anyhow::Error::new(second.execute_batch("BEGIN IMMEDIATE;").unwrap_err());
    assert!(is_database_busy(&error), "unexpected lock error: {error:#}");

    first.execute_batch("ROLLBACK;").unwrap();
}
