//! The PDF reader side of the note flow.

use super::*;

impl PdfReaderApp {
    pub(crate) fn note_position_for(&self, page_number: u32) -> Option<PdfNotePosition> {
        pdf_note_position(&self.pages, page_number)
    }

    /// Whether this document can own notes at all. An ephemeral Office preview
    /// has no canonical page identity, so its bridge chrome stays hidden.
    pub(crate) fn notes_enabled(&self) -> bool {
        self.pages.iter().any(page_can_own_notes)
    }

    /// Pages a request applies to, restricted to the window the viewer has
    /// declared as rendered. An undeclared page is never served.
    pub(crate) fn requested_note_pages(&self, action: &PdfAnnotationAction) -> Vec<u32> {
        let requested = if action.pages.is_empty() {
            vec![action.page]
        } else {
            action.pages.clone()
        };
        let mut pages = requested
            .into_iter()
            .filter(|page| self.annotations.rendered_pages.contains(page))
            .collect::<Vec<_>>();
        pages.dedup();
        pages.truncate(MAX_ANNOTATION_PAGES);
        pages
    }

    pub(crate) fn note_script(
        &mut self,
        method: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(webview) = self.webview.as_ref() else {
            return;
        };
        let script = format!("window.ngyAnnotations?.{method}({value});");
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.set_error(format!("无法更新 PDF 笔记界面：{error}"), cx);
        }
    }

    /// Binds the page bridge to this document revision. The session stays valid
    /// while the same PDF revision is open, so scrolling never invalidates the
    /// notes of the pages that remain mounted; the viewer declares its mounted
    /// window separately and every page request is checked against it.
    pub(crate) fn configure_annotations(&mut self, cx: &mut Context<Self>) {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        if !self.notes_enabled() {
            // An ephemeral preview carries no canonical page identity, so it
            // cannot own notes. Retire the session before the page bridge hides
            // its controls: a late request from the document that just went away
            // must not reach a later one.
            self.annotations.session.clear();
            self.annotations.last_request = 0;
            self.annotations.dirty = false;
            self.annotations.rendered_pages.clear();
            self.note_script("disable", serde_json::json!({}), cx);
            return;
        }
        if self.annotations.session.is_empty() {
            self.annotations.session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
            self.annotations.last_request = 0;
            self.annotations.dirty = false;
            self.annotations.rendered_pages.clear();
        }
        let session = self.annotations.session.clone();
        let revision = self.annotations.document_revision;
        self.note_script(
            "configure",
            serde_json::json!({
                "session": session,
                "revision": revision,
                "notes_enabled": true,
            }),
            cx,
        );
    }

    pub(crate) fn note_result(
        &mut self,
        action: &PdfAnnotationAction,
        result: Result<Vec<serde_json::Value>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.annotations.session != action.session || self.closing {
            return;
        }
        let session = action.session.clone();
        let revision = self.annotations.document_revision;
        let request_id = action.request_id;
        let payload = match result {
            Ok(pages) => {
                if action.action != PdfNoteAction::List {
                    self.notice = None;
                    cx.notify();
                }
                serde_json::json!({"session":session,"revision":revision,"request_id":request_id,"ok":true,"pages":pages})
            }
            Err(error) => {
                self.set_error(error.clone(), cx);
                serde_json::json!({"session":session,"revision":revision,"request_id":request_id,"ok":false,"error":error})
            }
        };
        self.note_script("result", payload, cx);
    }

    /// Page changes and window close must not silently drop an unsaved thought.
    pub(crate) fn annotation_navigation_blocked(&mut self, cx: &mut Context<Self>) -> bool {
        let reason = if self.annotations.dirty {
            Some("人工想法尚未保存，请先保存或取消编辑。")
        } else if self.annotations.busy {
            Some("笔记正在保存，请完成后再切页或关闭。")
        } else if self
            .annotations
            .pending_ai
            .as_ref()
            .is_some_and(|pending| pending.draft.comment.is_some())
        {
            Some("AI 想法尚未保存，请在笔记面板重试保存或放弃。")
        } else if self.annotations.pending_ai.is_some() {
            Some("AI 想法正在生成，请完成或在 AI 侧栏取消后再切页。")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.set_error(reason.to_owned(), cx);
            true
        } else {
            false
        }
    }

    pub(crate) fn note_draft(
        &self,
        action: &PdfAnnotationAction,
        kind: AnnotationKind,
    ) -> Result<AnnotationDraft, String> {
        let position = self
            .note_position_for(action.page)
            .ok_or("当前页面没有可用的笔记位置")?;
        Ok(AnnotationDraft {
            content_unit_id: position.unit_id,
            document_revision: self.annotations.document_revision,
            unit_revision: action.revision,
            anchor: action.anchor.clone().ok_or("请重新选择页面文字")?,
            kind,
            comment: action.content.clone(),
        })
    }

    pub(crate) fn handle_annotation_action(
        &mut self,
        action: PdfAnnotationAction,
        cx: &mut Context<Self>,
    ) {
        // Reject anything outside this document session, plus replayed request
        // numbers. Which pages may author a note is decided per request.
        if self.closing
            || self.webview_build_gate.close_requested
            || action.session != self.annotations.session
            || action.request_id <= self.annotations.last_request
        {
            return;
        }
        self.annotations.last_request = action.request_id;
        if action.action == PdfNoteAction::DraftChanged {
            self.annotations.dirty = action.dirty;
            return;
        }
        if action.action == PdfNoteAction::PagesRendered {
            // The mounted reading window. A page outside it is never served, so
            // a request that outlives its page cannot reach another page.
            self.annotations.rendered_pages = action.pages.iter().copied().collect();
            return;
        }
        let pages = self.requested_note_pages(&action);
        if pages.is_empty() {
            self.note_result(&action, Err("笔记请求与已渲染的页面不一致。".into()), cx);
            return;
        }
        if action.action == PdfNoteAction::List {
            // Listing is the one action a page without canonical identity may
            // still answer: it reports `enabled: false` instead of notes.
            self.write_or_load_notes(action, None, pages, cx);
            return;
        }
        let Some(position) = self.note_position_for(action.page) else {
            self.note_result(&action, Err("当前页面没有可用的笔记位置。".into()), cx);
            return;
        };
        // A write must describe the anchor revision of the page it targets.
        if action.revision != position.unit_revision {
            self.note_result(&action, Err("笔记请求与页面版本不一致。".into()), cx);
            return;
        }
        if self.annotations.busy {
            self.note_result(&action, Err("笔记正在处理，请稍后再试。".into()), cx);
            return;
        }
        if action.action == PdfNoteAction::RetryAiSave {
            if let Some(pending) = self.annotations.pending_ai.as_mut()
                && pending.draft.comment.is_some()
            {
                pending.action = action.clone();
                let draft = pending.draft.clone();
                self.write_or_load_notes(action, Some(draft), pages, cx);
            } else {
                self.note_result(&action, Err("没有待保存的 AI 想法。".into()), cx);
            }
            return;
        }
        if action.action == PdfNoteAction::DiscardAiSave {
            if self
                .annotations
                .pending_ai
                .as_ref()
                .is_some_and(|pending| pending.draft.comment.is_some())
            {
                self.annotations.pending_ai = None;
                self.write_or_load_notes(action, None, pages, cx);
            } else {
                self.note_result(&action, Err("没有可放弃的待保存 AI 想法。".into()), cx);
            }
            return;
        }
        if self.annotations.pending_ai.is_some() {
            self.note_result(&action, Err("请先完成或取消当前 AI 想法。".into()), cx);
            return;
        }
        if action.action == PdfNoteAction::AiExplain {
            self.start_annotation_explanation(action, cx);
            return;
        }
        let kind = match action.action {
            PdfNoteAction::Highlight => Some(AnnotationKind::Highlight),
            PdfNoteAction::Wavy => Some(AnnotationKind::Wavy),
            PdfNoteAction::Underline => Some(AnnotationKind::Underline),
            PdfNoteAction::HumanComment => Some(AnnotationKind::HumanComment),
            _ => None,
        };
        let draft = match kind.map(|kind| self.note_draft(&action, kind)).transpose() {
            Ok(draft) => draft,
            Err(error) => {
                self.note_result(&action, Err(error), cx);
                return;
            }
        };
        self.write_or_load_notes(action, draft, pages, cx);
    }

    pub(crate) fn write_or_load_notes(
        &mut self,
        action: PdfAnnotationAction,
        draft: Option<AnnotationDraft>,
        pages: Vec<u32>,
        cx: &mut Context<Self>,
    ) {
        // `pages` already passed the rendered-window check, so every target is
        // a page the viewer currently shows. A page without canonical identity
        // still answers with `enabled: false` instead of notes, and a write only
        // ever targets the page this request was authored on.
        let targets = pages
            .into_iter()
            .map(|page| (page, self.note_position_for(page)))
            .collect::<Vec<_>>();
        let Some(unit_id) = targets
            .iter()
            .find(|(page, _)| *page == action.page)
            .and_then(|(_, position)| position.as_ref())
            .map(|position| position.unit_id.clone())
        else {
            self.note_result(&action, Err("当前页面没有可用的笔记位置。".into()), cx);
            return;
        };
        let book_id = self.book_id.clone();
        let document_revision = self.annotations.document_revision;
        let operation = action.clone();
        let is_ai_save = draft
            .as_ref()
            .is_some_and(|draft| draft.kind == AnnotationKind::AiComment);
        let pending_markdown = draft
            .as_ref()
            .filter(|_| is_ai_save)
            .and_then(|draft| draft.comment.clone());
        let display_runtime = self.services.runtime();
        self.annotations.busy = true;
        let task = self.services.spawn_library(move |library| {
            if let Some(draft) = draft {
                library.create_pdf_annotation(&book_id, &draft)?;
            } else {
                match operation.action {
                    PdfNoteAction::RemoveMark => library.delete_pdf_annotation_marks(
                        &book_id,
                        &unit_id,
                        document_revision,
                        operation.revision,
                        operation.anchor.as_ref().context("请重新选择页面文字")?,
                    )?,
                    PdfNoteAction::Update | PdfNoteAction::Delete => {
                        let id = operation.id.as_deref().context("笔记 ID 不能为空")?;
                        // A page cannot mutate another page's notes even within
                        // the same book.
                        ensure!(
                            library
                                .list_annotations(&book_id, Some(&unit_id))?
                                .iter()
                                .any(|note| note.id == id),
                            "笔记不属于当前页面"
                        );
                        if operation.action == PdfNoteAction::Delete {
                            library.delete_annotation(&book_id, id)?;
                        } else {
                            library.update_human_comment(
                                &book_id,
                                id,
                                operation.content.as_deref().context("想法不能为空")?,
                            )?;
                        }
                    }
                    PdfNoteAction::List
                    | PdfNoteAction::DiscardAiSave
                    | PdfNoteAction::PagesRendered
                    | PdfNoteAction::DraftChanged => {}
                    _ => anyhow::bail!("无效的笔记操作"),
                }
            }
            // The mutation above has committed. A later refresh failure must
            // not invite a duplicate insert on retry, so the listing stays a
            // separate inner result.
            let listing = (|| -> Result<Vec<PageAnnotations>, anyhow::Error> {
                let mut results = Vec::with_capacity(targets.len());
                for (page, position) in targets {
                    match position {
                        Some(position) => results.push(PageAnnotations {
                            page,
                            unit_revision: position.unit_revision,
                            enabled: true,
                            notes: library.list_annotations(&book_id, Some(&position.unit_id))?,
                        }),
                        None => results.push(PageAnnotations {
                            page,
                            unit_revision: 0,
                            enabled: false,
                            notes: Vec::new(),
                        }),
                    }
                }
                Ok(results)
            })();
            Ok::<_, anyhow::Error>(listing)
        });
        cx.spawn(async move |view, cx| {
            let (committed, result) = match task.await {
                Ok(Ok(result)) => (
                    action.action != PdfNoteAction::List,
                    result.map_err(|error| format!("无法刷新笔记：{error:#}")),
                ),
                Ok(Err(error)) => (false, Err(format!("笔记处理失败：{error:#}"))),
                Err(error) => (false, Err(format!("笔记任务已停止：{error}"))),
            };
            // Parse away from GPUI and after releasing the library mutation
            // lock. Even a large AI thought must not freeze reader input.
            let (result, failed_content_html) = match display_runtime
                .handle()
                .spawn_blocking(move || {
                    let failed_html = if result.is_err() {
                        pending_markdown.as_deref().map(thought_display_html)
                    } else {
                        None
                    };
                    (
                        result.map(|pages| {
                            pages.into_iter().map(page_annotations_json).collect::<Vec<_>>()
                        }),
                        failed_html,
                    )
                })
                .await
            {
                Ok(value) => value,
                Err(error) => (Err(format!("笔记展示任务已停止：{error}")), None),
            };
            let _ = view.update(cx, |this, cx| {
                this.annotations.busy = false;
                if committed && result.is_err() {
                    if is_ai_save {
                        this.annotations.pending_ai = None;
                    }
                    this.note_script("result", serde_json::json!({"session":action.session,"revision":document_revision,"request_id":action.request_id,"ok":true}), cx);
                    this.set_error("笔记已更新，但列表刷新失败；请重新打开笔记面板。".into(), cx);
                    return;
                }
                if is_ai_save {
                    if let Err(error) = &result {
                        let content = this
                            .annotations
                            .pending_ai
                            .as_ref()
                            .and_then(|pending| pending.draft.comment.as_deref())
                            .unwrap_or_default()
                            .to_owned();
                        this.note_script("result", serde_json::json!({"session":action.session,"revision":document_revision,"request_id":action.request_id,"ok":false,"error":error,"retry_ai_save":true,"content":content,"content_html":failed_content_html}), cx);
                        this.set_error(error.clone(), cx);
                        return;
                    }
                    this.annotations.pending_ai = None;
                }
                this.note_result(&action, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn start_annotation_explanation(
        &mut self,
        action: PdfAnnotationAction,
        cx: &mut Context<Self>,
    ) {
        if self.annotations.pending_ai.is_some() {
            self.note_result(&action, Err("AI 想法正在生成，请先完成或取消。".into()), cx);
            return;
        }
        let draft = match self.note_draft(&action, AnnotationKind::AiComment) {
            Ok(draft) => draft,
            Err(error) => {
                self.note_result(&action, Err(error), cx);
                return;
            }
        };
        // The explanation cites the page the anchor was authored on, which the
        // continuous viewer already checked against its mounted window.
        let Some(reference) =
            pdf_explanation_reference(&self.book_id, &self.pages, action.page, &draft.anchor.quote)
        else {
            self.note_result(&action, Err("所选文本已失效，请重新选择。".into()), cx);
            return;
        };
        let book_id = self.book_id.clone();
        let check = draft.clone();
        self.annotations.busy = true;
        let task = self.services.spawn_library_read(move |library| {
            library.validate_pdf_annotation_anchor(
                &book_id,
                &check.content_unit_id,
                check.document_revision,
                check.unit_revision,
                &check.anchor,
            )
        });
        cx.spawn(async move |view, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            let _ = view.update(cx, |this, cx| {
                this.annotations.busy = false;
                if this.closing || this.annotations.session != action.session {
                    return;
                }
                if let Err(error) = result {
                    this.note_result(&action, Err(format!("无法解释所选文本：{error:#}")), cx);
                    return;
                }
                let sidebar = this.ai_sidebar.clone();
                let started = match this.ai_controller.as_mut() {
                    Some(controller) => controller.explain_selection(reference, sidebar, cx),
                    None => false,
                };
                if started {
                    let revision = this.annotations.document_revision;
                    this.note_script("state", serde_json::json!({"session":action.session,"revision":revision,"request_id":action.request_id,"phase":"pending"}), cx);
                    this.annotations.pending_ai = Some(PendingAiNote {
                        action,
                        draft,
                        request_id: None,
                    });
                } else {
                    this.note_result(&action, Err("AI 正在处理其它请求，请稍后再试。".into()), cx);
                }
            });
        })
        .detach();
    }

    /// Routes the native context-menu command through the page bridge so the
    /// frozen range, not a debounced text cache, becomes the anchor.
    pub(crate) fn explain_selection(&mut self, selected_text: &str, cx: &mut Context<Self>) {
        if self.closing || self.webview_build_gate.close_requested || !self.notes_enabled() {
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
            let script = format!(
                "window.ngyAnnotations?.explainSelection({});",
                serde_json::json!(selected_text)
            );
            if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
                self.set_error(format!("无法读取笔记选区：{error}"), cx);
            }
        }
    }

    pub(crate) fn on_pdf_annotation_ai_submitted(
        &mut self,
        _: Entity<AiSidebar>,
        event: &AiExplanationSubmitted,
        _: &mut Context<Self>,
    ) {
        if let Some(pending) = self.annotations.pending_ai.as_mut()
            && pending.request_id.is_none()
        {
            pending.request_id = Some(event.request_id);
        }
    }

    pub(crate) fn on_pdf_annotation_ai_completed(
        &mut self,
        _: Entity<AiSidebar>,
        event: &AiAnswerCompleted,
        cx: &mut Context<Self>,
    ) {
        if self
            .annotations
            .pending_ai
            .as_ref()
            .is_none_or(|pending| pending.request_id != Some(event.request_id))
        {
            return;
        }
        let Some(pending) = self.annotations.pending_ai.as_mut() else {
            return;
        };
        if self.closing {
            return;
        }
        pending.draft.comment = Some(event.markdown.clone());
        let pending = pending.clone();
        // The host chose this page when it started the explanation and already
        // validated the anchor against its canonical unit, so the saved thought
        // goes back to that page even if the reader scrolled on.
        let pages = vec![pending.action.page];
        self.write_or_load_notes(pending.action, Some(pending.draft), pages, cx);
    }

    pub(crate) fn on_pdf_annotation_ai_failed(
        &mut self,
        _: Entity<AiSidebar>,
        event: &AiExplanationFailed,
        cx: &mut Context<Self>,
    ) {
        if self
            .annotations
            .pending_ai
            .as_ref()
            .is_none_or(|pending| pending.request_id != event.request_id)
        {
            return;
        }
        if let Some(pending) = self.annotations.pending_ai.take() {
            self.note_result(&pending.action, Err(event.error.clone()), cx);
        }
    }
}
