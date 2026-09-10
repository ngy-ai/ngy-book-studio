//! Trusted host boundary for the PDF reader's single-table notes.
//!
//! Marks, human thoughts and AI thoughts use the very same `annotations` table
//! and the very same exclusive-mark rules as the EPUB reader. Only the anchor
//! contract differs: a PDF anchor addresses the bundled PDF.js text layer of
//! one page instead of a reflowable chapter body, so the host verifies book and
//! unit ownership, both revisions and the anchor's bounds while the immutable
//! original plus the pinned PDF.js build keep the offsets reproducible.
use super::*;
use crate::ui::ai_sidebar::{AiAnswerCompleted, AiExplanationFailed, AiExplanationSubmitted};
use anyhow::ensure;
use moye_epub_editor::annotations::{Annotation, AnnotationDraft, AnnotationKind, TextAnchor};
use std::sync::atomic::{AtomicU64, Ordering};

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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PdfNoteAction {
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

/// Typed notes request from the bundled viewer's private origin.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct PdfAnnotationAction {
    action: PdfNoteAction,
    session: String,
    revision: u64,
    request_id: u64,
    #[serde(default)]
    page: u32,
    #[serde(default)]
    anchor: Option<TextAnchor>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    dirty: bool,
}

impl PdfAnnotationAction {
    pub(super) fn valid(&self) -> bool {
        !self.session.is_empty()
            && self.session.len() <= 128
            // Every request names the page it was authored on. `handle` then
            // compares it with the page actually rendered.
            && self.page >= 1
            && (1..=(1_u64 << 53) - 1).contains(&self.request_id)
            && self
                .id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 256)
            && self
                .content
                .as_ref()
                .is_none_or(|text| text.len() <= 64 * 1024)
            && self.anchor.as_ref().is_none_or(|anchor| {
                anchor.start < anchor.end
                    && !anchor.quote.trim().is_empty()
                    && anchor.quote.len() <= MAX_PDF_SELECTION_BYTES
            })
            // A page may never author an AI thought or delete a thought by ID
            // through the mark path.
            && (self.action != PdfNoteAction::RemoveMark
                || (self.anchor.is_some() && self.id.is_none() && self.content.is_none()))
    }
}

#[derive(Clone)]
struct PendingAiNote {
    action: PdfAnnotationAction,
    draft: AnnotationDraft,
    request_id: Option<u64>,
}

pub(super) struct PdfAnnotations {
    /// Canonical document revision every note draft is written with. It has no
    /// say in whether notes are offered: `0` is the ordinary revision of every
    /// imported or created book, so a zero value must not hide the toolbar.
    document_revision: u64,
    session: String,
    last_request: u64,
    busy: bool,
    dirty: bool,
    pending_ai: Option<PendingAiNote>,
}

