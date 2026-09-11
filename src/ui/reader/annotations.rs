//! Trusted host boundary for the reader's single-table notes.
use super::*;
use crate::ui::ai_sidebar::{AiAnswerCompleted, AiExplanationFailed, AiExplanationSubmitted};
use moye_epub_editor::annotations::{Annotation, AnnotationDraft, AnnotationKind, TextAnchor};

// This bounds only the derived display. Stored Markdown retains its existing
// limits and is never replaced with HTML.
const MAX_NOTE_DISPLAY_HTML_BYTES: usize = 8 * 1024 * 1024;

fn thought_display_html(content: &str) -> String {
    let display = crate::ui::ai_sidebar::markdown::assistant_display_markdown(content);
    if let Ok(html) = ::markdown::to_html_with_options(&display, &::markdown::Options::gfm())
        && html.len() <= MAX_NOTE_DISPLAY_HTML_BYTES
    {
        return html;
    }
    let mut html = String::from("<pre>");
    for character in content.chars() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            _ => html.push(character),
        }
    }
    html.push_str("</pre>");
    html
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum NoteAction {
    List,
    Highlight,
    Wavy,
    Underline,
    RemoveMark,
    HumanComment,
    AiExplain,
    Update,
    Delete,
    DraftChanged,
    RetryAiSave,
    DiscardAiSave,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(in crate::ui) struct AnnotationAction {
    action: NoteAction,
    session: String,
    revision: u64,
    request_id: u64,
    #[serde(default)]
    anchor: Option<TextAnchor>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    content: Option<String>,
    /// Text the reader actually displayed for the selection, sent only for an AI
    /// explanation of reading-time translation text. The stored anchor, citation
    /// and note stay the original; this only names the passage in the question.
    #[serde(default)]
    displayed_text: Option<String>,
    #[serde(default)]
    dirty: bool,
}

impl AnnotationAction {
    pub(super) fn valid(&self) -> bool {
        !self.session.is_empty()
            && self.session.len() <= 128
            && (1..=(1_u64 << 53) - 1).contains(&self.request_id)
            && self
                .id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 256)
            && self
                .content
                .as_ref()
                .is_none_or(|text| text.len() <= 64 * 1024)
            && self
                .displayed_text
                .as_ref()
                .is_none_or(|text| text.len() <= MAX_READER_SELECTION_BYTES)
            && (self.action == NoteAction::AiExplain || self.displayed_text.is_none())
            && self.anchor.as_ref().is_none_or(|a| {
                a.start < a.end
                    && !a.quote.trim().is_empty()
                    && a.quote.len() <= MAX_READER_SELECTION_BYTES
            })
            && (self.action != NoteAction::RemoveMark
                || (self.anchor.is_some() && self.id.is_none() && self.content.is_none()))
    }
}

#[derive(Clone)]
struct PendingAiNote {
    action: AnnotationAction,
    draft: AnnotationDraft,
    request_id: Option<u64>,
}

pub(super) struct ReaderAnnotations {
    pub(super) revisions: (u64, Vec<u64>),
    session: String,
    last_request: u64,
    busy: bool,
    dirty: bool,
    pending_ai: Option<PendingAiNote>,
}

impl ReaderAnnotations {
    pub(super) fn new(revisions: (u64, Vec<u64>)) -> Self {
        Self {
            revisions,
            session: String::new(),
            last_request: 0,
            busy: false,
            dirty: false,
            pending_ai: None,
        }
    }
}

fn notes_json(notes: Vec<Annotation>) -> serde_json::Value {
    serde_json::Value::Array(
        notes
            .into_iter()
            .map(|note| {
                let content = note.comment.unwrap_or_default();
                let content_html = note
                    .kind
                    .is_comment()
                    .then(|| thought_display_html(&content));
                serde_json::json!({
                    "id": note.id, "kind": note.kind, "anchor": note.anchor,
                    "content": content, "content_html": content_html, "stale": note.stale,
                    "created_at": note.created_at,
                })
            })
            .collect(),
    )
}

