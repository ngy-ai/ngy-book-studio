//! Tests for importing, creating, editing and caching books.

use super::*;

#[test]
fn startup_backfill_fills_a_missing_language_once_and_never_replaces_one() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("library");

    // An empty library leaves the one-shot marker unwritten, so the books
    // created below still get their chance on the next open.
    let mut library = LibraryStore::load_from(dir.clone()).unwrap();
    let blank = library.create_book("待回填", "作者").unwrap();
    let spoken_for = library.create_book("已有语言", "作者").unwrap();
    drop(library);

    let conn = db::open_conn(&dir.join(db::DATABASE_FILE)).unwrap();
    conn.execute(
        "UPDATE books SET language = 'en' WHERE id = ?1",
        [spoken_for.id.as_str()],
    )
    .unwrap();
    drop(conn);

    // `create_book` stores no language while its generated original declares
    // `zh-CN` — exactly the shape the repair exists to fix.
    let reopened = LibraryStore::load_from(dir.clone()).unwrap();
    let language_of = |id: &str| {
        reopened
            .books()
            .iter()
            .find(|book| book.id == id)
            .expect("图书应仍然存在")
            .language
            .clone()
    };
    assert_eq!(language_of(&blank.id).as_deref(), Some("zh-CN"));
    assert_eq!(
        language_of(&spoken_for.id).as_deref(),
        Some("en"),
        "回填不得覆盖已记录的语言"
    );
    drop(reopened);

    // The marker makes the repair one-shot: blanking the language by hand
    // must not bring it back on the next start.
    let conn = db::open_conn(&dir.join(db::DATABASE_FILE)).unwrap();
    assert!(
        db::settings::get(&conn, LANGUAGE_BACKFILL_SETTING)
            .unwrap()
            .is_some(),
        "一次回填之后必须写下一次性标记"
    );
    conn.execute(
        "UPDATE books SET language = NULL WHERE id = ?1",
        [blank.id.as_str()],
    )
    .unwrap();
    drop(conn);

    let reopened = LibraryStore::load_from(dir).unwrap();
    assert!(
        reopened
            .books()
            .iter()
            .find(|book| book.id == blank.id)
            .unwrap()
            .language
            .is_none(),
        "标记存在时不得再次回填"
    );
}

#[test]
fn failed_object_reset_discards_the_fresh_database_and_retries_next_start() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("library");
    fs::create_dir_all(&data_dir).unwrap();
    let object_root = data_dir.join(OBJECT_DIRECTORY);

    // A non-directory at the managed object path deterministically makes
    // remove_dir_all fail on every supported platform. The database has
    // already been created by this point and must be rolled back.
    fs::write(&object_root, b"blocks object-directory reset").unwrap();
    let error = LibraryStore::load_from(data_dir.clone())
        .expect_err("an invalid managed-object path must fail startup");
    assert!(error.to_string().contains("无法清理旧对象目录"));
    assert!(!data_dir.join(db::DATABASE_FILE).exists());
    assert!(!data_dir.join("library.db-wal").exists());
    assert!(!data_dir.join("library.db-shm").exists());
    assert!(!data_dir.join("library.db-journal").exists());

    // Once the transient obstruction is gone, the missing database keeps
    // the whole reset eligible. A stale object is removed before the new
    // LocalBlobStore is published.
    fs::remove_file(&object_root).unwrap();
    fs::create_dir_all(&object_root).unwrap();
    let stale = object_root.join("stale-object");
    fs::write(&stale, b"orphan").unwrap();
    let library = LibraryStore::load_from(data_dir.clone())
        .expect("the next startup must retry and complete the reset");
    assert!(!stale.exists());
    assert!(data_dir.join(db::DATABASE_FILE).is_file());
    assert!(library.books().is_empty());
}

