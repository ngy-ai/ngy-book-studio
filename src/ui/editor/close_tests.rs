//! Exercise production editor navigation and close confirmation with a
//! temporary library. GPUI's test platform answers the prompt; no native
//! WebView is built.
//! The fixture suppresses only the final native window-removal callback because
//! GPUI test windows have no HWND. These tests verify confirmation, real writes,
//! and the transition into closing with its protocol released; native window
//! destruction and WebView teardown still require Windows GUI acceptance.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use moye_epub_editor::document::{
    BlockDocument, ContentUnit as DocumentUnit, SourceLocator, TocTarget,
};
use std::time::{Duration, Instant};

struct EditorFixture {
    editor: Entity<EditorApp>,
    _library_view: Entity<EpubReaderApp>,
    services: Arc<AppServices>,
    original: BookDocument,
    _directory: tempfile::TempDir,
}

fn open_editor(
    cx: &mut TestAppContext,
    webview_building: bool,
) -> (EditorFixture, &mut VisualTestContext) {
    open_editor_with_document(cx, webview_building, None)
}

fn open_editor_with_document(
    cx: &mut TestAppContext,
    webview_building: bool,
    prepare: Option<fn(&mut BookDocument)>,
) -> (EditorFixture, &mut VisualTestContext) {
    open_editor_with_prepared_assets(cx, webview_building, |document| {
        prepare.map(|prepare| {
            prepare(document);
            HashMap::new()
        })
    })
}

fn open_editor_with_prepared_assets(
    cx: &mut TestAppContext,
    webview_building: bool,
    prepare: impl FnOnce(&mut BookDocument) -> Option<HashMap<String, Arc<Vec<u8>>>>,
) -> (EditorFixture, &mut VisualTestContext) {
    let directory = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(directory.path()).unwrap());
    let mut created = services.runtime().block_on(async {
        services
            .spawn_library_projected(|library| library.create_book("关闭确认原书名", "测试作者"))
            .await
            .unwrap()
            .unwrap()
    });
    let book_id = created.value.id.clone();
    let mut original = services.runtime().block_on(async {
        services
            .spawn_library_read(move |library| library.document(&book_id))
            .await
            .unwrap()
            .unwrap()
    });
    if let Some(asset_bytes) = prepare(&mut original) {
        original.validate().unwrap();
        created = services.runtime().block_on(async {
            services
                .spawn_library_projected(move |library| {
                    library.apply_document_with_assets(original, asset_bytes)
                })
                .await
                .unwrap()
                .unwrap()
        });
        let book_id = created.value.id.clone();
        original = services.runtime().block_on(async {
            services
                .spawn_library_read(move |library| library.document(&book_id))
                .await
                .unwrap()
                .unwrap()
        });
    }
    let chapters = editor_chapters_from_document(&original).unwrap();
    let web_state = EditorWebState::new(
        original.id.clone(),
        chapters[0].href.clone(),
        chapters[0].html.clone(),
    );
    web_state.authorize_media(&original).unwrap();
    cx.update(gpui_component::init);
    let mut editor_entity = None;
    let mut library_entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let library_view = cx.new(|cx| {
            EpubReaderApp::new(created.snapshot.clone(), Arc::clone(&services), window, cx)
        });
        let title_input =
            cx.new(|cx| InputState::new(window, cx).default_value(created.value.title.clone()));
        let author_input =
            cx.new(|cx| InputState::new(window, cx).default_value(created.value.author.clone()));
        let chapter_title_input =
            cx.new(|cx| InputState::new(window, cx).default_value(chapters[0].title.clone()));
        let body_input = cx.new(|cx| InputState::new(window, cx).multi_line(true));
        let editor = cx.new(|cx| {
            let mut editor = EditorApp::new(
                original.id.clone(),
                created.snapshot,
                created.generation,
                Arc::clone(&services),
                library_view.downgrade(),
                None,
                title_input,
                author_input,
                chapter_title_input,
                body_input,
                chapters,
                original.clone(),
                web_state,
                webview_building,
                window,
                cx,
            );
            // run_until_parked may draw refreshed frames. Keep the production
            // close transition, but prevent Render from scheduling an HWND
            // removal for the GPUI test platform's synthetic window.
            editor.removal_scheduled = true;
            editor
        });
        editor_entity = Some(editor.clone());
        library_entity = Some(library_view);
        Root::new(editor, window, cx)
    });
    (
        EditorFixture {
            editor: editor_entity.unwrap(),
            _library_view: library_entity.unwrap(),
            services,
            original,
            _directory: directory,
        },
        visual,
    )
}

