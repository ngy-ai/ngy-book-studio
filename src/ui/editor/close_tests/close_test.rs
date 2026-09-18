//! Tests for the editor close handshake.

use super::*;

#[gpui::test]
pub(crate) fn close_requires_an_answer_and_cancel_preserves_the_draft(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "取消后保留的书名");
    arm_application_exit(visual);
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    assert_eq!(persisted_document(&fixture), fixture.original);

    // Repeated close messages must leave one pending question and no write.
    request_close(&fixture, visual);
    assert_prompt_open(&fixture, visual);
    visual.simulate_prompt_answer("取消");
    visual.run_until_parked();
    assert_application_exit_cancelled(visual);
    assert!(!visual.has_pending_prompt());
    fixture.editor.read_with(visual, |editor, cx| {
        assert!(!editor.close_prompt_open);
        assert!(!editor.closing);
        assert!(editor.active_write.is_none());
        assert_eq!(editor.title_input.read(cx).value(), "取消后保留的书名");
        assert_eq!(
            editor.body_input.read(cx).value(),
            "<h1>第一章</h1><p>尚未保存的正文。</p>"
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
pub(crate) fn close_without_saving_keeps_the_persisted_document(cx: &mut TestAppContext) {
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
pub(crate) fn export_picker_cancels_application_exit_and_preserves_the_editor(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "导出选择器打开时的草稿");
    arm_application_exit(visual);
    visual.update(|_, cx| {
        fixture.editor.update(cx, |editor, _| {
            editor.export_dialog_open = true;
        });
    });
    request_close(&fixture, visual);
    assert_application_exit_cancelled(visual);
    assert!(!visual.has_pending_prompt());
    fixture.editor.read_with(visual, |editor, cx| {
        assert!(!editor.closing);
        assert!(editor.active_write.is_none());
        assert_eq!(
            editor.title_input.read(cx).value(),
            "导出选择器打开时的草稿"
        );
        assert!(
            editor
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("导出文件选择器")
        );
    });
    assert_eq!(persisted_document(&fixture), fixture.original);
}

#[gpui::test]
pub(crate) fn save_and_close_waits_for_the_confirmed_background_commit(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "确认后保存的书名");
    arm_application_exit(visual);
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
    visual.update(|_, cx| {
        assert!(cx.global::<ApplicationWindowLifecycle>().exit.is_some());
    });
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
pub(crate) fn rejected_save_keeps_the_window_open_and_can_be_corrected(cx: &mut TestAppContext) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "");
    arm_application_exit(visual);
    request_close(&fixture, visual);
    visual.simulate_prompt_answer("保存并关闭");
    visual.run_until_parked();
    assert_application_exit_cancelled(visual);
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
pub(crate) fn background_save_failure_keeps_the_draft_and_reopens_confirmation(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_editor(cx, false);
    edit_draft(&fixture, visual, "本窗口未保存的书名");
    arm_application_exit(visual);
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
    assert_application_exit_cancelled(visual);
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
pub(crate) fn close_during_an_explicit_save_waits_and_then_asks(cx: &mut TestAppContext) {
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
pub(crate) fn discard_during_a_previously_requested_write_waits_without_another_save(
    cx: &mut TestAppContext,
) {
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
pub(crate) fn failed_webview_build_resumes_close_with_a_question(cx: &mut TestAppContext) {
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
pub(crate) fn removing_the_book_closes_without_saving_or_asking(cx: &mut TestAppContext) {
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
pub(crate) fn removing_the_book_during_webview_build_discards_the_draft(cx: &mut TestAppContext) {
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
