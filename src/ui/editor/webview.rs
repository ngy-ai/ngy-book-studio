//! WebView lifecycle: attaching, appearance, IPC intake and the ready
//! handshake.

use super::*;

impl EditorApp {
    pub(crate) fn fail_webview_build(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let close_after_build = self.webview_build_gate.finish();
        self.notice = Some(Notice {
            text: format!("无法创建预览视图：{error}"),
            error: true,
        });
        if close_after_build {
            self.resume_deferred_close(window, cx);
            return;
        }
        cx.notify();
    }

    pub(crate) fn apply_web_event(
        &mut self,
        event: EditorWebEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorWebEvent::Ipc(message) => self.apply_ipc_message(message, window, cx),
            EditorWebEvent::PageLoaded(url) => {
                self.apply_webview_appearance(cx);
                self.run_pending_citation_navigation(&url, cx);
            }
            EditorWebEvent::CitationNavigationResult {
                session_id,
                chapter_id,
                href,
                revision,
                request_id,
                found,
                reason,
            } => self.finish_citation_navigation(
                &session_id,
                &chapter_id,
                &href,
                revision,
                request_id,
                found,
                &reason,
                cx,
            ),
        }
    }

    pub(crate) fn run_pending_citation_navigation(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(document) = url
            .parse()
            .ok()
            .and_then(|uri| editor_document_request(&uri))
        else {
            return;
        };
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if document.kind != EditorDocumentKind::Preview
            || pending.session_id != document.session_id
            || pending.chapter_id != document.chapter_id
            || pending.revision != document.revision
            || normalized_editor_path(&pending.href).as_deref() != Some(document.href.as_str())
        {
            return;
        }
        let Some(webview) = self.editor_webview.as_ref() else {
            self.pending_citation_navigation = None;
            self.set_error("预览视图尚未就绪，无法定位引用", cx);
            return;
        };
        let script = match citation_dom_navigation_script(
            &pending.focus,
            serde_json::json!({
                "type": "citation_navigation_result",
                "session_id": pending.session_id,
                "chapter_id": pending.chapter_id,
                "href": pending.href,
                "revision": pending.revision,
                "request_id": pending.request_id,
            }),
        ) {
            Ok(script) => script,
            Err(error) => {
                self.pending_citation_navigation = None;
                self.set_error(error, cx);
                return;
            }
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.pending_citation_navigation = None;
            self.set_error(format!("无法执行引用定位：{error}"), cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_citation_navigation(
        &mut self,
        session_id: &str,
        chapter_id: &str,
        href: &str,
        revision: u64,
        request_id: u64,
        found: bool,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_citation_navigation.as_ref() else {
            return;
        };
        if pending.session_id != session_id
            || pending.chapter_id != chapter_id
            || !editor_hrefs_match(&pending.href, href)
            || pending.revision != revision
            || pending.request_id != request_id
        {
            return;
        }
        self.pending_citation_navigation = None;
        if found {
            self.notice = None;
            cx.notify();
        } else {
            let detail = match reason {
                "ambiguous" => "预览中存在多个相同片段",
                "not_found" | "missing_root" | "empty_text" => "预览中找不到该片段",
                _ => "预览无法映射该文字范围",
            };
            self.set_error(
                format!("引用定位失败：{detail}；未跳转到章节中的其它位置。"),
                cx,
            );
        }
    }

    /// Applies only snapshots from the currently authoritative rich-text page.
    /// Explicit snapshots carry a request id; the queued action runs only after
    /// that exact acknowledgement has updated the chapter.
    pub(crate) fn apply_ipc_message(
        &mut self,
        message: EditorIpcUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let Some(active) = self.active_web_page.as_ref() else {
            return;
        };
        if self.tab != EditorTab::RichText
            || message.session_id != active.session_id
            || message.chapter_id != active.chapter_id
            || message.revision != active.revision
            || !editor_hrefs_match(&message.href, &active.href)
        {
            return;
        }

        // The session/chapter/revision identity above binds this transient
        // selection to the active page. An Ask-AI action consumes the selection
        // carried by its exact request-id acknowledgement below.
        self.ai_selected_text = message.selected_text.clone();

        if message.ready {
            self.ready_timeout_task.take();
            if let Some(active) = self.active_web_page.as_mut() {
                active.ready = true;
            }
            if let Some(action) = self.pending_ready_action.take() {
                self.begin_action(action, window, cx);
            } else {
                cx.notify();
            }
            return;
        }

        let acknowledges_pending = self.pending_snapshot.as_ref().is_some_and(|pending| {
            message.request_id == Some(pending.request_id)
                && message.session_id == pending.session_id
                && message.chapter_id == pending.chapter_id
                && message.revision == pending.revision
                && editor_hrefs_match(&message.href, &pending.href)
        });
        if message.too_large {
            if acknowledges_pending {
                let action = self.pending_snapshot.take().map(|pending| pending.action);
                self.pending_export_path = None;
                if action == Some(PendingEditorAction::Close) {
                    cancel_application_exit(cx);
                }
                if action == Some(PendingEditorAction::AskAi) {
                    self.fail_pending_ai("所选章节过大，无法安全冻结为 AI 引用。", cx);
                }
            }
            self.notice = Some(Notice {
                text: "富文本正文超过 8 MiB，请先在富文本中精简内容后再保存或切换".to_string(),
                error: true,
            });
            cx.notify();
            return;
        }
        let Some(chapter_index) = self
            .chapters
            .iter()
            .position(|chapter| editor_hrefs_match(&chapter.href, &message.href))
        else {
            return;
        };
        if message.edited || self.chapters[chapter_index].html != message.html {
            if let Err(error) = self.update_unit_from_rich_text(chapter_index, &message.html) {
                self.notice = Some(Notice {
                    text: format!("无法解析富文本编辑结果：{error:#}"),
                    error: true,
                });
                if self
                    .pending_snapshot
                    .take()
                    .is_some_and(|pending| pending.action == PendingEditorAction::Close)
                {
                    cancel_application_exit(cx);
                }
                self.pending_export_path = None;
                cx.notify();
                return;
            }
            if self.chapters[chapter_index].html != message.html {
                self.draft_generation = self.draft_generation.wrapping_add(1);
            }
            self.chapters[chapter_index].html = message.html.clone();
            if let Some(state) = self.unit_states.get(chapter_index) {
                self.modified_chapter_ids.insert(state.id.clone());
            }
        }
        // An unchanged acknowledgement carries the original display shell,
        // including its article wrapper. It is not a new editable snapshot:
        // reparsing it would collapse typed blocks and lose media references.
        // A previously rejected snapshot differs from the accepted chapter and
        // must still pass parsing, even if the web page now reports unchanged.
        // Only an accepted snapshot releases the exact pending action below.
        if !self.search_query.is_empty() {
            self.search_results = search_editor_chapters(&self.chapters, &self.search_query);
        }
        self.sync_ai_reference(cx);

        if acknowledges_pending {
            let action = self.pending_snapshot.take().unwrap().action;
            self.perform_action(action, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn handle_editor_ready_timeout(
        &mut self,
        session_id: &str,
        chapter_id: &str,
        revision: u64,
        href: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let still_waiting = self
            .active_web_page
            .as_ref()
            .is_some_and(|active| active.is_unready_match(session_id, chapter_id, revision, href));
        if self.tab != EditorTab::RichText || !still_waiting {
            return;
        }

        // A page that never sends Ready was never confirmed editable. Drop its
        // page identity instead of waiting forever for a bridge that does not
        // exist. Export must not fall back to potentially stale Rust state.
        self.active_web_page = None;
        let pending_action = self.pending_ready_action.take();
        if let Some(action) = pending_action {
            if action == PendingEditorAction::Export {
                self.pending_export_path = None;
            } else if action == PendingEditorAction::AskAi {
                self.fail_pending_ai("富文本编辑器尚未就绪，未发送可能过期的章节内容。", cx);
            } else {
                self.perform_action(action, window, cx);
                if action == PendingEditorAction::Close {
                    return;
                }
            }
        }
        self.notice = Some(Notice {
            text: "富文本编辑器初始化失败，已安全退回；请使用源码模式检查本章 XHTML".to_string(),
            error: true,
        });
        cx.notify();
    }
}