fn persisted_document(fixture: &EditorFixture) -> BookDocument {
    let book_id = fixture.original.id.clone();
    fixture.services.runtime().block_on(async {
        fixture
            .services
            .spawn_library_read(move |library| library.document(&book_id))
            .await
            .unwrap()
            .unwrap()
    })
}

fn edit_draft(fixture: &EditorFixture, visual: &mut VisualTestContext, title: &str) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.title_input.update(cx, |input, cx| {
                input.set_value(title.to_string(), window, cx);
            });
            editor.body_input.update(cx, |input, cx| {
                input.set_value("# 第一章\n\n尚未保存的正文。", window, cx);
            });
        });
    });
    visual.run_until_parked();
}

fn request_close(fixture: &EditorFixture, visual: &mut VisualTestContext) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            assert!(!editor.handle_window_close(window, cx));
        });
    });
    visual.run_until_parked();
}

fn assert_prompt_open(fixture: &EditorFixture, visual: &VisualTestContext) {
    assert!(visual.has_pending_prompt());
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.close_prompt_open);
        assert!(!editor.closing);
        assert!(editor.active_write.is_none());
    });
}

fn assert_ready_for_removal(fixture: &EditorFixture, visual: &VisualTestContext) {
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.closing);
        assert!(editor.active_write.is_none());
        assert!(!editor.web_state.protocol_is_open());
        assert!(editor.pending_snapshot.is_none());
        assert!(editor.pending_ready_action.is_none());
        assert!(editor.ipc_sync_task.is_none());
    });
}

fn wait_for_write(fixture: &EditorFixture, visual: &mut VisualTestContext) {
    // Storage uses the independent IoRuntime, so a parked GPUI executor does
    // not by itself mean the background write has completed.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        visual.run_until_parked();
        if fixture
            .editor
            .read_with(visual, |editor, _| editor.active_write.is_none())
        {
            return;
        }
        assert!(Instant::now() < deadline, "editor write did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn close_requires_an_answer_and_cancel_preserves_the_draft(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "取消后保留的书名");
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);

    // Repeated close messages must leave one pending question and no write.
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    visual.simulate_prompt_answer("取消");
    visual.run_until_parked();
    assert!(!visual.has_pending_prompt());
    fixture.editor.read_with(visual, |editor, cx| {
        assert!(!editor.close_prompt_open);
        assert!(!editor.closing);
        assert!(editor.active_write.is_none());
        assert_eq!(editor.title_input.read(cx).value(), "取消后保留的书名");
        assert_eq!(
            editor.body_input.read(cx).value(),
            "# 第一章\n\n尚未保存的正文。"
        );
    });
    assert_eq!(persisted_document(&fixture), fixture.original);

    // Cancelling also leaves the next close request usable.
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    visual.simulate_prompt_answer("取消");
    visual.run_until_parked();
}

#[gpui::test]
fn close_without_saving_keeps_the_persisted_document(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "不应该保存的书名");
    request_close(&fixture, visual);
    visual.simulate_prompt_answer("不保存");
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.closing);
        assert!(!editor.close_prompt_open);
        assert!(editor.active_write.is_none());
        assert_eq!(editor.write_sequence, 0);
    });
    assert_ready_for_removal(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui::test]
