//! Exercise production editor navigation and close confirmation with a
//! temporary library. GPUI's test platform answers the prompt; no native
//! WebView is built.
//! The fixture suppresses only the final native window-removal callback because
//! GPUI test windows have no HWND. These tests verify confirmation, real writes,
//! and the transition into closing with its protocol released; native window
//! destruction and WebView teardown still require Windows GUI acceptance.

use super::*;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};
use ngy_book_studio::document::{
    BlockDocument, ContentUnit as DocumentUnit, SourceLocator, TocTarget,
};
use std::time::{Duration, Instant};

pub(crate) struct EditorFixture {
    pub(crate) editor: Entity<EditorApp>,
    pub(crate) _library_view: Entity<EpubReaderApp>,
    pub(crate) services: Arc<AppServices>,
    pub(crate) original: BookDocument,
    pub(crate) _directory: tempfile::TempDir,
}

pub(crate) fn open_editor(
    cx: &mut TestAppContext,
    webview_building: bool,
) -> (EditorFixture, &mut VisualTestContext) {
    open_editor_with_document(cx, webview_building, None)
}

pub(crate) fn open_editor_with_document(
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

pub(crate) fn open_editor_with_prepared_assets(
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
    init_ui_test(cx);
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
        let body_input = cx.new(|cx| TextareaState::new(window, cx));
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

pub(crate) fn persisted_document(fixture: &EditorFixture) -> BookDocument {
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

pub(crate) fn edit_draft(fixture: &EditorFixture, visual: &mut VisualTestContext, title: &str) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.title_input.update(cx, |input, cx| {
                input.set_value(title.to_string(), window, cx);
            });
            editor.body_input.update(cx, |input, cx| {
                input.set_value("<h1>第一章</h1><p>尚未保存的正文。</p>", window, cx);
            });
        });
    });
    visual.run_until_parked();
}

pub(crate) fn request_close(fixture: &EditorFixture, visual: &mut VisualTestContext) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            assert!(!editor.handle_window_close(window, cx));
        });
    });
    visual.run_until_parked();
}

pub(crate) fn arm_application_exit(visual: &mut VisualTestContext) {
    visual.update(|window, cx| {
        init_application_window_lifecycle(cx);
        let handle = Window::window_handle(window);
        // Isolate the real editor's veto notification from native teardown.
        // The coordinator is waiting for this editor to finish its close.
        cx.global_mut::<ApplicationWindowLifecycle>().exit = Some(ApplicationExit {
            main_window: handle,
            close_main: std::rc::Rc::new(|_, _| {}),
            waiting_for: Some(handle),
            main_close_requested: false,
            main_ready: false,
            removal_scheduled: false,
        });
    });
}

pub(crate) fn assert_application_exit_cancelled(visual: &mut VisualTestContext) {
    visual.update(|_, cx| {
        assert!(cx.global::<ApplicationWindowLifecycle>().exit.is_none());
    });
}

pub(crate) fn assert_prompt_open(fixture: &EditorFixture, visual: &VisualTestContext) {
    assert!(visual.has_pending_prompt());
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.close_prompt_open);
        assert!(!editor.closing);
        assert!(editor.active_write.is_none());
    });
}

pub(crate) fn assert_ready_for_removal(fixture: &EditorFixture, visual: &VisualTestContext) {
    fixture.editor.read_with(visual, |editor, _| {
        assert!(editor.closing);
        assert!(editor.active_write.is_none());
        assert!(!editor.web_state.protocol_is_open());
        assert!(editor.pending_snapshot.is_none());
        assert!(editor.pending_ready_action.is_none());
        assert!(editor.ipc_sync_task.is_none());
    });
}

pub(crate) fn wait_for_write(fixture: &EditorFixture, visual: &mut VisualTestContext) {
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

mod close_test;

mod rich_test;

mod toc;

mod toc_test;

pub(crate) use toc_test::*;

pub(crate) use toc::*;