impl PdfAnnotations {
    pub(super) fn new(document_revision: u64) -> Self {
        Self {
            document_revision,
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

/// Canonical identity of the page a note may be written to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PdfNotePosition {
    unit_id: String,
    unit_revision: u64,
    page_number: u32,
}

/// Resolves whether the rendered page can own persisted notes.
///
/// Only the page's own canonical unit identity decides this: without a stable
/// unit ID nothing could be associated without guessing, which is how
/// ephemeral Office PDFs are represented. The document revision must never
/// gate this because `Revision::INITIAL` (`0`) is the ordinary revision of
/// every imported or created book, and only editing advances it.
fn pdf_note_position(pages: &[PdfReaderPage], current_page: u32) -> Option<PdfNotePosition> {
    let page = pages.iter().find(|page| page.page_number == current_page)?;
    let unit_id = page
        .unit_id
        .as_deref()
        .filter(|unit_id| !unit_id.trim().is_empty())?;
    Some(PdfNotePosition {
        unit_id: unit_id.to_string(),
        unit_revision: page.unit_revision,
        page_number: page.page_number,
    })
}

impl PdfReaderApp {
    fn note_position(&self) -> Option<PdfNotePosition> {
        pdf_note_position(&self.pages, self.current_page)
    }

    pub(super) fn notes_enabled(&self) -> bool {
        self.note_position().is_some()
    }

    fn note_script(&mut self, method: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        let Some(webview) = self.webview.as_ref() else {
            return;
        };
        let script = format!("window.moyeAnnotations?.{method}({value});");
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            self.set_error(format!("无法更新 PDF 笔记界面：{error}"), cx);
        }
    }

    /// Rebinds the page bridge to the page that is actually rendered. A new
    /// session invalidates every in-flight request from the previous page.
    pub(super) fn configure_annotations(&mut self, cx: &mut Context<Self>) {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        let Some(position) = self.note_position() else {
            // An ephemeral preview page carries no canonical identity, so it
            // cannot own notes. Retire the previous page's session before the
            // page bridge hides its controls: a late request from the page
            // that just scrolled away must not reach this one.
            self.annotations.session.clear();
            self.annotations.last_request = 0;
            self.annotations.dirty = false;
            self.note_script("disable", serde_json::json!({}), cx);
            return;
        };
        self.annotations.session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
        self.annotations.last_request = 0;
        self.annotations.dirty = false;
        self.note_script(
            "configure",
            serde_json::json!({
                "session": self.annotations.session,
                "revision": position.unit_revision,
                "page": position.page_number,
                "notes": [],
            }),
            cx,
        );
    }

    fn note_result(
        &mut self,
        action: &PdfAnnotationAction,
        result: Result<serde_json::Value, String>,
        cx: &mut Context<Self>,
    ) {
        if self.annotations.session != action.session || self.closing {
            return;
        }
        let payload = match result {
            Ok(notes) => {
                if action.action != PdfNoteAction::List {
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

    /// Page changes and window close must not silently drop an unsaved thought.
    pub(super) fn annotation_navigation_blocked(&mut self, cx: &mut Context<Self>) -> bool {
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

    fn note_draft(
        &self,
        action: &PdfAnnotationAction,
        kind: AnnotationKind,
    ) -> Result<AnnotationDraft, String> {
        let position = self.note_position().ok_or("当前页面没有可用的笔记位置")?;
        Ok(AnnotationDraft {
            content_unit_id: position.unit_id,
            document_revision: self.annotations.document_revision,
            unit_revision: action.revision,
            anchor: action.anchor.clone().ok_or("请重新选择页面文字")?,
            kind,
            comment: action.content.clone(),
        })
    }

    pub(super) fn handle_annotation_action(
        &mut self,
        action: PdfAnnotationAction,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.note_position() else {
            return;
        };
        // Reject anything that does not describe the page currently rendered in
        // this exact session generation, plus replayed request numbers.
        if self.closing
            || self.webview_build_gate.close_requested
            || action.session != self.annotations.session
            || action.revision != position.unit_revision
            || action.page != position.page_number
            || action.request_id <= self.annotations.last_request
        {
            return;
        }
        self.annotations.last_request = action.request_id;
        if action.action == PdfNoteAction::DraftChanged {
            self.annotations.dirty = action.dirty;
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
                self.write_or_load_notes(action, Some(draft), cx);
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
        self.write_or_load_notes(action, draft, cx);
    }

    fn write_or_load_notes(
        &mut self,
        action: PdfAnnotationAction,
        draft: Option<AnnotationDraft>,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.note_position() else {
            return;
        };
        let book_id = self.book_id.clone();
        let unit_id = position.unit_id;
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
                    PdfNoteAction::List | PdfNoteAction::DiscardAiSave => {}
                    _ => anyhow::bail!("无效的笔记操作"),
                }
            }
            // The mutation above has committed. A later refresh failure must
            // not invite a duplicate insert on retry.
            Ok::<_, anyhow::Error>(library.list_annotations(&book_id, Some(&unit_id)))
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
                    (result.map(notes_json), failed_html)
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
                    this.note_script("result", serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"ok":true}), cx);
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
                        this.note_script("result", serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"ok":false,"error":error,"retry_ai_save":true,"content":content,"content_html":failed_content_html}), cx);
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

    fn start_annotation_explanation(
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
        let Some(reference) = pdf_explanation_reference(
            &self.book_id,
            &self.pages,
            self.current_page,
            &draft.anchor.quote,
        ) else {
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
                    this.note_script("state", serde_json::json!({"session":action.session,"revision":action.revision,"request_id":action.request_id,"phase":"pending"}), cx);
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
    pub(super) fn explain_selection(&mut self, selected_text: &str, cx: &mut Context<Self>) {
        if self.closing || self.webview_build_gate.close_requested || !self.notes_enabled() {
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
            let script = format!(
                "window.moyeAnnotations?.explainSelection({});",
                serde_json::json!(selected_text)
            );
            if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
                self.set_error(format!("无法读取笔记选区：{error}"), cx);
            }
        }
    }

    pub(super) fn on_pdf_annotation_ai_submitted(
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

    pub(super) fn on_pdf_annotation_ai_completed(
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

    pub(super) fn on_pdf_annotation_ai_failed(
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

/// Freezes the selected page text onto the current page's canonical reference.
fn pdf_explanation_reference(
    book_id: &str,
    pages: &[PdfReaderPage],
    current_page: u32,
    quote: &str,
) -> Option<AiReferenceHint> {
    let selection = PdfTextSelection {
        request_id: 0,
        page_number: current_page,
        text: normalize_pdf_selection(quote),
    };
    if selection.text.is_empty() || selection.text.len() > MAX_PDF_SELECTION_BYTES {
        return None;
    }
    pdf_reference_hints(book_id, pages, current_page, 0, Some(&selection))
        .into_iter()
        .find(|reference| reference.frozen_text.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_body(action: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"annotation_action","action":"{action}","session":"1","revision":3,"request_id":1,"page":7{extra}}}"#
        )
    }

    fn parse(body: &str) -> Option<PdfAnnotationAction> {
        match serde_json::from_str::<PdfIpcMessage>(body).ok()? {
            PdfIpcMessage::AnnotationAction { action } => action.valid().then_some(action),
            _ => None,
        }
    }

    /// An imported book that was never edited carries `Revision::INITIAL` (`0`)
    /// for both its document and its pages. Reading that as "no canonical
    /// identity" silently hid the selection toolbar on every such book.
    #[test]
    fn any_canonical_page_owns_notes_including_the_first_imported_revision() {
        let imported = PdfReaderPage {
            unit_id: Some("unit-1".to_string()),
            unit_index: Some(0),
            unit_revision: 0,
            title: "Page 1".to_string(),
            page_number: 1,
        };
        assert_eq!(
            pdf_note_position(&[imported.clone()], 1),
            Some(PdfNotePosition {
                unit_id: "unit-1".to_string(),
                unit_revision: 0,
                page_number: 1,
            })
        );
        assert_eq!(pdf_note_position(&[imported.clone()], 2), None);
        // The canonical unit ID alone decides ownership; a page number without
        // one stays a preview, and the spine index is not notes metadata.
        for unit_id in [None, Some("   ".to_string())] {
            assert!(
                pdf_note_position(
                    &[PdfReaderPage {
                        unit_id,
                        ..imported.clone()
                    }],
                    1
                )
                .is_none(),
                "a page without canonical identity cannot own notes"
            );
        }
        assert_eq!(
            pdf_note_position(
                &[PdfReaderPage {
                    unit_index: None,
                    ..imported
                }],
                1
            )
            .map(|position| position.unit_id),
            Some("unit-1".to_string()),
            "a unit index is not required to own notes"
        );
    }

    #[test]
    fn typed_note_ipc_bounds_untrusted_content_and_forbids_ai_authorship() {
        let body = action_body(
            "human_comment",
            r#","anchor":{"quote":"选区","start":0,"end":2},"content":"人工想法""#,
        );
        let action = parse(&body).expect("valid human thought action");
        assert_eq!(action.action, PdfNoteAction::HumanComment);
        assert_eq!(action.page, 7);
        assert_eq!(action.revision, 3);

        for invalid in [
            body.replace("human_comment", "ai_comment"),
            body.replace(r#""request_id":1"#, r#""request_id":0"#),
            body.replace(r#""page":7"#, r#""page":0"#),
            body.replace(r#","page":7"#, ""),
            body.replace("人工想法", &"x".repeat(64 * 1024 + 1)),
            body.replace(r#""end":2"#, r#""end":0"#),
            body.replace(r#""quote":"选区""#, r#""quote":"  ""#),
            body.replace(
                r#""quote":"选区""#,
                &format!(r#""quote":"{}""#, "x".repeat(MAX_PDF_SELECTION_BYTES + 1)),
            ),
        ] {
            assert!(parse(&invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn remove_mark_ipc_requires_a_range_and_cannot_target_a_thought_by_id() {
        let body = action_body(
            "remove_mark",
            r#","anchor":{"quote":"选区","start":0,"end":2}"#,
        );
        assert_eq!(
            parse(&body).expect("valid mark removal").action,
            PdfNoteAction::RemoveMark
        );
        for invalid in [
            action_body(
                "strikethrough",
                r#","anchor":{"quote":"选区","start":0,"end":2}"#,
            ),
            action_body("remove_mark", ""),
            body.replace(r#""page":7"#, r#""page":7,"id":"human-note""#),
            body.replace(r#""page":7"#, r#""page":7,"content":"想法""#),
        ] {
            assert!(parse(&invalid).is_none(), "{invalid}");
        }
    }

    /// The injected bridge is the only page-side author of note requests, so
    /// its contract with this host is pinned here: private origin, PDF.js text
    /// layer scope, and a page number on every request.
    #[test]
    fn trusted_pdf_bridge_binds_every_request_to_the_rendered_page() {
        let bridge = include_str!("annotations.js");
        for contract in [
            "window.top === window",
            "moyepdf.viewer",
            "\"moyepdf:\"",
            "querySelector(\".textLayer\")",
            "attachShadow({ mode: \"closed\" })",
            "type: \"annotation_action\"",
            "page: context.page",
            "!Number.isSafeInteger(payload.page) || payload.page < 1",
            "disable()",
        ] {
            assert!(
                bridge.contains(contract),
                "missing bridge contract: {contract}"
            );
        }
        // The page may only ask for an explanation; the AI thought itself is
        // written by this host after the answer is saved.
        assert!(!bridge.contains(r#"post("ai_comment""#));
        assert!(!bridge.contains(r#"choose("ai_comment")"#));
    }

    #[test]
    fn thoughts_render_gfm_but_preserve_both_authors_markdown_source() {
        let source = "## 标题\n\n**重点**\n\n- 一项\n\n> 引用\n\n```rust\nlet value = 1;\n```\n\n| 类别 | 内容 |\n| --- | --- |\n| 人工 | 想法 |";
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
                "<ul>",
                "<blockquote>",
                "<pre><code",
                "<table>",
            ] {
                assert!(html.contains(expected), "missing {expected}: {html}");
            }
        }
        assert_eq!(payload[0]["content_html"], payload[1]["content_html"]);
    }

    #[test]
    fn thought_html_never_activates_model_links_images_or_raw_html() {
        let html = thought_display_html(
            "[链接](https://example.invalid/x)\n\n![图片说明](https://example.invalid/image.png)\n\n<img src=x onerror=alert(1)>\n\n<script>alert(2)</script>",
        );
        for forbidden in ["<a ", "<img", "<script", "<iframe", "href=", "src=\"https:"] {
            assert!(!html.contains(forbidden), "{forbidden}: {html}");
        }
        assert!(html.contains("链接"));
        assert!(html.contains("图片说明"));
        assert!(html.contains("&lt;script"));
    }

    #[test]
    fn explanation_reference_freezes_the_selection_on_the_current_page_only() {
        let pages = vec![
            PdfReaderPage {
                unit_id: Some("unit-1".to_string()),
                unit_index: Some(0),
                unit_revision: 1,
                title: "One".to_string(),
                page_number: 1,
            },
            PdfReaderPage {
                unit_id: Some("unit-2".to_string()),
                unit_index: Some(1),
                unit_revision: 4,
                title: "Two".to_string(),
                page_number: 2,
            },
            PdfReaderPage {
                unit_id: None,
                unit_index: None,
                unit_revision: 0,
                title: "Ephemeral".to_string(),
                page_number: 3,
            },
        ];

        let reference =
            pdf_explanation_reference("book-a", &pages, 2, "  所选\n\t文本  ").expect("reference");
        assert_eq!(reference.unit_id, "unit-2");
        assert_eq!(reference.frozen_text.as_deref(), Some("所选 文本"));
        assert_eq!(
            reference.locator,
            Some(DocumentLocator::unit("book-a", "unit-2").with_source(SourceLocator::pdf_page(2)))
        );

        assert!(pdf_explanation_reference("book-a", &pages, 2, " \n\t ").is_none());
        assert!(
            pdf_explanation_reference("book-a", &pages, 3, "临时页面").is_none(),
            "a page without canonical identity cannot become an AI reference"
        );
        assert!(pdf_explanation_reference("book-a", &pages, 9, "缺失页面").is_none());
    }
}
