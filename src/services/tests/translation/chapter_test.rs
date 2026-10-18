use super::*;

/// 阅读端的「立即翻译本章」：服务层按当前显示语言找到那本书的翻译任务，让正在跑的整本
/// 翻译先落盘，再把推进点借给这一章的块 —— 收窄范围是**本章**的块、记下借出前的位置、
/// 写下插队优先级，任务带着这些回到队列。
///
/// 收窄跑完回到借出位置由 walk 的用例固定
/// （`a_chapter_jump_returns_the_cursor_to_the_borrowed_position`）。
#[test]
fn translating_the_readers_chapter_narrows_its_task_and_prioritizes_it() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let runtime = services.runtime();

    // 与「重新翻译」那条路径相同：先关掉翻译把书建出来，再打开 —— 这样测的是任务存在
    // 之后的规划，而不是任务怎么来的。
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let book_id = runtime
        .block_on(async {
            services
                .spawn_library(|library| Ok(library.create_book("Chapter book", "Author")?.id))
                .await
                .context("library test worker stopped")?
        })
        .unwrap();
    let source_id = {
        let conn = db::open_conn(services.database_path()).unwrap();
        db::book_sources::list_for_book(&conn, &book_id)
            .unwrap()
            .into_iter()
            .next()
            .context("the created book has no source")
            .unwrap()
            .id
    };
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = Some("zh-Hans".to_string());
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let job_id = format!("translation:{source_id}:zh-Hans");

    let unit_id = {
        let conn = db::open_conn(services.database_path()).unwrap();
        db::content_units::list_for_source(&conn, &source_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("the created book has no chapter")
            .id
    };
    // 把任务停在书末：模拟「整本已经翻到后面」，借出的位置因此不等于章首。
    let parked = {
        let conn = db::open_conn(services.database_path()).unwrap();
        let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
        let mut cursor: serde_json::Value = serde_json::from_str(&job.cursor_json).unwrap();
        cursor["next_ordinal"] = serde_json::json!(2);
        // 一次跑完留下的位置一定带执行器的块版本；重配置路径写下的「较小形状」缺这一项
        // （缺省 0 = 位置来源未知），`JobCursor::from_job` 会照它读成「块列表变过」而退回
        // 书首。这里要的是一个真实存在的推进点，所以把当前版本补上（见
        // `tests/fixture.rs::finish_job_at` 的同一条说明）。
        cursor["blocks_version"] = serde_json::json!(1);
        let cursor_json = serde_json::to_string(&cursor).unwrap();
        // 停在「已暂停」：规划路径本来就只改写终态或暂停的任务，运行中的要由服务层先停。
        db::index_jobs::update_state(
            &conn,
            &job_id,
            db::index_jobs::IndexJobStatus::Paused,
            &cursor_json,
            None,
            1,
            Some(1),
            None,
        )
        .unwrap();
        cursor
    };

    block_on_without_tokio(services.translate_reader_chapter_now(book_id.clone(), unit_id.clone()))
        .unwrap();

    let conn = db::open_conn(services.database_path()).unwrap();
    let job = db::index_jobs::get(&conn, &job_id).unwrap().unwrap();
    assert_eq!(job.status, db::index_jobs::IndexJobStatus::Queued);
    let planned: serde_json::Value = serde_json::from_str(&job.cursor_json).unwrap();
    let ordinals: Vec<usize> = serde_json::from_value(planned["retry_ordinals"].clone()).unwrap();
    assert!(!ordinals.is_empty(), "收窄范围不能为空");
    assert_eq!(
        planned["next_ordinal"],
        serde_json::json!(ordinals[0]),
        "planned cursor: {planned}"
    );
    assert_eq!(
        planned["resume_ordinal"],
        serde_json::json!(parked["next_ordinal"].clone()),
        "parked: {parked} / planned: {planned}"
    );
    assert!(
        planned["priority"].as_u64().is_some(),
        "插队优先级必须写进游标"
    );
    for ordinal in &ordinals {
        let detail = block_on_without_tokio(services.background_job_translation_block_detail(
            job_id.clone(),
            *ordinal,
            vec![book_id.clone()],
        ))
        .unwrap();
        assert_eq!(detail.unit_id, unit_id, "收窄范围必须正好落在这一章");
    }

    // 没有配置目标语言时明确报错，而不是挑一章翻。
    let mut settings = services.provider_settings().unwrap();
    settings.default_language = None;
    block_on_without_tokio(services.configure_provider(settings, ApiKeyUpdate::Keep)).unwrap();
    let error = block_on_without_tokio(services.translate_reader_chapter_now(book_id, unit_id))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("尚未配置默认译文语言"),
        "{error:#}"
    );
}
