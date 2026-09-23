//! Tests for translation job configuration and its format gates.

pub(super) use super::super::*;
pub(super) use super::*;

#[test]
fn translation_jobs_track_target_language_and_model_identity() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    // The fixture book is declared zh-CN, so a zh target has nothing to do.
    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("zh-Hans"),
            "chat-1",
            "translation-v1:a",
            true,
            10
        )
        .unwrap(),
        0
    );
    assert!(
        index_jobs::list_by_kind(&conn, "translation")
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-1",
            "translation-v1:a",
            true,
            11
        )
        .unwrap(),
        1
    );
    let job = index_jobs::get(&conn, "translation:source-1:en")
        .unwrap()
        .unwrap();
    assert_eq!(job.kind, "translation");
    assert_eq!(job.status, IndexJobStatus::Queued);
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).unwrap();
    assert_eq!(cursor.model.as_deref(), Some("chat-1"));
    assert_eq!(cursor.next_ordinal, 0);

    // An unchanged identity preserves the existing job and its progress.
    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-1",
            "translation-v1:a",
            true,
            12
        )
        .unwrap(),
        0
    );
    // A changed model keeps the durable position (0 here: nothing committed yet).
    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-2",
            "translation-v1:b",
            true,
            13
        )
        .unwrap(),
        1
    );
    let job = index_jobs::get(&conn, "translation:source-1:en")
        .unwrap()
        .unwrap();
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).unwrap();
    assert_eq!(cursor.model.as_deref(), Some("chat-2"));
    assert_eq!(cursor.next_ordinal, 0);

    // Switching the target supersedes the old language and adds the new one.
    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("ja"),
            "chat-2",
            "translation-v1:b",
            true,
            14
        )
        .unwrap(),
        2
    );
    assert_eq!(
        index_jobs::get(&conn, "translation:source-1:en")
            .unwrap()
            .unwrap()
            .status,
        IndexJobStatus::Cancelled
    );
    assert_eq!(
        index_jobs::get(&conn, "translation:source-1:ja")
            .unwrap()
            .unwrap()
            .status,
        IndexJobStatus::Queued
    );

    // Disabling translation cancels every active job.
    assert_eq!(
        reconfigure_translation_jobs(&mut conn, None, "chat-2", "translation-v1:b", true, 15)
            .unwrap(),
        1
    );
    assert_eq!(
        index_jobs::get(&conn, "translation:source-1:ja")
            .unwrap()
            .unwrap()
            .status,
        IndexJobStatus::Cancelled
    );
}

fn stored_translation(execution_identity: &str) -> NewTranslation {
    NewTranslation {
        book_id: "book-1".to_string(),
        content_unit_id: "unit-1".to_string(),
        block_id: "block-1".to_string(),
        ordinal: 0,
        document_revision: 1,
        unit_revision: 1,
        target_language: "en".to_string(),
        source_language: Some("zh".to_string()),
        model: "chat-1".to_string(),
        source_text: "searchable document body".to_string(),
        translated_text: serde_json::to_string(&crate::ai::translation::StoredTranslation {
            execution_identity: execution_identity.to_string(),
            segments: vec![crate::ai::translation::TranslationSegment {
                source: "searchable document body".to_string(),
                translated: "译".to_string(),
            }],
        })
        .unwrap(),
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn translation_identity_change_keeps_translated_blocks_and_the_durable_cursor() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    translations::upsert(&conn, &stored_translation("translation-v2:old")).unwrap();
    index_jobs::insert(
        &conn,
        &IndexJob {
            id: "translation:source-1:en".to_string(),
            book_id: "book-1".to_string(),
            source_id: Some("source-1".to_string()),
            kind: "translation".to_string(),
            status: IndexJobStatus::Queued,
            pause_requested: false,
            cancel_requested: false,
            attempts: 0,
            cursor_json: serde_json::to_string(&PersistedIndexCursor {
                schema_version: 1,
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                revision: 1,
                kind: "translation".to_string(),
                model: Some("chat-1".to_string()),
                execution_identity: Some("translation-v2:old".to_string()),
                input_execution_identity: None,
                next_ordinal: 3,
                // 执行器写下的块版本。这条测试只要求重配置把它原样带回：具体是几由
                // `ai::indexing::TRANSLATION_BLOCKS_VERSION` 决定，db 层不认识它，
                // 所以这里用一个任意的非零值来区分「带回来了」和「丢了」。
                blocks_version: 2,
            })
            .unwrap(),
            error: None,
            created_at: 1,
            updated_at: 1,
            started_at: None,
            finished_at: None,
        },
    )
    .unwrap();

    // A changed model keeps the text that is already translated and resumes
    // at the same block instead of restarting the book.
    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-2",
            "translation-v2:new",
            true,
            20
        )
        .unwrap(),
        1
    );
    let job = index_jobs::get(&conn, "translation:source-1:en")
        .unwrap()
        .unwrap();
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).unwrap();
    assert_eq!(cursor.model.as_deref(), Some("chat-2"));
    assert_eq!(
        cursor.execution_identity.as_deref(),
        Some("translation-v2:new")
    );
    assert_eq!(
        cursor.next_ordinal, 3,
        "an engine change keeps the durable position"
    );
    assert_eq!(
        cursor.blocks_version, 2,
        "重建的游标必须带着原位置的块版本：丢下它，执行器按「块列表变过」退回书首"
    );
    let rows = translations::list_for_unit(&conn, "unit-1", "en").unwrap();
    assert_eq!(rows.len(), 1, "already translated blocks are kept");
    assert_eq!(rows[0].model, "chat-2");
    let stored: crate::ai::translation::StoredTranslation =
        serde_json::from_str(&rows[0].translated_text).unwrap();
    assert_eq!(stored.execution_identity, "translation-v2:new");
    assert_eq!(stored.segments[0].translated, "译");

    // A newer protocol cannot read the stored segments, so that change still
    // discards the rows and starts the book over.
    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-3",
            "translation-v3:new",
            true,
            21
        )
        .unwrap(),
        1
    );
    assert!(
        translations::list_for_unit(&conn, "unit-1", "en")
            .unwrap()
            .is_empty()
    );
    let job = index_jobs::get(&conn, "translation:source-1:en")
        .unwrap()
        .unwrap();
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).unwrap();
    assert_eq!(cursor.next_ordinal, 0);
    assert_eq!(
        cursor.execution_identity.as_deref(),
        Some("translation-v3:new")
    );
}

