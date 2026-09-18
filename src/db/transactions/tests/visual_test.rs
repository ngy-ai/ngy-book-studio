//! Tests for visual page staging, checkpointing and publication.

pub(super) use super::super::*;
pub(super) use super::*;

#[test]
fn preview_only_office_locator_cannot_be_committed_as_chat_citation() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let thread = ChatThread {
        id: "thread-preview-only".to_string(),
        book_id: Some(fixture.book.id.clone()),
        title: "Preview-only citation".to_string(),
        scope_json: r#"{"book_ids":["book-1"]}"#.to_string(),
        window_kind: "reader".to_string(),
        created_at: 2,
        updated_at: 2,
    };
    chat_threads::insert(&conn, &thread).unwrap();
    let message = ChatMessage {
        id: "message-preview-only".to_string(),
        thread_id: thread.id,
        parent_id: None,
        ordinal: 0,
        role: "assistant".to_string(),
        content: "must roll back".to_string(),
        model: Some("test-model".to_string()),
        created_at: 3,
    };
    let citation = ChatCitation {
        id: "citation-preview-only".to_string(),
        message_id: message.id.clone(),
        content_unit_id: Some(fixture.units[0].id.clone()),
        search_chunk_id: None,
        ordinal: 0,
        quote: "preview text".to_string(),
        document_revision: fixture.book.revision,
        unit_revision: fixture.units[0].revision,
        locator_json: Some(
            serde_json::to_string(
                &DocumentLocator::unit(&fixture.book.id, &fixture.units[0].id)
                    .with_source(SourceLocator::office_rendered_page(1)),
            )
            .unwrap(),
        ),
        source_kind: "book".to_string(),
        url: None,
        source_title: None,
        created_at: 3,
    };

    let error = insert_chat_message(&mut conn, &message, std::slice::from_ref(&citation), 3)
        .expect_err("preview-only locator must be rejected at the commit boundary");
    assert!(error.to_string().contains("Office 增强预览页"));
    assert!(chat_messages::get(&conn, &message.id).unwrap().is_none());
    assert!(chat_citations::get(&conn, &citation.id).unwrap().is_none());
}

#[test]
fn visual_page_checkpoints_are_atomic_and_publish_as_one_final_commit() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let spec = running_visual_job(&conn);
    let first_blob = staged_visual_blob("first", 17);
    let first_page = staged_visual_page(
        &fixture,
        &spec,
        "staged-page-first",
        0,
        &first_blob.object_key,
    );

    let empty = visual_page_staging::prefix_stats(&conn, &spec.id).unwrap();
    assert!(empty.is_contiguous_prefix(0));
    checkpoint_visual_page(&mut conn, &spec, &first_blob, &first_page, 1, 11).unwrap();
    let first_prefix = visual_page_staging::prefix_stats(&conn, &spec.id).unwrap();
    assert_eq!(first_prefix.page_count, 1);
    assert_eq!(first_prefix.min_page_index, Some(0));
    assert_eq!(first_prefix.max_page_index, Some(0));
    assert_eq!(first_prefix.total_bytes, first_blob.byte_len);
    assert!(first_prefix.is_contiguous_prefix(1));
    assert!(!blobs::is_unreferenced(&conn, &first_blob.object_key).unwrap());
    assert_eq!(
        visual_pages::list_for_source(&conn, &spec.source_id).unwrap(),
        fixture.pages,
        "a durable prefix must not expose a partial replacement"
    );

    // The blob row is inserted before the staging row in the transaction.
    // A duplicate page ID fails afterwards and must roll both metadata and
    // the cursor back to the one-page prefix.
    let rejected_blob = staged_visual_blob("rejected", 19);
    let rejected_page = staged_visual_page(
        &fixture,
        &spec,
        &first_page.id,
        1,
        &rejected_blob.object_key,
    );
    assert!(
        checkpoint_visual_page(&mut conn, &spec, &rejected_blob, &rejected_page, 2, 12).is_err()
    );
    assert!(
        blobs::get(&conn, &rejected_blob.object_key)
            .unwrap()
            .is_none()
    );
    assert!(
        visual_page_staging::prefix_stats(&conn, &spec.id)
            .unwrap()
            .is_contiguous_prefix(1)
    );
    let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
    assert_eq!(decode_persisted_visual_job(&job.cursor_json).unwrap().1, 1);

    let second_blob = staged_visual_blob("second", 23);
    let second_page = staged_visual_page(
        &fixture,
        &spec,
        "staged-page-second",
        1,
        &second_blob.object_key,
    );
    checkpoint_visual_page(&mut conn, &spec, &second_blob, &second_page, 2, 13).unwrap();
    let complete = visual_page_staging::prefix_stats(&conn, &spec.id).unwrap();
    assert!(complete.is_contiguous_prefix(2));
    assert_eq!(complete.total_bytes, 40);

    let replaced = publish_staged_visual_pages(&mut conn, &spec, 2, 14).unwrap();
    assert_eq!(replaced, vec![fixture.additional_blobs[1].clone()]);
    assert!(
        visual_page_staging::prefix_stats(&conn, &spec.id)
            .unwrap()
            .is_contiguous_prefix(0)
    );
    let pages = visual_pages::list_for_source(&conn, &spec.source_id).unwrap();
    assert_eq!(
        pages.iter().map(|page| page.page_index).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(pages.iter().all(|page| {
        page.document_revision == spec.document_revision.get()
            && page.renderer == spec.renderer
            && page.renderer_version == spec.renderer_version
    }));
    let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
    assert_eq!(job.status, IndexJobStatus::Succeeded);
    assert_eq!(decode_persisted_visual_job(&job.cursor_json).unwrap().1, 2);
    assert!(
        blobs::is_unreferenced(&conn, &fixture.additional_blobs[1].object_key).unwrap(),
        "the replaced final object must become reclaimable"
    );
}

