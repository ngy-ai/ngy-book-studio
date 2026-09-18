//! Tests for the unchanged rich-body acknowledgement.

use super::*;

#[gpui::test]
pub(crate) fn rejected_rich_body_cannot_be_saved_by_an_unchanged_ack_and_can_be_corrected(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_unchanged_media_page(cx);
    let depth = ngy_book_studio::document::MAX_DOCUMENT_DEPTH + 8;
    let rejected_body = format!(
        "<body xmlns=\"http://www.w3.org/1999/xhtml\">{}<p>合法 XML，但超过正文层级上限</p>{}</body>",
        "<div>".repeat(depth),
        "</div>".repeat(depth),
    );
    resvg::usvg::roxmltree::Document::parse(&rejected_body).unwrap();
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            let active = editor.active_web_page.clone().unwrap();
            let update = editor
                .web_state
                .apply_message(EditorIpcMessage {
                    session_id: active.session_id,
                    chapter_id: active.chapter_id.clone(),
                    href: active.href,
                    revision: active.revision,
                    request_id: None,
                    body: Some(rejected_body),
                    selected_text: String::new(),
                    too_large: false,
                    ready: false,
                })
                .unwrap();
            assert!(update.edited);
            assert!(parse_rich_text_snapshot(&update.html, &active.chapter_id).is_err());
            editor.apply_ipc_message(update, window, cx);
        });
    });
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, _| {
        assert_eq!(editor.canonical_document.as_ref(), Some(&fixture.original));
        assert!(editor.notice.as_ref().unwrap().error);
        let active = editor.active_web_page.as_ref().unwrap();
        assert_ne!(
            editor.web_state.page(active.revision).unwrap().html,
            editor.chapters[3].html,
            "the protocol page retains the body rejected by the editor"
        );
    });

    // The browser already transmitted the edit and reset its dirty flag. Its
    // exact reply to a later confirmed close therefore has body=None, while
    // the protocol page still contains the rejected, unaccepted body.
    arm_application_exit(visual);
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            let active = editor.active_web_page.clone().unwrap();
            editor.pending_snapshot = Some(PendingEditorSnapshot {
                session_id: active.session_id.clone(),
                chapter_id: active.chapter_id.clone(),
                href: active.href.clone(),
                revision: active.revision,
                request_id: 81,
                action: PendingEditorAction::Close,
            });
            let update = editor
                .web_state
                .apply_message(EditorIpcMessage {
                    session_id: active.session_id,
                    chapter_id: active.chapter_id,
                    href: active.href,
                    revision: active.revision,
                    request_id: Some(81),
                    body: None,
                    selected_text: String::new(),
                    too_large: false,
                    ready: false,
                })
                .unwrap();
            assert!(!update.edited);
            editor.apply_ipc_message(update, window, cx);
        });
    });
    visual.run_until_parked();
    assert_application_exit_cancelled(visual);
    fixture.editor.read_with(visual, |editor, _| {
        assert!(!editor.closing);
        assert!(editor.active_write.is_none());
        assert_eq!(editor.write_sequence, 0);
        assert!(editor.pending_snapshot.is_none());
        assert!(editor.notice.as_ref().unwrap().error);
        assert_eq!(editor.canonical_document.as_ref(), Some(&fixture.original));
    });
    assert_eq!(persisted_document(&fixture), fixture.original);

    let corrected_body = format!(
        "<body xmlns=\"http://www.w3.org/1999/xhtml\">{}<p>修改结构后恢复保存</p></body>",
        serialize_xhtml(&fixture.original.units[3].document).unwrap(),
    );
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            let active = editor.active_web_page.clone().unwrap();
            editor.pending_snapshot = Some(PendingEditorSnapshot {
                session_id: active.session_id.clone(),
                chapter_id: active.chapter_id.clone(),
                href: active.href.clone(),
                revision: active.revision,
                request_id: 82,
                action: PendingEditorAction::Close,
            });
            let update = editor
                .web_state
                .apply_message(EditorIpcMessage {
                    session_id: active.session_id,
                    chapter_id: active.chapter_id,
                    href: active.href,
                    revision: active.revision,
                    request_id: Some(82),
                    body: Some(corrected_body),
                    selected_text: String::new(),
                    too_large: false,
                    ready: false,
                })
                .unwrap();
            editor.apply_ipc_message(update, window, cx);
        });
    });
    wait_for_write(&fixture, visual);
    assert_ready_for_removal(&fixture, visual);
    let saved = persisted_document(&fixture);
    assert!(saved.units[3].plain_text().contains("修改结构后恢复保存"));
    assert_eq!(
        saved.units[3].document.referenced_asset_ids(),
        fixture.original.units[3].document.referenced_asset_ids()
    );
}