#[test]
fn editing_one_chapter_keeps_the_other_chapters_revision_and_translation() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().into()).unwrap();
    let (book, first_id, second_id) = create_two_unit_book(&mut library);
    let published = library.document(&book.id).unwrap();
    assert_eq!(published.revision.get(), book.revision);
    let second_revision = published
        .units
        .iter()
        .find(|unit| unit.id == second_id)
        .unwrap()
        .revision
        .get();

    // One translated block per chapter, written by the running task.
    let conn = db::open_conn(library.database_path()).unwrap();
    for unit_id in [&first_id, &second_id] {
        let unit = db::content_units::get(&conn, unit_id).unwrap().unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: book.id.clone(),
                content_unit_id: unit_id.clone(),
                block_id: format!("{unit_id}::h0"),
                ordinal: 0,
                document_revision: book.revision,
                unit_revision: unit.revision,
                target_language: "zh-Hans".to_string(),
                source_language: Some("zh".to_string()),
                model: "chat-model".to_string(),
                source_text: "原文".to_string(),
                translated_text: serde_json::to_string(
                    &crate::ai::translation::StoredTranslation {
                        execution_identity: "translation-v2:test".to_string(),
                        segments: vec![crate::ai::translation::TranslationSegment {
                            source: "原文".to_string(),
                            translated: "译文".to_string(),
                        }],
                    },
                )
                .unwrap(),
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();
    }
    drop(conn);

    library
        .update_content_unit_source(&book.id, &first_id, "<h1>改写</h1><p>新的正文</p>")
        .unwrap();

    let edited = library.document(&book.id).unwrap();
    assert_eq!(edited.revision.get(), book.revision + 1);
    let first = edited
        .units
        .iter()
        .find(|unit| unit.id == first_id)
        .unwrap();
    let second = edited
        .units
        .iter()
        .find(|unit| unit.id == second_id)
        .unwrap();
    assert_eq!(
        first.revision, edited.revision,
        "the edited chapter must take the new revision"
    );
    assert_eq!(
        second.revision.get(),
        second_revision,
        "an untouched chapter must keep its own revision"
    );

    let conn = db::open_conn(library.database_path()).unwrap();
    let kept = db::translations::list_for_unit(&conn, &second_id, "zh-Hans").unwrap();
    assert_eq!(kept.len(), 1, "an untouched chapter keeps its译文");
    assert_eq!(
        kept[0].document_revision,
        edited.revision.get(),
        "and follows the newly published document revision"
    );
    assert_eq!(kept[0].unit_revision, second_revision);
    let stale = db::translations::list_for_unit(&conn, &first_id, "zh-Hans").unwrap();
    assert_eq!(stale.len(), 1);
    assert_ne!(
        stale[0].unit_revision,
        first.revision.get(),
        "the edited chapter's译文 must be stale"
    );
    assert_eq!(stale[0].document_revision, book.revision);
}

#[test]
fn chunks_utf8_without_splitting_characters() {
    let text = "中文abc".repeat(1_000);
    let ranges = character_chunks(&text, 100, 10);
    assert!(ranges.len() > 1);
    assert!(
        ranges
            .iter()
            .all(|(start, end)| text.get(*start..*end).is_some())
    );
    assert_eq!(ranges.first().unwrap().0, 0);
    assert_eq!(ranges.last().unwrap().1, text.len());
}

#[test]
fn search_chunks_keep_block_and_text_range_locators() {
    let long_text = "中文定位".repeat(500);
    let first = Block::paragraph("block-long", long_text);
    let second = Block::paragraph("block-short", "表格附近的说明");
    let unit = DocumentUnit::new(
        "unit-locators",
        ContentUnitKind::Chapter,
        "定位测试",
        "",
        BlockDocument::new(vec![first, second]),
    )
    .with_source_locator(SourceLocator::created());
    let mut document = BookDocument::created("book-locators", "定位测试");
    document.units.push(unit);

    let chunks = build_search_chunks(&document, "source-locators", 1).unwrap();
    assert_eq!(chunks.len(), 3, "long block splits twice; short block once");
    for (ordinal, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.ordinal, ordinal);
        let locator: DocumentLocator = serde_json::from_str(&chunk.locator_json).unwrap();
        locator.validate().unwrap();
        assert_eq!(locator.source, Some(SourceLocator::created()));
        let block = document.units[0]
            .document
            .find_block(locator.block_id.as_deref().unwrap())
            .unwrap();
        let text = block.plain_text();
        let range = locator.text_range.unwrap();
        assert_eq!(
            &text[range.start_byte as usize..range.end_byte as usize],
            chunk.body
        );
    }
    assert_eq!(
        serde_json::from_str::<DocumentLocator>(&chunks[2].locator_json)
            .unwrap()
            .block_id
            .as_deref(),
        Some("block-short")
    );
}

