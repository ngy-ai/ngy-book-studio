//! Tests for table-of-contents navigation.

use super::*;

#[gpui_kit::test]
pub(crate) fn toc_clicks_use_real_node_targets_and_nonoverlapping_rows(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor_with_document(cx, false, Some(prepare_toc_document));
    visual.simulate_resize(size(px(1360.), px(860.)));
    redraw_editor(visual);
    let first = visual.debug_bounds("editor-toc-toc-title").unwrap();
    let second = visual.debug_bounds("editor-toc-toc-intro").unwrap();
    assert!(first.size.height > px(0.));
    assert!(second.size.height > px(0.));
    assert!(first.bottom() <= second.top(), "TOC rows must not overlap");

    click_toc_button(visual, "editor-toc-toc-title");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-title",
        "unit-title",
        1,
        "书名页正确正文",
    );
    click_toc_button(visual, "editor-toc-toc-intro");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-intro",
        "unit-intro",
        3,
        "引言正确正文",
    );
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui_kit::test]
pub(crate) fn toc_siblings_in_one_unit_keep_independent_selection_and_unsaved_source(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_editor_with_document(cx, false, Some(prepare_toc_document));
    select_toc_node(&fixture, visual, "toc-intro-a");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-intro-a",
        "unit-intro",
        3,
        "引言正确正文",
    );
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.body_input.update(cx, |input, cx| {
                input.set_value("<h1>引言</h1><p>目录切换前尚未保存的修改。</p>", window, cx);
            });
        });
    });
    select_toc_node(&fixture, visual, "toc-intro-b");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-intro-b",
        "unit-intro",
        3,
        "目录切换前尚未保存的修改",
    );
    select_toc_node(&fixture, visual, "toc-title");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-title",
        "unit-title",
        1,
        "书名页正确正文",
    );
    select_toc_node(&fixture, visual, "toc-intro-a");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-intro-a",
        "unit-intro",
        3,
        "目录切换前尚未保存的修改",
    );
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui_kit::test]
pub(crate) fn toc_navigation_resolves_stable_ids_after_linear_units_are_reordered(
    cx: &mut TestAppContext,
) {
    fn prepare_reordered(document: &mut BookDocument) {
        prepare_toc_document(document);
        document.units.swap(0, 3);
        document.units.swap(1, 4);
    }
    let (fixture, visual) = open_editor_with_document(cx, false, Some(prepare_reordered));
    select_toc_node(&fixture, visual, "toc-title");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-title",
        "unit-title",
        4,
        "书名页正确正文",
    );
    select_toc_node(&fixture, visual, "toc-intro-b");
    assert_toc_selection(
        &fixture,
        visual,
        "toc-intro-b",
        "unit-intro",
        0,
        "引言正确正文",
    );
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui_kit::test]
pub(crate) fn saving_an_edited_unit_preserves_all_293_distinct_toc_entries(
    cx: &mut TestAppContext,
) {
    fn prepare_many_entries(document: &mut BookDocument) {
        prepare_toc_document(document);
        for index in 0..288 {
            document.toc[1].children.push(TocNode::new(
                format!("toc-detail-{index}"),
                format!("附加目录第 {} 节：保留这一项自己的完整标题", index + 1),
                TocTarget::unit("unit-intro"),
            ));
        }
    }
    let (fixture, visual) = open_editor_with_document(cx, false, Some(prepare_many_entries));
    assert_eq!(
        fixture.original.toc.len() + fixture.original.toc[1].children.len(),
        293
    );
    select_toc_node(&fixture, visual, "toc-intro-b");
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.body_input.update(cx, |input, cx| {
                input.set_value(
                    "<h1>引言</h1><p>保存正文时必须保留独立目录标签。</p>",
                    window,
                    cx,
                );
            });
            editor.save_draft(window, cx);
        });
    });
    wait_for_write(&fixture, visual);
    fixture.editor.read_with(visual, |editor, _| {
        assert!(!editor.closing);
        assert_eq!(editor.selected_toc_id.as_deref(), Some("toc-intro-b"));
        assert!(!editor.notice.as_ref().unwrap().error);
    });
    let saved = persisted_document(&fixture);
    assert!(saved.revision > fixture.original.revision);
    assert_eq!(saved.toc, fixture.original.toc);
    assert!(
        saved
            .find_unit("unit-intro")
            .unwrap()
            .source
            .contains("保存正文时必须保留独立目录标签")
    );
}

pub(crate) fn open_unchanged_media_page(
    cx: &mut TestAppContext,
) -> (EditorFixture, &mut VisualTestContext) {
    let (fixture, visual) = open_editor_with_prepared_assets(cx, false, |document| {
        prepare_toc_document(document);
        let bytes = Arc::new(b"unchanged-image-fixture".to_vec());
        let image = AssetRef::from_bytes(
            &document.id,
            AssetRole::ContentImage,
            "image/png",
            None,
            bytes.as_slice(),
        );
        let unit = &mut document.units[3];
        unit.document = BlockDocument::new(vec![
            Block::Paragraph {
                id: "typed-intro-paragraph".into(),
                content: vec![
                    Inline::text("只浏览的引言正文"),
                    Inline::HardBreak,
                    Inline::text("第二行保留原结构"),
                ],
            },
            Block::Image {
                id: "typed-intro-image".into(),
                asset_id: image.id.clone(),
                alt: "原始图片说明".into(),
                title: Some("原始图片标题".into()),
                caption: Vec::new(),
            },
        ]);
        unit.source = serialize_source(&unit.document).unwrap();
        let bytes = HashMap::from([(image.id.clone(), bytes)]);
        document.assets.push(image);
        Some(bytes)
    });
    select_toc_node(&fixture, visual, "toc-intro");
    visual.update(|_, cx| {
        fixture.editor.update(cx, |editor, _| {
            let chapter = &editor.chapters[editor.selected];
            let unit = &editor.unit_states[editor.selected];
            editor.tab = EditorTab::RichText;
            editor.web_revision = 17;
            editor.web_state.set(
                editor.web_revision,
                unit.id.clone(),
                chapter.href.clone(),
                chapter.html.clone(),
            );
            editor.active_web_page = Some(ActiveEditorPage {
                session_id: editor.web_state.session_id().to_string(),
                chapter_id: unit.id.clone(),
                href: chapter.href.clone(),
                revision: editor.web_revision,
                ready: true,
            });
        });
    });
    visual.run_until_parked();
    (fixture, visual)
}

