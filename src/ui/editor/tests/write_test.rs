//! Tests for the write job, the build gate and export replacement.

use super::*;

#[test]
fn editor_write_uses_background_runtime_and_stale_failure_does_not_overwrite() {
    let data_dir = tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let book_id = runtime.block_on(async {
        services
            .spawn_library(|library| library.create_book("初始书名", "作者"))
            .await
            .unwrap()
            .unwrap()
            .id
    });
    let stale = runtime.block_on(async {
        let lookup = book_id.clone();
        services
            .spawn_library_read(move |library| library.document(&lookup))
            .await
            .unwrap()
            .unwrap()
    });

    let mut winner = stale.clone();
    winner.title = "后台保存成功".to_string();
    let caller_thread = std::thread::current().id();
    let saved = runtime.block_on(async {
        spawn_editor_write(
            &services,
            EditorWriteJob {
                document: winner,
                new_asset_bytes: HashMap::new(),
                export_target: None,
            },
        )
        .await
        .unwrap()
        .unwrap()
    });
    assert_ne!(caller_thread, saved.value.worker_thread_id);
    assert_eq!(saved.value.document.title, "后台保存成功");
    assert_eq!(
        saved
            .snapshot
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .unwrap()
            .title,
        "后台保存成功"
    );

    let export_target = data_dir.path().join("background-export.epub");
    let mut export_document = saved.value.document.clone();
    export_document.title = "后台保存并导出".to_string();
    let exported = runtime.block_on(async {
        spawn_editor_write(
            &services,
            EditorWriteJob {
                document: export_document,
                new_asset_bytes: HashMap::new(),
                export_target: Some(export_target.clone()),
            },
        )
        .await
        .unwrap()
        .unwrap()
    });
    assert_ne!(caller_thread, exported.value.worker_thread_id);
    assert!(exported.generation > saved.generation);
    assert!(matches!(
        exported.value.export,
        EditorExportOutcome::Exported(_)
    ));
    assert!(std::fs::metadata(&export_target).unwrap().len() > 0);

    let mut loser = stale;
    loser.title = "过期窗口不应覆盖".to_string();
    let stale_result = runtime.block_on(async {
        spawn_editor_write(
            &services,
            EditorWriteJob {
                document: loser,
                new_asset_bytes: HashMap::new(),
                export_target: None,
            },
        )
        .await
        .unwrap()
    });
    assert!(stale_result.is_err());

    let persisted = runtime.block_on(async {
        let lookup = book_id.clone();
        services
            .spawn_library_read(move |library| library.document(&lookup))
            .await
            .unwrap()
            .unwrap()
    });
    assert_eq!(persisted.title, "后台保存并导出");
    assert_eq!(Arc::strong_count(&services), 1);
    drop(services);
    drop(runtime);
    close_test_data_dir(data_dir);
}

#[test]
fn editor_projection_guard_preserves_a_newer_group_assignment() {
    let data_dir = tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let created = runtime.block_on(async {
        services
            .spawn_library_projected(|library| library.create_book("初始书名", "作者"))
            .await
            .unwrap()
            .unwrap()
    });
    let book_id = created.value.id;
    let group = runtime.block_on(async {
        services
            .spawn_library_projected(|library| library.create_group("稍后分组", None))
            .await
            .unwrap()
            .unwrap()
    });
    let group_id = group.value.id;
    let mut editor_document = runtime.block_on(async {
        let lookup = book_id.clone();
        services
            .spawn_library_read(move |library| library.document(&lookup))
            .await
            .unwrap()
            .unwrap()
    });
    editor_document.title = "编辑器保存".to_string();
    let editor = runtime.block_on(async {
        spawn_editor_write(
            &services,
            EditorWriteJob {
                document: editor_document,
                new_asset_bytes: HashMap::new(),
                export_target: None,
            },
        )
        .await
        .unwrap()
        .unwrap()
    });
    let moved = runtime.block_on(async {
        let moved_book_id = book_id.clone();
        let moved_group_id = group_id.clone();
        services
            .spawn_library_projected(move |library| {
                library.set_book_group(&moved_book_id, Some(&moved_group_id))
            })
            .await
            .unwrap()
            .unwrap()
    });
    assert!(moved.generation > editor.generation);

    let mut local = group.snapshot;
    let mut applied_generation = group.generation;
    assert!(apply_editor_library_projection(
        &mut local,
        &mut applied_generation,
        moved.generation,
        moved.snapshot,
    ));
    assert_eq!(
        local
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .unwrap()
            .group_id
            .as_deref(),
        Some(group_id.as_str())
    );
    assert!(!apply_editor_library_projection(
        &mut local,
        &mut applied_generation,
        editor.generation,
        editor.snapshot,
    ));
    assert_eq!(
        local
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .unwrap()
            .group_id
            .as_deref(),
        Some(group_id.as_str())
    );

    drop(services);
    drop(runtime);
    close_test_data_dir(data_dir);
}

#[test]
fn replacing_a_pending_export_discards_its_target_path() {
    let mut export_path = Some(PathBuf::from("first.epub"));
    assert_eq!(
        merge_pending_editor_action(
            PendingEditorAction::Save,
            PendingEditorAction::Export,
            &mut export_path,
        ),
        PendingEditorAction::Export
    );
    assert_eq!(
        export_path.as_deref(),
        Some(std::path::Path::new("first.epub"))
    );

    assert_eq!(
        merge_pending_editor_action(
            PendingEditorAction::Export,
            PendingEditorAction::SwitchTab(EditorTab::Preview),
            &mut export_path,
        ),
        PendingEditorAction::SwitchTab(EditorTab::Preview)
    );
    assert!(export_path.is_none());
}

#[test]
fn webview_build_gate_defers_close_until_the_builder_settles() {
    let mut gate = EditorWebViewBuildGate::new(true);
    assert!(gate.request_close());
    assert!(gate.request_close());
    assert!(gate.finish());
    assert!(!gate.finish());
    assert!(!gate.request_close());
}

#[test]
fn ready_timeout_only_matches_the_current_unready_page() {
    let mut page = ActiveEditorPage {
        session_id: "session-a".to_string(),
        chapter_id: "unit-a".to_string(),
        href: "/EPUB/ch.xhtml".to_string(),
        revision: 7,
        ready: false,
    };
    assert!(page.is_unready_match("session-a", "unit-a", 7, "EPUB/ch.xhtml"));
    assert!(!page.is_unready_match("session-b", "unit-a", 7, "EPUB/ch.xhtml"));
    assert!(!page.is_unready_match("session-a", "unit-b", 7, "EPUB/ch.xhtml"));
    assert!(!page.is_unready_match("session-a", "unit-a", 8, "EPUB/ch.xhtml"));
    assert!(!page.is_unready_match("session-a", "unit-a", 7, "EPUB/other.xhtml"));
    page.ready = true;
    assert!(!page.is_unready_match("session-a", "unit-a", 7, "EPUB/ch.xhtml"));
}