fn save_and_close_waits_for_the_confirmed_background_commit(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "确认后保存的书名");
    request_close(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);

    // Reserve the shared mutation queue before confirming. The real save must
    // wait, which lets us verify the window remains open until its commit.
    let (release, wait) = std::sync::mpsc::channel();
    let blocker = fixture.services.spawn_library(move |_| {
        wait.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    });
    visual.simulate_prompt_answer("保存并关闭");
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, _| {
        assert!(!editor.closing);
        assert!(!editor.close_prompt_open);
        assert_eq!(
            editor.active_write.as_ref().unwrap().intent,
            EditorWriteIntent::Close
        );
    });
    release.send(()).unwrap();
    fixture
        .services
        .runtime()
        .block_on(blocker)
        .unwrap()
        .unwrap();
    wait_for_write(&fixture, visual);
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.closing);
        assert!(editor.active_write.is_none());
    });
    assert_ready_for_removal(&fixture, visual);
    let saved = persisted_document(&fixture);
    assert_eq!(saved.title, "确认后保存的书名");
    assert!(saved.units[0].source.contains("尚未保存的正文"));
    assert!(saved.revision > fixture.original.revision);
}

#[gpui::test]
fn rejected_save_keeps_the_window_open_and_can_be_corrected(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "");
    request_close(&fixture, visual);
    visual.simulate_prompt_answer("保存并关闭");
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, cx| {
        assert!(!editor.closing);
        assert!(!editor.close_prompt_open);
        assert!(editor.active_write.is_none());
        assert_eq!(editor.title_input.read(cx).value(), "");
        assert!(editor.notice.as_ref().unwrap().error);
        assert!(
            editor
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("书名不能为空")
        );
    });
    assert_eq!(persisted_document(&fixture), fixture.original);

    edit_draft(&fixture, visual, "修正后再次确认");
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    visual.simulate_prompt_answer("保存并关闭");
    wait_for_write(&fixture, visual);
    assert_ready_for_removal(&fixture, visual);
    assert_eq!(persisted_document(&fixture).title, "修正后再次确认");
}

#[gpui::test]
fn background_save_failure_keeps_the_draft_and_reopens_confirmation(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "本窗口未保存的书名");
    let mut winner = fixture.original.clone();
    winner.title = "另一个窗口已经保存".into();
    let saved = fixture.services.runtime().block_on(async {
        spawn_editor_write(
            &fixture.services,
            EditorWriteJob {
                document: winner,
                new_asset_bytes: HashMap::new(),
                export_target: None,
            },
        )
        .await
        .unwrap()
        .unwrap()
        .value
        .document
    });
    request_close(&fixture, visual);
    visual.simulate_prompt_answer("保存并关闭");
    wait_for_write(&fixture, visual);
    fixture.editor.read_with(visual, |editor, cx| {
        assert!(!editor.closing);
        assert!(!editor.close_after_write);
        assert_eq!(editor.title_input.read(cx).value(), "本窗口未保存的书名");
        assert!(
            editor
                .body_input
                .read(cx)
                .value()
                .contains("尚未保存的正文")
        );
        assert!(editor.notice.as_ref().unwrap().error);
        assert!(editor.notice.as_ref().unwrap().text.contains("保存失败"));
    });
    assert_eq!(persisted_document(&fixture), saved);
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    visual.simulate_prompt_answer("取消");
    visual.run_until_parked();
    assert!(!fixture.editor.read_with(visual, |editor, _| editor.closing));
}

#[gpui::test]
fn close_during_an_explicit_save_waits_and_then_asks(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "用户已点击保存");
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.begin_action(PendingEditorAction::Save, window, cx);
            assert!(editor.active_write.is_some());
            assert!(!editor.handle_window_close(window, cx));
            assert!(editor.close_confirmation_pending);
            assert!(!editor.close_after_write);
            assert!(!editor.close_prompt_open);
            assert!(!editor.closing);
        });
    });
    wait_for_write(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    assert_eq!(persisted_document(&fixture).title, "用户已点击保存");
    let saved = persisted_document(&fixture);
    visual.simulate_prompt_answer("取消");
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, _| {
        assert!(!editor.closing);
        assert!(!editor.close_confirmation_pending);
        assert_eq!(editor.write_sequence, 1);
    });
    assert_eq!(persisted_document(&fixture), saved);
}