pub(crate) fn deliver_unchanged_rich_snapshot(
    fixture: &EditorFixture,
    visual: &mut VisualTestContext,
    request_id: Option<u64>,
) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            let active = editor.active_web_page.clone().unwrap();
            let update = editor
                .web_state
                .apply_message(EditorIpcMessage {
                    session_id: active.session_id,
                    chapter_id: active.chapter_id,
                    href: active.href,
                    revision: active.revision,
                    request_id,
                    body: None,
                    selected_text: "只浏览的引言正文".into(),
                    too_large: false,
                    ready: false,
                })
                .unwrap();
            assert!(!update.edited);
            assert!(update.html.contains("<article>"));
            editor.apply_ipc_message(update, window, cx);
        });
    });
    visual.run_until_parked();
}

#[gpui_kit::test]
pub(crate) fn unchanged_rich_snapshot_preserves_typed_media_and_all_draft_state(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_unchanged_media_page(cx);
    let (generation, modified, source, html) = fixture.editor.read_with(visual, |editor, _| {
        (
            editor.draft_generation,
            editor.modified_chapter_ids.clone(),
            editor.unit_states[3].source.clone(),
            editor.chapters[3].html.clone(),
        )
    });
    assert_eq!(fixture.original.units[3].document.blocks.len(), 2);
    assert_eq!(
        fixture.original.units[3]
            .document
            .referenced_asset_ids()
            .len(),
        1
    );
    deliver_unchanged_rich_snapshot(&fixture, visual, None);
    fixture.editor.read_with(visual, |editor, _| {
        assert_eq!(editor.canonical_document.as_ref(), Some(&fixture.original));
        assert_eq!(editor.draft_generation, generation);
        assert_eq!(editor.modified_chapter_ids, modified);
        assert_eq!(editor.unit_states[3].source, source);
        assert_eq!(editor.chapters[3].html, html);
        assert_eq!(editor.ai_selected_text.as_deref(), Some("只浏览的引言正文"));
    });
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui_kit::test]
pub(crate) fn unchanged_rich_ack_runs_only_the_matching_toc_action_and_saves_typed_media(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_unchanged_media_page(cx);
    let (generation, modified) = fixture.editor.read_with(visual, |editor, _| {
        (editor.draft_generation, editor.modified_chapter_ids.clone())
    });
    visual.update(|_, cx| {
        fixture.editor.update(cx, |editor, _| {
            let active = editor.active_web_page.as_ref().unwrap();
            editor.pending_toc_id = Some("toc-title".into());
            editor.pending_snapshot = Some(PendingEditorSnapshot {
                session_id: active.session_id.clone(),
                chapter_id: active.chapter_id.clone(),
                href: active.href.clone(),
                revision: active.revision,
                request_id: 42,
                action: PendingEditorAction::SelectToc,
            });
        });
    });
    deliver_unchanged_rich_snapshot(&fixture, visual, Some(41));
    fixture.editor.read_with(visual, |editor, _| {
        assert_eq!(editor.selected, 3);
        assert_eq!(editor.selected_toc_id.as_deref(), Some("toc-intro"));
        assert_eq!(editor.pending_snapshot.as_ref().unwrap().request_id, 42);
        assert_eq!(editor.pending_toc_id.as_deref(), Some("toc-title"));
        assert_eq!(editor.canonical_document.as_ref(), Some(&fixture.original));
        assert_eq!(editor.draft_generation, generation);
        assert_eq!(editor.modified_chapter_ids, modified);
    });
    deliver_unchanged_rich_snapshot(&fixture, visual, Some(42));
    fixture.editor.read_with(visual, |editor, _| {
        assert_eq!(editor.selected, 1);
        assert_eq!(editor.selected_toc_id.as_deref(), Some("toc-title"));
        assert!(editor.pending_snapshot.is_none());
        assert!(editor.pending_toc_id.is_none());
        assert_eq!(editor.canonical_document.as_ref(), Some(&fixture.original));
        assert_eq!(editor.modified_chapter_ids, modified);
    });
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.save_draft(window, cx);
        });
    });
    wait_for_write(&fixture, visual);
    let saved = persisted_document(&fixture);
    assert!(saved.revision > fixture.original.revision);
    assert_eq!(saved.units[3].document, fixture.original.units[3].document);
    assert_eq!(saved.units[3].source, fixture.original.units[3].source);
    assert_eq!(saved.assets, fixture.original.assets);
    assert_eq!(saved.toc, fixture.original.toc);
}