#[test]
fn created_book_uses_object_store_and_survives_reload() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("  测试新书  ", "作者").unwrap();
    assert_eq!(book.title, "测试新书");
    assert!(library.epub_bytes(&book.id).unwrap().starts_with(b"PK"));
    let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
    assert_eq!(restored.books().len(), 1);
    let document = restored.document(&book.id).unwrap();
    assert_eq!(document.units[0].source, "<h1>第一章</h1><p>开始写作……</p>");
    let conn = db::open_conn(restored.database_path()).unwrap();
    let stored = db::content_units::get(&conn, &document.units[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.media_type.as_deref(), Some("text/html"));
    assert_eq!(
        stored.source_text.as_deref(),
        Some(document.units[0].source.as_str())
    );
    assert!(matches!(document.source, DocumentSource::Created));
}

#[test]
fn shared_auto_run_flag_pauses_new_and_edited_document_jobs() {
    fn assert_paused_jobs(library: &LibraryStore, book_id: &str, source_id: &str) {
        let conn = db::open_conn(library.database_path()).unwrap();
        let jobs = db::index_jobs::list_for_book(&conn, book_id).unwrap();
        assert_eq!(jobs.len(), 3);
        assert!(jobs.iter().all(|job| {
            job.source_id.as_deref() == Some(source_id)
                && job.status == db::index_jobs::IndexJobStatus::Paused
                && !job.pause_requested
                && !job.cancel_requested
                && job.attempts == 0
                && job.started_at.is_none()
                && job.finished_at.is_none()
        }));
    }

    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let flag = library.background_job_auto_run_flag();
    let projected_clone = library.clone();
    assert!(Arc::ptr_eq(
        &flag,
        &projected_clone.background_job_auto_run_flag()
    ));
    flag.store(false, Ordering::Release);

    let created = library.create_book("暂停派生任务", "作者").unwrap();
    let first_source = db::book_sources::get_revision(
        &db::open_conn(library.database_path()).unwrap(),
        &created.id,
        created.revision,
    )
    .unwrap()
    .unwrap();
    assert_paused_jobs(&library, &created.id, &first_source.id);

    let mut document = library.document(&created.id).unwrap();
    document.title = "暂停编辑后的派生任务".to_string();
    let updated = library.apply_document(document).unwrap();
    let second_source = db::book_sources::get_revision(
        &db::open_conn(library.database_path()).unwrap(),
        &updated.id,
        updated.revision,
    )
    .unwrap()
    .unwrap();
    assert_ne!(second_source.id, first_source.id);
    assert_paused_jobs(&library, &updated.id, &second_source.id);
    assert!(
        db::index_jobs::list_for_source_kind(
            &db::open_conn(library.database_path()).unwrap(),
            &first_source.id,
            "embedding",
        )
        .unwrap()
        .is_empty(),
        "the edited revision must remove superseded paused work"
    );
}

#[test]
fn create_book_rejects_blank_title_without_persisting_a_book() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();

    let error = library
        .create_book("  \t\r\n  ", "")
        .expect_err("blank title must be rejected");

    assert!(format!("{error:#}").contains("图书名称不能为空"));
    assert!(library.books().is_empty());
    let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
    assert!(restored.books().is_empty());
}

#[test]
fn committed_document_does_not_depend_on_a_postcommit_catalog_read() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let created = library.create_book("提交后无需读回", "作者").unwrap();
    let mut document = library.document(&created.id).unwrap();
    let unit_id = document.units[0].id.clone();
    let mut editor = crate::editing::DocumentEditor::new(document).unwrap();
    editor
        .update_unit_source(
            &unit_id,
            "<h1>已提交</h1><p>即使提交后读回失败，这一版也应成功。</p>",
        )
        .unwrap();
    document = editor.into_document();

    // `apply_document` reads the current row once, then the revision
    // transaction reads it once while holding its write lock. The former
    // implementation issued a third `books::get` after commit. Make that
    // exact next query fail: the save must still return the already
    // validated and published graph record.
    db::books::fail_get_after_for_test(2);
    let updated = library
        .apply_document(document)
        .expect("a post-commit SELECT failure must not turn a durable save into failure");
    assert_eq!(updated.revision, created.revision + 1);
    assert_eq!(
        library
            .books()
            .iter()
            .find(|record| record.id == updated.id),
        Some(&updated)
    );

    // Prove the injected failure was not silently consumed elsewhere and
    // that a subsequent normal read sees the durable row.
    let injected = library.book_record(&updated.id).unwrap_err();
    assert!(injected.to_string().contains("测试注入的图书记录读取失败"));
    assert_eq!(library.book_record(&updated.id).unwrap(), updated);
}