impl ReaderApp {
    pub(super) fn refresh_note_navigation_gate(&self) {
        if let Some(gate) = &self.protocol_gate {
            gate.annotation_navigation_blocked.store(
                self.annotations.dirty
                    || self.annotations.busy
                    || self.annotations.pending_ai.is_some(),
                Ordering::SeqCst,
            );
        }
    }
    fn note_script(&mut self, method: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        let Some(webview) = self.webview.as_ref() else {
            return;
        };
        let script = format!("window.moyeAnnotations?.{method}({value});");
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.set_error(format!("无法更新阅读笔记界面：{error}"), cx);
        }
    }

    pub(super) fn configure_annotations(&mut self, cx: &mut Context<Self>) {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        self.annotations.session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
        self.annotations.last_request = 0;
        self.annotations.dirty = false;
        let revision = self
            .annotations
            .revisions
            .1
            .get(self.current_spine)
            .copied()
            .unwrap_or(0);
        self.note_script(
            "configure",
            serde_json::json!({
                "session": self.annotations.session, "revision": revision, "notes": [],
            }),
            cx,
        );
    }

    fn note_result(
        &mut self,
        action: &AnnotationAction,
        result: Result<serde_json::Value, String>,
        cx: &mut Context<Self>,
    ) {
        if self.annotations.session != action.session || self.closing {
            return;
        }
        let payload = match result {
            Ok(notes) => {
                if action.action != NoteAction::List {
                    self.notice = None;
                    cx.notify();
                }
                serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"ok":true,"notes":notes})
            }
            Err(error) => {
                self.set_error(error.clone(), cx);
                serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"ok":false,"error":error})
            }
        };
        self.note_script("result", payload, cx);
    }

    pub(super) fn annotation_navigation_blocked(&mut self, cx: &mut Context<Self>) -> bool {
        let reason = if self.annotations.dirty {
            Some("人工想法尚未保存，请先保存或取消编辑。")
        } else if self.annotations.busy {
            Some("笔记正在保存，请完成后再切章或关闭。")
        } else if self
            .annotations
            .pending_ai
            .as_ref()
            .is_some_and(|pending| pending.draft.comment.is_some())
        {
            Some("AI 想法尚未保存，请在笔记面板重试保存或放弃。")
        } else if self.annotations.pending_ai.is_some() {
            Some("AI 想法正在生成，请完成或在 AI 侧栏取消后再切章。")
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

    fn note_draft(
        &self,
        action: &AnnotationAction,
        kind: AnnotationKind,
    ) -> Result<AnnotationDraft, String> {
        let unit = self
            .progress_locators
            .get(self.current_spine)
            .map(|l| l.unit_id.clone())
            .ok_or("当前章节没有可用的笔记位置")?;
        Ok(AnnotationDraft {
            content_unit_id: unit,
            document_revision: self.annotations.revisions.0,
            unit_revision: action.revision,
            anchor: action.anchor.clone().ok_or("请重新选择正文文字")?,
            kind,
            comment: action.content.clone(),
        })
    }

    pub(super) fn handle_annotation_action(
        &mut self,
        url: &str,
        action: AnnotationAction,
        cx: &mut Context<Self>,
    ) {
        if self.closing
            || self.webview_build_gate.close_requested
            || self.book.spine_index_for_url(url) != Some(self.current_spine)
            || action.session != self.annotations.session
            || self.annotations.revisions.1.get(self.current_spine) != Some(&action.revision)
            || action.request_id <= self.annotations.last_request
        {
            return;
        }
        self.annotations.last_request = action.request_id;
        if action.action == NoteAction::DraftChanged {
            self.annotations.dirty = action.dirty;
            return;
        }
        if self.annotations.busy {
            self.note_result(&action, Err("笔记正在处理，请稍后再试。".into()), cx);
            return;
        }
        if action.action == NoteAction::RetryAiSave {
            if let Some(pending) = self.annotations.pending_ai.as_mut()
                && pending.draft.comment.is_some()
            {
                pending.action = action.clone();
                let draft = pending.draft.clone();
                self.write_or_load_notes(action, Some(draft), cx);
            } else {
                self.note_result(&action, Err("没有待保存的 AI 想法。".into()), cx);
            }
            return;
        }
        if action.action == NoteAction::DiscardAiSave {
            if self
                .annotations
                .pending_ai
                .as_ref()
                .is_some_and(|pending| pending.draft.comment.is_some())
            {
                self.annotations.pending_ai = None;
                self.write_or_load_notes(action, None, cx);
            } else {
                self.note_result(&action, Err("没有可放弃的待保存 AI 想法。".into()), cx);
            }
            return;
        }
        if self.annotations.pending_ai.is_some() {
            self.note_result(&action, Err("请先完成或取消当前 AI 想法。".into()), cx);
            return;
        }
        if action.action == NoteAction::AiExplain {
            self.start_annotation_explanation(url, action, cx);
            return;
        }
        let kind = match action.action {
            NoteAction::Highlight => Some(AnnotationKind::Highlight),
            NoteAction::Wavy => Some(AnnotationKind::Wavy),
            NoteAction::Underline => Some(AnnotationKind::Underline),
            NoteAction::HumanComment => Some(AnnotationKind::HumanComment),
            _ => None,
        };
        let draft = match kind.map(|kind| self.note_draft(&action, kind)).transpose() {
            Ok(draft) => draft,
            Err(error) => {
                self.note_result(&action, Err(error), cx);
                return;
            }
        };
        self.write_or_load_notes(action, draft, cx);
    }

    fn write_or_load_notes(
        &mut self,
        action: AnnotationAction,
        draft: Option<AnnotationDraft>,
        cx: &mut Context<Self>,
    ) {
        let book_id = self.book_id.clone();
        let unit_id = Some(self.progress_locators[self.current_spine].unit_id.clone());
        let document_revision = self.annotations.revisions.0;
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
        self.refresh_note_navigation_gate();
        let task = self.services.spawn_library(move |library| {
            if let Some(draft) = draft {
                library.create_annotation(&book_id, &draft)?;
            } else {
                match operation.action {
                    NoteAction::RemoveMark => library.delete_annotation_marks(
                        &book_id,
                        unit_id.as_deref().context("当前章节没有可用的笔记位置")?,
                        document_revision,
                        operation.revision,
                        operation.anchor.as_ref().context("请重新选择正文文字")?,
                    )?,
                    NoteAction::Update | NoteAction::Delete => {
                        let id = operation.id.as_deref().context("笔记 ID 不能为空")?;
                        // A page cannot mutate another chapter's notes even within the same book.
                        ensure!(
                            library
                                .list_annotations(&book_id, unit_id.as_deref())?
                                .iter()
                                .any(|note| note.id == id),
                            "笔记不属于当前章节"
                        );
                        if operation.action == NoteAction::Delete {
                            library.delete_annotation(&book_id, id)?;
                        } else {
                            library.update_human_comment(
                                &book_id,
                                id,
                                operation.content.as_deref().context("想法不能为空")?,
                            )?;
                        }
                    }
                    NoteAction::List | NoteAction::DiscardAiSave => {}
                    _ => anyhow::bail!("无效的笔记操作"),
                }
            }
            // The mutation above has committed. A later refresh failure must
            // not invite a duplicate insert on retry.
            Ok::<_, anyhow::Error>(library.list_annotations(&book_id, unit_id.as_deref()))
        });
        cx.spawn(async move |view, cx| {
            let (committed, result) = match task.await {
                Ok(Ok(result)) => (action.action != NoteAction::List, result.map_err(|error| format!("无法刷新笔记：{error:#}"))),
                Ok(Err(error)) => (false, Err(format!("笔记处理失败：{error:#}"))),
                Err(error) => (false, Err(format!("笔记任务已停止：{error}"))),
            };
            // Parse away from GPUI and after releasing the library mutation
            // lock. Even a large AI thought must not freeze reader input.
            let (result, failed_content_html) = match display_runtime.handle().spawn_blocking(move || {
                let failed_html = if result.is_err() {
                    pending_markdown.as_deref().map(thought_display_html)
                } else { None };
                (result.map(notes_json), failed_html)
            }).await {
                Ok(value) => value,
                Err(error) => (Err(format!("笔记展示任务已停止：{error}")), None),
            };
            let _ = view.update(cx, |this, cx| {
                this.annotations.busy = false;
                this.refresh_note_navigation_gate();
                if committed && result.is_err() {
                    if is_ai_save { this.annotations.pending_ai = None; this.refresh_note_navigation_gate(); }
                    this.note_script("result", serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"ok":true}), cx);
                    this.set_error("笔记已更新，但列表刷新失败；请重新打开笔记面板。".into(), cx);
                    return;
                }
                if is_ai_save {
                    if let Err(error) = &result {
                        let content = this.annotations.pending_ai.as_ref().and_then(|pending| pending.draft.comment.as_deref()).unwrap_or_default().to_owned();
                        this.note_script("result", serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"ok":false,"error":error,"retry_ai_save":true,"content":content,"content_html":failed_content_html}), cx);
                        this.set_error(error.clone(), cx);
                        return;
                    }
                    this.annotations.pending_ai = None;
                    this.refresh_note_navigation_gate();
                }
                this.note_result(&action, result, cx);
                cx.notify();
            });
        }).detach();
    }

    fn start_annotation_explanation(
        &mut self,
        url: &str,
        action: AnnotationAction,
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
        let Some(reference) = reader_explanation_reference(
            &self.book_id,
            &self.book,
            &self.progress_locators,
            self.current_spine,
            url,
            &draft.anchor.quote,
            action.displayed_text.as_deref(),
        ) else {
            self.note_result(&action, Err("所选文本已失效，请重新选择。".into()), cx);
            return;
        };
        let book_id = self.book_id.clone();
        let check = draft.clone();
        self.annotations.busy = true;
        self.refresh_note_navigation_gate();
        let task = self.services.spawn_library_read(move |library| {
            library.validate_annotation_anchor(
                &book_id,
                &check.content_unit_id,
                check.document_revision,
                check.unit_revision,
                &check.anchor,
            )
        });
        cx.spawn(async move |view, cx| {
            let result = match task.await { Ok(result) => result, Err(error) => Err(anyhow::anyhow!(error)) };
            let _ = view.update(cx, |this, cx| {
                this.annotations.busy = false;
                this.refresh_note_navigation_gate();
                if this.closing || this.annotations.session != action.session { return; }
                if let Err(error) = result {
                    this.note_result(&action, Err(format!("无法解释所选文本：{error:#}")), cx); return;
                }
                if this.ai_controller.explain_selection(reference, this.ai_sidebar.clone(), cx) {
                    this.note_script("state", serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"phase":"pending"}), cx);
                    this.annotations.pending_ai = Some(PendingAiNote {action, draft, request_id: None});
                    this.refresh_note_navigation_gate();
                } else {
                    this.note_result(&action, Err("AI 正在处理其它请求，请稍后再试。".into()), cx);
                }
            });
        }).detach();
    }

    pub(super) fn on_annotation_ai_submitted(
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

    pub(super) fn on_annotation_ai_completed(
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
        self.write_or_load_notes(pending.action, Some(pending.draft), cx);
    }

    pub(super) fn on_annotation_ai_failed(
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
            self.refresh_note_navigation_gate();
            self.note_result(&pending.action, Err(event.error.clone()), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thoughts_render_gfm_but_preserve_both_authors_markdown_source() {
        let source = "## 标题\n\n**重点**和*强调*\n\n- 第一项\n- 第二项\n\n> 引用\n\n```rust\nlet value = 1;\n```\n\n| 类别 | 内容 |\n| --- | --- |\n| 人工 | 想法 |\n\n~~已修改~~";
        let records = [AnnotationKind::HumanComment, AnnotationKind::AiComment]
            .into_iter()
            .enumerate()
            .map(|(index, kind)| Annotation {
                id: format!("note-{index}"),
                book_id: "book".into(),
                content_unit_id: "unit".into(),
                document_revision: 1,
                unit_revision: 1,
                anchor: TextAnchor {
                    quote: "引文".into(),
                    start: 0,
                    end: 2,
                },
                kind,
                comment: Some(source.into()),
                created_at: 1,
                updated_at: 1,
                stale: false,
            })
            .collect();
        let payload = notes_json(records);
        for entry in payload.as_array().unwrap() {
            assert_eq!(entry["content"], source);
            let html = entry["content_html"].as_str().unwrap();
            for expected in [
                "<h2>标题</h2>",
                "<strong>重点</strong>",
                "<em>强调</em>",
                "<ul>",
                "<blockquote>",
                "<pre><code",
                "<table>",
                "<del>已修改</del>",
            ] {
                assert!(html.contains(expected), "missing {expected}: {html}");
            }
        }
        assert_eq!(payload[0]["content_html"], payload[1]["content_html"]);
    }

    #[test]
    fn thought_html_never_activates_model_links_images_or_raw_html() {
        let html = thought_display_html(
            "[链接](https://example.invalid/x)\n\n![图片说明](https://example.invalid/image.png)\n\n<img src=x onerror=alert(1)>\n\n<script>alert(2)</script>\n\n`<iframe>`",
        );
        for forbidden in ["<a ", "<img", "<script", "<iframe", "href=", "src=\"https:"] {
            assert!(!html.contains(forbidden), "{forbidden}: {html}");
        }
        assert!(html.contains("链接"));
        assert!(html.contains("图片说明"));
        assert!(html.contains("&lt;img"));
        assert!(html.contains("&lt;script"));
    }

    #[test]
    fn typed_note_ipc_rejects_remote_sources_and_bounds_untrusted_content() {
        let body = r#"{"type":"annotation_action","action":"human_comment","session":"1","revision":1,"request_id":1,"anchor":{"quote":"选区","start":0,"end":2},"content":"人工想法"}"#;
        assert!(matches!(
            reader_ipc_event(
                &"http://epubreader.book/text/one.xhtml".parse().unwrap(),
                body
            ),
            Some(ReaderWebEvent::AnnotationAction { .. })
        ));
        assert!(
            reader_ipc_event(&"https://example.com/text/one.xhtml".parse().unwrap(), body)
                .is_none()
        );
        for replacement in [
            body.replace("human_comment", "ai_comment"),
            body.replace("\"request_id\":1", "\"request_id\":0"),
            body.replace("人工想法", &"x".repeat(64 * 1024 + 1)),
            body.replace("\"end\":2", "\"end\":0"),
            // Displayed译文 text belongs to an AI explanation only, and stays
            // inside the reader's own selection limit.
            body.replace(
                r#""content":"人工想法""#,
                &format!(
                    r#""content":"人工想法","displayed_text":"{}""#,
                    "译".repeat(MAX_READER_SELECTION_BYTES)
                ),
            ),
        ] {
            assert!(
                reader_ipc_event(
                    &"http://epubreader.book/text/one.xhtml".parse().unwrap(),
                    &replacement
                )
                .is_none()
            );
        }
        // An explanation may name the译文 the reader saw.
        let explained = r#"{"type":"annotation_action","action":"ai_explain","session":"1","revision":1,"request_id":1,"anchor":{"quote":"选区","start":0,"end":2},"displayed_text":"译文段落"}"#;
        assert!(matches!(
            reader_ipc_event(
                &"http://epubreader.book/text/one.xhtml".parse().unwrap(),
                explained
            ),
            Some(ReaderWebEvent::AnnotationAction {
                action: AnnotationAction {
                    action: NoteAction::AiExplain,
                    displayed_text: Some(text),
                    ..
                },
                ..
            }) if text == "译文段落"
        ));
    }

    #[test]
    fn remove_mark_ipc_requires_a_range_and_cannot_target_a_thought_by_id() {
        let body = r#"{"type":"annotation_action","action":"remove_mark","session":"1","revision":1,"request_id":1,"anchor":{"quote":"选区","start":0,"end":2}}"#;
        let origin = "http://epubreader.book/text/one.xhtml".parse().unwrap();
        assert!(matches!(
            reader_ipc_event(&origin, body),
            Some(ReaderWebEvent::AnnotationAction {
                action: AnnotationAction {
                    action: NoteAction::RemoveMark,
                    ..
                },
                ..
            })
        ));
        for invalid in [
            body.replace("remove_mark", "strikethrough"),
            body.replace(r#","anchor":{"quote":"选区","start":0,"end":2}"#, ""),
            body.replace(r#""request_id":1"#, r#""request_id":1,"id":"human-note""#),
            body.replace(r#""request_id":1"#, r#""request_id":1,"content":"想法""#),
        ] {
            assert!(reader_ipc_event(&origin, &invalid).is_none(), "{invalid}");
        }
    }
}
