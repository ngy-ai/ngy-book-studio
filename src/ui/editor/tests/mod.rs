//! Shared fixtures for the editor tests.

mod document_test;

mod ipc_test;

mod media_test;

mod model_test;

mod protocol_test;

mod ui_test;

mod write_test;

use super::*;
use ngy_book_studio::document::{BlockDocument, ContentUnit};
use tempfile::tempdir;

#[test]
fn pane_widths_respect_limits_and_preserve_the_editor_center() {
    assert_eq!(
        constrained_editor_pane_width(
            px(100.),
            px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
            px(1400.),
            px(AI_SIDEBAR_WIDTH),
        ),
        px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH)
    );
    assert_eq!(
        constrained_editor_pane_width(
            px(900.),
            px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
            px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
            px(1400.),
            px(AI_SIDEBAR_WIDTH),
        ),
        px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH)
    );

    let viewport = px(1100.);
    let left = constrained_editor_pane_width(
        px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
        px(EDITOR_LEFT_SIDEBAR_MIN_WIDTH),
        px(EDITOR_LEFT_SIDEBAR_MAX_WIDTH),
        viewport,
        px(AI_SIDEBAR_MAX_WIDTH),
    );
    let right = constrained_editor_pane_width(
        px(AI_SIDEBAR_MAX_WIDTH),
        px(AI_SIDEBAR_MIN_WIDTH),
        px(AI_SIDEBAR_MAX_WIDTH),
        viewport,
        left,
    );
    assert!(
        viewport - left - right - px(EDITOR_RESIZE_HANDLE_WIDTH * 2.)
            >= px(EDITOR_CONTENT_MIN_WIDTH)
    );
}

#[test]
fn resize_handle_center_preserves_each_sidebar_width() {
    let half_handle = px(EDITOR_RESIZE_HANDLE_WIDTH / 2.);
    let viewport = px(1400.);
    let left = px(EDITOR_LEFT_SIDEBAR_DEFAULT_WIDTH);
    let right = px(AI_SIDEBAR_WIDTH);

    assert_eq!(
        editor_pane_width_from_pointer(EditorResizablePane::Left, left + half_handle, viewport,),
        left
    );
    assert_eq!(
        editor_pane_width_from_pointer(
            EditorResizablePane::Right,
            viewport - right - half_handle,
            viewport,
        ),
        right
    );
}

fn close_test_data_dir(data_dir: tempfile::TempDir) {
    #[cfg(target_os = "windows")]
    {
        // SQLite/object-store worker handles can take one scheduler turn
        // to disappear after a runtime joins while the full parallel test
        // suite is active. Keep ownership of the exact tempfile path and
        // require it to become removable instead of silently leaking it.
        let path = data_dir.keep();
        assert!(path.starts_with(std::env::temp_dir()));
        let mut last_error = None;
        for _ in 0..100 {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(error) => last_error = Some(error),
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!(
            "temporary editor data directory remained locked: {}: {}",
            path.display(),
            last_error.expect("at least one remove attempt")
        );
    }
    #[cfg(not(target_os = "windows"))]
    data_dir.close().unwrap();
}

fn document_with_audio(book_id: &str, asset: AssetRef) -> BookDocument {
    let source = format!(
        "<audio controls=\"controls\" src=\"ngy-asset:{}\"></audio>",
        asset.id
    );
    let mut document = BookDocument::created(book_id, "媒体测试");
    document.units.push(ContentUnit::new(
        "unit-1",
        ContentUnitKind::Chapter,
        "第一章",
        source,
        BlockDocument::new(vec![Block::Audio {
            id: "block-audio".to_string(),
            asset_id: asset.id.clone(),
            title: None,
            caption: Vec::new(),
        }]),
    ));
    document.assets.push(asset);
    document
}

fn protocol_request(uri: impl AsRef<str>, range: Option<&str>) -> wry::http::Request<()> {
    let mut builder = wry::http::Request::builder().uri(uri.as_ref());
    if let Some(range) = range {
        builder = builder.header("Range", range);
    }
    builder.body(()).unwrap()
}

fn editor_ipc_identity(state: &EditorWebState, revision: u64) -> (String, String) {
    (
        state.session_id().to_string(),
        state.page(revision).unwrap().chapter_id,
    )
}

/// 共享锁中毒之后，读取与写入照旧给出最后一致的状态。
///
/// 持锁线程 panic 会毒化 `pages`/`media`/`protocol_open`；此后每一次
/// `lock().unwrap()` 都会 abort 进程，而 GPUI 回调不能 unwind —— 一枚毒化的锁足以
/// 让整个应用在状态本身完好无损时陪葬（2026-10-18 修：改走 `lock_shared` 恢复）。
#[test]
fn a_poisoned_editor_shared_lock_still_serves_its_last_state() {
    let state = EditorWebState::new(
        "book-1".to_string(),
        "chapter.xhtml".to_string(),
        "<p>正文</p>".to_string(),
    );
    let pages = Arc::clone(&state.pages);
    let poisoned = std::thread::spawn(move || {
        let _guard = pages.lock().unwrap();
        panic!("poison the shared editor lock on purpose");
    })
    .join();
    assert!(poisoned.is_err(), "夹具线程必须真的 panic 过");

    assert!(state.page(0).is_some(), "锁中毒后页面读取仍要成功");
    let _ = state.media_snapshot();
    assert!(state.protocol_is_open());
    state.close_protocol();
    assert!(!state.protocol_is_open());
    // 中毒不阻塞后续写入。
    state.set(
        1,
        "chapter-2".to_string(),
        "c2.xhtml".to_string(),
        "<p>其二</p>".to_string(),
    );
    assert!(state.page(1).is_some());
}