#[gpui::test]
fn discard_during_a_previously_requested_write_waits_without_another_save(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "已经请求的保存");
    request_close(&fixture, visual);
    let (release, wait) = std::sync::mpsc::channel();
    let blocker = fixture.services.spawn_library(move |_| {
        wait.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    });
    // Model an earlier save reaching its snapshot barrier while the prompt
    // is open. This already authorized write must finish its state callback.
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.perform_save_draft(window, cx);
            assert!(editor.active_write.is_some());
            assert!(editor.close_prompt_open);
        });
    });
    visual.simulate_prompt_answer("不保存");
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.close_without_saving_pending);
        assert!(editor.active_write.is_some());
        assert!(!editor.closing);
        assert!(!editor.close_after_write);
    });
    release.send(()).unwrap();
    fixture
        .services
        .runtime()
        .block_on(blocker)
        .unwrap()
        .unwrap();
    wait_for_write(&fixture, visual);
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.closing);
        assert!(!editor.close_without_saving_pending);
        assert_eq!(editor.write_sequence, 1);
    });
    assert_ready_for_removal(&fixture, visual);
    assert_eq!(persisted_document(&fixture).title, "已经请求的保存");
}

#[gpui::test]
fn failed_webview_build_resumes_close_with_a_question(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, true);
    edit_draft(&fixture, visual, "初始化期间的草稿");
    request_close(&fixture, visual);
    assert!(!visual.has_pending_prompt());
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.webview_build_gate.close_requested);
        assert!(editor.active_write.is_none());
    });
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.fail_webview_build("测试构造失败".into(), window, cx);
        });
    });
    visual.run_until_parked();
    assert_prompt_open(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);
    visual.simulate_prompt_answer("不保存");
    visual.run_until_parked();
    assert_ready_for_removal(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui::test]
fn removing_the_book_closes_without_saving_or_asking(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "删除后不应保存的书名");
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.close_for_removed_book(window, cx);
        });
    });
    visual.run_until_parked();
    // The document these edits belong to is already gone: unsaved work is
    // discarded and no confirmation may keep the window open.
    assert!(!visual.has_pending_prompt());
    assert_ready_for_removal(&fixture, visual);
    fixture.editor.read_with(visual, |editor, _| {
        assert!(!editor.close_prompt_open);
        assert_eq!(editor.write_sequence, 0);
    });
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui::test]
fn removing_the_book_during_webview_build_discards_the_draft(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, true);
    edit_draft(&fixture, visual, "初始化期间的草稿");
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.close_for_removed_book(window, cx);
        });
    });
    visual.run_until_parked();
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.closing_for_removed_book);
        assert!(!editor.closing);
        assert!(editor.webview_build_gate.close_requested);
    });
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.fail_webview_build("测试构造失败".into(), window, cx);
        });
    });
    visual.run_until_parked();
    // The resumed close must not turn into a save question.
    assert!(!visual.has_pending_prompt());
    assert_ready_for_removal(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);
}

const TITLE_TOC_LABEL: &str = "书名页：人工智能代理系统的设计、实现与实践指南";
const INTRO_TOC_LABEL: &str = "引言：从阅读实例到构建可以验证的完整应用";

fn prepare_toc_document(document: &mut BookDocument) {
    // The linear sequence includes a cover and a navigation document that the
    // actual TOC omits. Its row ordinals must never become unit ordinals.
    document.units = [
        ("unit-cover", "封面内部标题", "只有封面内容"),
        ("unit-title", "书名页内部标题", "书名页正确正文"),
        ("unit-nav", "导航文件内部标题", "仅用于导航的文件内容"),
        ("unit-intro", "引言内部标题", "引言正确正文"),
        ("unit-chapter", "第一章内部标题", "第一章正确正文"),
    ]
    .into_iter()
    .map(|(id, title, body)| {
        let source = format!("# {title}\n\n{body}\n");
        let parsed = parse_source_for_unit(SourceKind::Markdown, &source, id).unwrap();
        DocumentUnit::new(
            id,
            ContentUnitKind::Chapter,
            title,
            SourceKind::Markdown,
            parsed.canonical_source,
            parsed.document,
        )
        .with_source_locator(SourceLocator::created())
    })
    .collect();
    let mut introduction =
        TocNode::new("toc-intro", INTRO_TOC_LABEL, TocTarget::unit("unit-intro"));
    introduction.children = vec![
        TocNode::new(
            "toc-intro-a",
            "引言第一节：为什么需要工具调用",
            TocTarget::unit("unit-intro"),
        ),
        TocNode::new(
            "toc-intro-b",
            "引言第二节：如何验证运行结果",
            TocTarget::unit("unit-intro"),
        ),
    ];
    document.toc = vec![
        TocNode::new("toc-title", TITLE_TOC_LABEL, TocTarget::unit("unit-title")),
        introduction,
        TocNode::new(
            "toc-chapter",
            "第一章：一条完整的执行路径",
            TocTarget::unit("unit-chapter"),
        ),
    ];
}