#[test]
fn cached_projection_preserves_newer_content_but_applies_shelf_state_and_removals() {
    let temp = tempfile::tempdir().unwrap();
    let mut canonical = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = canonical.create_book("投影合并", "作者").unwrap();
    let group = canonical.create_group("待阅读", None).unwrap();
    canonical.set_book_group(&book.id, Some(&group.id)).unwrap();
    let service_projection = canonical.clone();

    let mut window = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let mut newer_editor_record = window.book_record(&book.id).unwrap();
    newer_editor_record.title = "编辑器中的较新标题".to_string();
    newer_editor_record.revision += 1;
    newer_editor_record.updated_at += 1;
    newer_editor_record.group_id = None;
    assert!(window.apply_cached_book_update(newer_editor_record, None));

    window.merge_cached_projection(service_projection);
    let merged = &window.books()[0];
    assert_eq!(merged.title, "编辑器中的较新标题");
    assert_eq!(merged.group_id.as_deref(), Some(group.id.as_str()));
    assert_eq!(window.groups(), &[group]);

    canonical.remove_book(&book.id).unwrap();
    window.merge_cached_projection(canonical);
    assert!(window.books().is_empty());
}

#[test]
fn source_edit_is_cleaned_before_revision_is_published() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("安全编辑", "作者").unwrap();
    let unit_id = library.document(&book.id).unwrap().units[0].id.clone();

    let updated = library
        .update_content_unit_source(
            &book.id,
            &unit_id,
            "<h1>新标题</h1><p>正文<script>alert(1)</script></p>",
        )
        .unwrap();

    assert_eq!(updated.revision, book.revision + 1);
    let unit = library.document(&book.id).unwrap().units.remove(0);
    assert!(!unit.source.contains("script"));
    assert!(unit.source.contains("正文"));
}

#[test]
fn media_insert_replace_delete_uses_owned_immutable_assets() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("媒体图书", "作者").unwrap();
    let unit_id = library.document(&book.id).unwrap().units[0].id.clone();
    let audio_bytes = b"ID3-test-audio".to_vec();
    let audio = MediaDraft::from_bytes(
        MediaKind::Audio,
        "audio/mpeg",
        Some("voice.mp3".into()),
        audio_bytes.clone(),
    )
    .unwrap()
    .with_title("朗读")
    .with_description("人工说明");
    library
        .insert_media_block(&book.id, &unit_id, 1, audio)
        .unwrap();
    let document = library.document(&book.id).unwrap();
    let (block_id, audio_asset_id) = match &document.units[0].document.blocks[1] {
        Block::Audio { id, asset_id, .. } => (id.clone(), asset_id.clone()),
        other => panic!("expected audio block, got {other:?}"),
    };
    assert_eq!(
        library.asset_bytes(&book.id, &audio_asset_id).unwrap(),
        audio_bytes
    );

    let video_bytes = b"test-video-container".to_vec();
    let video = MediaDraft::from_bytes(
        MediaKind::Video,
        "video/mp4",
        Some("clip.mp4".into()),
        video_bytes.clone(),
    )
    .unwrap();
    library
        .replace_media_block(&book.id, &unit_id, &block_id, video)
        .unwrap();
    assert!(library.asset_bytes(&book.id, &audio_asset_id).is_err());
    let document = library.document(&book.id).unwrap();
    let video_asset_id = match &document.units[0].document.blocks[1] {
        Block::Video { asset_id, .. } => asset_id.clone(),
        other => panic!("expected video block, got {other:?}"),
    };
    assert_eq!(
        library.asset_bytes(&book.id, &video_asset_id).unwrap(),
        video_bytes
    );

    library
        .delete_media_block(&book.id, &unit_id, &block_id)
        .unwrap();
    assert!(library.asset_bytes(&book.id, &video_asset_id).is_err());
    assert_eq!(
        library.document(&book.id).unwrap().units[0]
            .document
            .blocks
            .len(),
        2
    );
}