#[test]
fn visual_job_retry_discards_staging_before_resetting_cursor() {
    let (_temp, mut conn) = open_database();
    let fixture = Fixture::new();
    insert_document(&mut conn, &fixture.graph(), true).unwrap();
    let spec = running_visual_job(&conn);
    let blob = staged_visual_blob("retry", 29);
    let page = staged_visual_page(&fixture, &spec, "staged-page-retry", 0, &blob.object_key);
    checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 11).unwrap();
    let cursor = encode_persisted_visual_job(spec.clone(), 1).unwrap();
    index_jobs::update_state(
        &conn,
        &spec.id,
        IndexJobStatus::Failed,
        &cursor,
        Some("test failure"),
        12,
        None,
        Some(12),
    )
    .unwrap();

    let reclaimable = clear_visual_page_staging(&mut conn, &spec).unwrap();
    assert_eq!(reclaimable, vec![blob.clone()]);
    assert!(
        visual_page_staging::prefix_stats(&conn, &spec.id)
            .unwrap()
            .is_contiguous_prefix(0)
    );
    let failed = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
    assert_eq!(failed.status, IndexJobStatus::Failed);
    assert_eq!(
        decode_persisted_visual_job(&failed.cursor_json).unwrap().1,
        1
    );
    let reset_cursor = encode_persisted_visual_job(spec.clone(), 0).unwrap();
    index_jobs::update_state(
        &conn,
        &spec.id,
        IndexJobStatus::Queued,
        &reset_cursor,
        None,
        13,
        None,
        None,
    )
    .unwrap();
    let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
    assert_eq!(job.status, IndexJobStatus::Queued);
    assert_eq!(decode_persisted_visual_job(&job.cursor_json).unwrap().1, 0);
    assert!(blobs::is_unreferenced(&conn, &blob.object_key).unwrap());
}

#[test]
fn visual_publication_observes_last_page_pause_and_cancel_requests() {
    for cancel in [false, true] {
        let (_temp, mut conn) = open_database();
        let fixture = Fixture::new();
        insert_document(&mut conn, &fixture.graph(), true).unwrap();
        let spec = running_visual_job(&conn);
        let suffix = if cancel { "cancel-race" } else { "pause-race" };
        let blob = staged_visual_blob(suffix, 31);
        let page = staged_visual_page(
            &fixture,
            &spec,
            &format!("staged-page-{suffix}"),
            0,
            &blob.object_key,
        );
        checkpoint_visual_page(&mut conn, &spec, &blob, &page, 1, 11).unwrap();
        if cancel {
            assert!(index_jobs::request_cancel(&conn, &spec.id, 12).unwrap() == 1);
        } else {
            assert!(index_jobs::request_pause(&conn, &spec.id, 12).unwrap() == 1);
        }

        assert!(publish_staged_visual_pages(&mut conn, &spec, 1, 13).is_err());
        assert_eq!(
            visual_pages::list_for_source(&conn, &spec.source_id).unwrap(),
            fixture.pages,
            "a final-page control request must keep the old visible set"
        );
        assert!(
            visual_page_staging::prefix_stats(&conn, &spec.id)
                .unwrap()
                .is_contiguous_prefix(1)
        );
        let job = index_jobs::get(&conn, &spec.id).unwrap().unwrap();
        assert_eq!(job.status, IndexJobStatus::Running);
        assert_eq!(job.cancel_requested, cancel);
        assert_eq!(job.pause_requested, !cancel);
    }
}