fn redraw_editor(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear());
    visual.run_until_parked();
}

fn click_toc_button(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .expect("the actual TOC node button must be rendered");
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    visual.simulate_click(bounds.center(), Modifiers::none());
    redraw_editor(visual);
}

fn select_toc_node(fixture: &EditorFixture, visual: &mut VisualTestContext, node_id: &str) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.select_toc(node_id, window, cx);
        });
    });
    visual.run_until_parked();
}

fn assert_toc_selection(
    fixture: &EditorFixture,
    visual: &VisualTestContext,
    node_id: &str,
    unit_id: &str,
    unit_index: usize,
    body: &str,
) {
    fixture.editor.read_with(visual, |editor, cx| {
        assert_eq!(editor.selected_toc_id.as_deref(), Some(node_id));
        assert_eq!(editor.current_toc_id().as_deref(), Some(node_id));
        assert_eq!(editor.selected, unit_index);
        assert_eq!(editor.unit_states[editor.selected].id, unit_id);
        assert!(editor.body_input.read(cx).value().contains(body));
    });
}

#[gpui::test]
fn toc_clicks_use_real_node_targets_and_nonoverlapping_rows(cx: &mut TestAppContext) {
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

#[gpui::test]
fn toc_siblings_in_one_unit_keep_independent_selection_and_unsaved_source(cx: &mut TestAppContext) {
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
                input.set_value("# 引言\n\n目录切换前尚未保存的修改。", window, cx);
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

#[gpui::test]
fn toc_navigation_resolves_stable_ids_after_linear_units_are_reordered(cx: &mut TestAppContext) {
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

#[gpui::test]
fn saving_an_edited_unit_preserves_all_293_distinct_toc_entries(cx: &mut TestAppContext) {
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
                input.set_value("# 引言\n\n保存正文时必须保留独立目录标签。", window, cx);
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

fn open_unchanged_media_page(cx: &mut TestAppContext) -> (EditorFixture, &mut VisualTestContext) {
    let (fixture, visual) = open_editor_with_prepared_assets(cx, false, |document| {
        prepare_toc_document(document);
        let bytes = Arc::new(b"unchanged-image-fixture".to_vec());
        let image =
            AssetRef::from_bytes(AssetRole::ContentImage, "image/png", None, bytes.as_slice());
        let unit = &mut document.units[3];
        unit.source_kind = SourceKind::Html;
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
        unit.source = serialize_source(&unit.document, unit.source_kind).unwrap();
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

fn deliver_unchanged_rich_snapshot(
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

#[gpui::test]
fn unchanged_rich_snapshot_preserves_typed_media_and_all_draft_state(cx: &mut TestAppContext) {
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
        assert_eq!(editor.unit_states[3].source_kind, SourceKind::Html);
        assert_eq!(editor.unit_states[3].source, source);
        assert_eq!(editor.chapters[3].html, html);
        assert_eq!(editor.ai_selected_text.as_deref(), Some("只浏览的引言正文"));
    });
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui::test]
fn unchanged_rich_ack_runs_only_the_matching_toc_action_and_saves_typed_media(
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
    assert_eq!(saved.units[3].source_kind, SourceKind::Html);
    assert_eq!(saved.units[3].document, fixture.original.units[3].document);
    assert_eq!(saved.units[3].source, fixture.original.units[3].source);
    assert_eq!(saved.assets, fixture.original.assets);
    assert_eq!(saved.toc, fixture.original.toc);
}

#[gpui::test]
fn rejected_rich_body_cannot_be_saved_by_an_unchanged_ack_and_can_be_corrected(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_unchanged_media_page(cx);
    let depth = moye_epub_editor::document::MAX_DOCUMENT_DEPTH + 8;
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