#[test]
fn import_into_group_files_a_new_book_and_leaves_a_duplicate_where_it_is() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = epub_source_file(temp.path(), "分组导入");

    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let target = library.create_group("待阅读", None).unwrap();
    let other = library.create_group("参考资料", None).unwrap();

    let ImportOutcome::Added(book) = library
        .import_into_group(&source_path, Some(&target.id))
        .unwrap()
    else {
        panic!("a fresh library must not deduplicate the import");
    };
    assert_eq!(book.group_id.as_deref(), Some(target.id.as_str()));
    assert_eq!(
        library.books()[0].group_id.as_deref(),
        Some(target.id.as_str())
    );

    // The grouping is part of the import transaction, not a later repair.
    let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
    assert_eq!(
        restored.books()[0].group_id.as_deref(),
        Some(target.id.as_str())
    );

    // Re-importing the same source never moves the book the library has.
    let ImportOutcome::AlreadyExists(existing) = library
        .import_into_group(&source_path, Some(&other.id))
        .unwrap()
    else {
        panic!("the second import must be deduplicated");
    };
    assert_eq!(existing.group_id.as_deref(), Some(target.id.as_str()));
    assert_eq!(
        library.books()[0].group_id.as_deref(),
        Some(target.id.as_str())
    );
}

#[test]
fn import_without_a_group_keeps_the_book_unfiled_and_rejects_an_unknown_group() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = epub_source_file(temp.path(), "未分组导入");
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();

    let ImportOutcome::Added(book) = library.import(&source_path).unwrap() else {
        panic!("a fresh library must not deduplicate the import");
    };
    assert_eq!(book.group_id, None);

    let error = library
        .import_into_group(&source_path, Some("missing-group"))
        .expect_err("an unknown target group must not import a book unfiled");
    assert!(format!("{error:#}").contains("分组不存在"), "{error:#}");
}

#[test]
fn rename_book_updates_catalog_and_keeps_chapter_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("改名之前", "作者").unwrap();
    let unit_before = library.document(&book.id).unwrap().units[0].clone();

    let renamed = library.rename_book(&book.id, "  改名之后  ").unwrap();

    assert_eq!(renamed.title, "改名之后");
    assert_eq!(renamed.revision, book.revision + 1);
    let document = library.document(&book.id).unwrap();
    assert_eq!(document.title, "改名之后");
    // The title is metadata: a chapter whose body did not change keeps its own
    // revision, so its notes and its whole-book translation stay valid.
    assert_eq!(document.units[0].revision, unit_before.revision);
    assert_eq!(document.units[0].source, unit_before.source);

    let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
    assert_eq!(restored.books()[0].title, "改名之后");
}

#[test]
fn set_book_author_replaces_and_clears_authors_without_touching_content() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("作者测试", "原作者").unwrap();
    let unit_before = library.document(&book.id).unwrap().units[0].clone();

    let updated = library.set_book_author(&book.id, " 张三 、 李四 ").unwrap();

    assert_eq!(updated.author, "张三、李四");
    assert_eq!(updated.revision, book.revision + 1);
    let document = library.document(&book.id).unwrap();
    assert_eq!(document.authors, vec!["张三", "李四"]);
    // The author is metadata: the chapter keeps its own revision and body.
    assert_eq!(document.units[0].revision, unit_before.revision);
    assert_eq!(document.units[0].source, unit_before.source);

    let cleared = library.set_book_author(&book.id, "  \t\n ").unwrap();
    assert_eq!(cleared.author, "未知作者");
    assert!(library.document(&book.id).unwrap().authors.is_empty());

    let restored = LibraryStore::load_from(temp.path().join("library")).unwrap();
    assert!(restored.document(&book.id).unwrap().authors.is_empty());
}

#[test]
fn rename_book_rejects_a_blank_title_without_touching_the_book() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let book = library.create_book("保持原名", "作者").unwrap();

    let error = library
        .rename_book(&book.id, "  \t\n ")
        .expect_err("a blank title must be rejected");

    assert!(format!("{error:#}").contains("图书名称不能为空"));
    assert_eq!(library.books()[0].title, "保持原名");
    assert_eq!(library.document(&book.id).unwrap().title, "保持原名");
}