#[test]
fn translation_format_gate_excludes_fixed_layout_sources() {
    for format in ["epub", "mobi", "azw", "azw3", "doc", "docx"] {
        assert!(
            format_supports_translation(format),
            "{format} must translate"
        );
    }
    for format in ["pdf", "pptx", "xlsx"] {
        assert!(
            !format_supports_translation(format),
            "{format} is outside the current translation scope"
        );
    }
}

#[test]
fn translation_jobs_skip_fixed_layout_formats() {
    let (_temp, mut conn) = open_database();
    let mut fixture = Fixture::new();
    fixture.book.format = "pdf".to_string();
    fixture.source.format = "pdf".to_string();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    assert_eq!(
        reconfigure_translation_jobs(
            &mut conn,
            Some("en"),
            "chat-1",
            "translation-v1:a",
            true,
            10
        )
        .unwrap(),
        0
    );
    assert!(
        index_jobs::list_by_kind(&conn, "translation")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn translation_jobs_start_paused_without_auto_run() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), false).unwrap();

    reconfigure_translation_jobs(
        &mut conn,
        Some("en"),
        "chat-1",
        "translation-v1:a",
        false,
        10,
    )
    .unwrap();
    assert_eq!(
        index_jobs::get(&conn, "translation:source-1:en")
            .unwrap()
            .unwrap()
            .status,
        IndexJobStatus::Paused
    );
}

#[test]
fn djvu_sources_select_the_portable_djvu_renderer() {
    let (_temp, mut conn) = open_database();
    let mut fixture = Fixture::new();
    fixture.book.format = "djvu".to_string();
    fixture.source.format = "djvu".to_string();
    fixture.source_blob.media_type = "image/vnd.djvu".to_string();
    for (index, unit) in fixture.units.iter_mut().enumerate() {
        unit.kind = "page".to_string();
        unit.source_locator_json =
            serde_json::to_string(&SourceLocator::djvu_page((index + 1) as u32)).unwrap();
    }
    insert_document(&mut conn, &fixture.graph(), true).unwrap();

    // A DjVu source must be rendered from its original bytes by the
    // portable DjVu renderer, not by the structural SVG renderer.
    let spec = running_visual_job(&conn);
    assert_eq!(spec.renderer, DJVU_RENDERER_NAME);
    assert!(spec.renderer_version.contains("djvu-rs"), "{spec:?}");
    assert_eq!(spec.fidelity, crate::preview::RenderFidelity::Structural);
    assert_eq!(spec.unit_ids, vec!["unit-1".to_string()]);

    // The same source keeps selecting the DjVu renderer when the job is
    // reconciled against the full registered descriptor set.
    let descriptors = vec![
        crate::preview::VisualRenderer::descriptor(&StructuralPngRenderer),
        crate::preview::VisualRenderer::descriptor(&DjvuPngRenderer),
    ];
    let selected = renderer_for_source(&conn, &fixture.source, &descriptors).unwrap();
    assert_eq!(selected.renderer, DJVU_RENDERER_NAME);
}
