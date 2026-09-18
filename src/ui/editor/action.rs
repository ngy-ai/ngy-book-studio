//! The queued action pipeline: begin, perform and the export follow-ups.

use super::*;

impl EditorApp {
    pub(crate) fn begin_action(
        &mut self,
        action: PendingEditorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(operation) = self.active_write.as_ref() {
            if action == PendingEditorAction::Close {
                self.close_after_write = true;
                self.notice = Some(Notice {
                    text: "正在后台保存；保存成功后会安全关闭编辑器。".to_string(),
                    error: false,
                });
            } else {
                if action == PendingEditorAction::Export {
                    self.pending_export_path = None;
                }
                let operation_name = match operation.intent {
                    EditorWriteIntent::Save => "保存",
                    EditorWriteIntent::Export => "导出",
                    EditorWriteIntent::Close => "关闭保存",
                };
                self.notice = Some(Notice {
                    text: format!("正在后台{operation_name}，请等待当前操作完成。"),
                    error: false,
                });
            }
            cx.notify();
            return;
        }
        if self.media_modal.is_some()
            && matches!(
                action,
                PendingEditorAction::Save
                    | PendingEditorAction::Export
                    | PendingEditorAction::Close
            )
        {
            self.pending_media_action = Some(match self.pending_media_action {
                Some(pending) => {
                    merge_pending_editor_action(pending, action, &mut self.pending_export_path)
                }
                None => action,
            });
            self.notice = Some(Notice {
                text: "请先保存或取消媒体信息，随后会继续当前操作。".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        if self.media_loading
            && matches!(
                action,
                PendingEditorAction::Save
                    | PendingEditorAction::Export
                    | PendingEditorAction::Close
            )
        {
            self.pending_media_action = Some(match self.pending_media_action {
                Some(pending) => {
                    merge_pending_editor_action(pending, action, &mut self.pending_export_path)
                }
                None => action,
            });
            self.notice = Some(Notice {
                text: "正在读取媒体文件，完成后会继续操作。".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        if self.cover_loading
            && matches!(
                action,
                PendingEditorAction::Save
                    | PendingEditorAction::Export
                    | PendingEditorAction::Close
            )
        {
            self.pending_cover_action = Some(match self.pending_cover_action {
                Some(pending) => {
                    merge_pending_editor_action(pending, action, &mut self.pending_export_path)
                }
                None => action,
            });
            self.notice = Some(Notice {
                text: "正在读取封面图片，完成后会继续操作…".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        if let Some(pending) = self.pending_snapshot.as_mut() {
            pending.action =
                merge_pending_editor_action(pending.action, action, &mut self.pending_export_path);
            return;
        }
        if let Some(pending) = self.pending_ready_action.as_mut() {
            *pending = merge_pending_editor_action(*pending, action, &mut self.pending_export_path);
            return;
        }
        if action != PendingEditorAction::Export {
            self.pending_export_path = None;
        }
        if self.tab == EditorTab::RichText
            && let Some(active) = self.active_web_page.clone()
        {
            if !active.ready {
                self.pending_ready_action = Some(action);
                self.notice = Some(Notice {
                    text: "富文本编辑器正在加载，请稍候…".to_string(),
                    error: false,
                });
                cx.notify();
                return;
            }
            let Some(webview) = self.editor_webview.as_ref() else {
                self.perform_action(action, window, cx);
                return;
            };
            self.snapshot_request = if self.snapshot_request >= MAX_EDITOR_REQUEST_ID {
                1
            } else {
                self.snapshot_request + 1
            };
            let request_id = self.snapshot_request;
            self.pending_snapshot = Some(PendingEditorSnapshot {
                session_id: active.session_id,
                chapter_id: active.chapter_id,
                href: active.href,
                revision: active.revision,
                request_id,
                action,
            });
            let script = format!("window.__ngyEditorSend && window.__ngyEditorSend({request_id});");
            if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
                self.pending_snapshot = None;
                self.pending_export_path = None;
                if action == PendingEditorAction::Close {
                    cancel_application_exit(cx);
                }
                if action == PendingEditorAction::AskAi {
                    self.fail_pending_ai(format!("无法冻结当前章节供 AI 引用：{error}"), cx);
                }
                self.notice = Some(Notice {
                    text: format!("无法同步富文本内容：{error}"),
                    error: true,
                });
                cx.notify();
            } else {
                // Expose the pending sync/save/export state immediately instead
                // of waiting for the WebView's snapshot acknowledgement.
                cx.notify();
            }
            return;
        }
        self.perform_action(action, window, cx);
    }

    pub(crate) fn perform_action(
        &mut self,
        action: PendingEditorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action != PendingEditorAction::SelectToc {
            self.pending_toc_id = None;
        }
        match action {
            PendingEditorAction::SwitchTab(tab) => self.perform_switch_tab(tab, window, cx),
            PendingEditorAction::SelectChapter(index) => {
                self.perform_select_chapter(index, window, cx)
            }
            PendingEditorAction::SelectToc => self.perform_select_toc(window, cx),
            PendingEditorAction::AddChapter => self.perform_add_chapter(window, cx),
            PendingEditorAction::RemoveChapter => self.perform_remove_chapter(window, cx),
            PendingEditorAction::MoveChapterUp => self.perform_move_chapter(-1, window, cx),
            PendingEditorAction::MoveChapterDown => self.perform_move_chapter(1, window, cx),
            PendingEditorAction::IndentToc => self.perform_indent_toc(cx),
            PendingEditorAction::OutdentToc => self.perform_outdent_toc(cx),
            PendingEditorAction::CycleUnitKind => self.perform_cycle_unit_kind(cx),
            PendingEditorAction::InsertMedia(kind) => {
                self.choose_media_file(kind, None, window, cx)
            }
            PendingEditorAction::ReplaceMedia => {
                if let Some(target) = self.media_target.take() {
                    self.choose_media_file(target.kind, Some(target), window, cx);
                }
            }
            PendingEditorAction::EditMediaMetadata => {
                if let Some(target) = self.media_target.take() {
                    self.open_media_metadata_editor(target, window, cx);
                }
            }
            PendingEditorAction::DeleteMedia => self.perform_delete_media(window, cx),
            PendingEditorAction::Save => {
                self.perform_save_draft(window, cx);
            }
            PendingEditorAction::Export => self.perform_export(window, cx),
            PendingEditorAction::AskAi => self.perform_ai_question(cx),
            PendingEditorAction::OpenCitation(index) => {
                self.perform_open_citation(index, window, cx)
            }
            PendingEditorAction::Close => {
                self.start_editor_write(EditorWriteIntent::Close, None, window, cx);
            }
        }
    }

    pub(crate) fn switch_tab(
        &mut self,
        tab: EditorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending_tab = self
            .pending_snapshot
            .as_ref()
            .map(|pending| pending.action)
            .or(self.pending_ready_action)
            .and_then(|action| match action {
                PendingEditorAction::SwitchTab(tab) => Some(tab),
                _ => None,
            });
        if self.tab != tab || pending_tab.is_some_and(|pending| pending != tab) {
            self.begin_action(PendingEditorAction::SwitchTab(tab), window, cx);
        }
    }

    pub(crate) fn perform_switch_tab(
        &mut self,
        tab: EditorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab == tab {
            return;
        }
        self.ready_timeout_task.take();
        if self.tab == EditorTab::Source {
            if !self.flush_body(cx) {
                return;
            }
        }
        if tab == EditorTab::RichText
            && self
                .chapters
                .get(self.selected)
                .is_some_and(|chapter| chapter.html.len() > MAX_EDITOR_BODY_BYTES)
        {
            self.notice = Some(Notice {
                text: "本章超过 8 MiB，无法进入富文本模式；仍可使用源码和预览".to_string(),
                error: true,
            });
            cx.notify();
            return;
        }
        self.ai_selected_text = None;
        self.tab = tab;
        match tab {
            EditorTab::Source => {
                // Removing a native child HWND from the GPUI element tree does
                // not hide it; left visible, it keeps covering the source view.
                self.active_web_page = None;
                if let Some(webview) = &self.editor_webview {
                    webview.update(cx, |webview, _| webview.hide());
                }
                self.sync_body_input(self.selected, window, cx);
            }
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.notice = None;
        cx.notify();
    }
}
