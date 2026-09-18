//! Tabs, chapter and TOC selection, and the WebView load that follows them.

use super::*;

impl EditorApp {
    /// Reloads the editor webview for the preview or rich-text tab using a
    /// fresh `epubeditor://` URL. A new URL every time guarantees the webview
    /// performs a real navigation and re-queries the custom protocol.
    pub(crate) fn load_webview(
        &mut self,
        tab: EditorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.load_webview_with_focus(tab, None, false, window, cx);
    }

    pub(crate) fn load_webview_for_citation(
        &mut self,
        target: AiCitationNavigationTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_webview.is_none() {
            self.set_error("预览视图尚未就绪，无法打开引用", cx);
            return;
        }
        self.load_webview_with_focus(EditorTab::Preview, target.focus, true, window, cx);
    }

    pub(crate) fn load_webview_with_focus(
        &mut self,
        tab: EditorTab,
        citation_focus: Option<AiCitationTextFocus>,
        citation_navigation_requested: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ready_timeout_task.take();
        self.pending_citation_navigation = None;
        self.ai_selected_text = None;
        let Some(webview) = &self.editor_webview else {
            self.active_web_page = None;
            return;
        };
        webview.update(cx, |webview, _| webview.show());
        let Some(chapter) = self.chapters.get(self.selected) else {
            return;
        };
        let Some(unit) = self.unit_states.get(self.selected) else {
            self.active_web_page = None;
            return;
        };
        self.web_revision += 1;
        let revision = self.web_revision;
        let session_id = self.web_state.session_id().to_string();
        let chapter_id = unit.id.clone();
        self.web_state.set(
            revision,
            chapter_id.clone(),
            chapter.href.clone(),
            chapter.html.clone(),
        );
        let url = editor_navigation_url(tab, &session_id, &chapter_id, revision, &chapter.href);
        self.pending_citation_navigation =
            citation_focus.map(|focus| PendingEditorCitationNavigation {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: chapter.href.clone(),
                revision,
                request_id: revision,
                focus,
            });
        self.active_web_page = if tab == EditorTab::RichText {
            Some(ActiveEditorPage {
                session_id: session_id.clone(),
                chapter_id: chapter_id.clone(),
                href: chapter.href.clone(),
                revision,
                ready: false,
            })
        } else {
            None
        };
        if let Err(error) = webview.read(cx).raw().load_url(&url) {
            self.active_web_page = None;
            self.pending_citation_navigation = None;
            tracing::warn!(%error, "failed to load editor page");
            if citation_navigation_requested {
                self.set_error(format!("无法打开引用预览：{error}"), cx);
            }
        } else if tab == EditorTab::RichText {
            let href = chapter.href.clone();
            self.ready_timeout_task = Some(cx.spawn_in(window, async move |view, cx| {
                Timer::after(EDITOR_READY_TIMEOUT).await;
                let _ = view.update_in(cx, |editor, window, cx| {
                    editor.handle_editor_ready_timeout(
                        &session_id,
                        &chapter_id,
                        revision,
                        &href,
                        window,
                        cx,
                    )
                });
            }));
        }
        self.sync_ai_reference(cx);
    }

    /// Pushes the title of `index` into the chapter-title input.
    pub(crate) fn sync_chapter_title_input(
        &self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(chapter) = self.chapters.get(index) {
            self.chapter_title_input.update(cx, |state, cx| {
                state.set_value(chapter.title.clone(), window, cx);
            });
        }
    }

    /// Pushes the canonical HTML source into the source editor.
    pub(crate) fn sync_body_input(
        &self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(source) = self
            .unit_states
            .get(index)
            .map(|unit| unit.source.clone())
            .or_else(|| self.chapters.get(index).map(|chapter| chapter.html.clone()))
        {
            self.body_input.update(cx, |state, cx| {
                state.set_value(source, window, cx);
            });
        }
    }

    pub(crate) fn sync_ai_reference(&mut self, cx: &mut Context<Self>) {
        let live_title = self.chapter_title_input.read(cx).value().trim().to_string();
        let references = editor_reference_hints(
            &self.book_id,
            &self.chapters,
            &self.unit_states,
            self.selected,
            (!live_title.is_empty()).then_some(live_title.as_str()),
            self.ai_selected_text.as_deref(),
            self.web_revision,
        );
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_reference_hints(references, cx);
        });
    }

    pub(crate) fn sync_ai_scope(&mut self, cx: &mut Context<Self>) {
        let current = AiBookOption::new(
            self.book_id.clone(),
            self.title_input.read(cx).value().to_string(),
        );
        let available = self
            .library
            .books()
            .iter()
            .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
            .collect();
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.set_scope(AiSidebarScope::book(current, available), cx);
        });
    }

    pub(crate) fn select_chapter(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index == self.selected || index >= self.chapters.len() {
            return;
        }
        self.begin_action(PendingEditorAction::SelectChapter(index), window, cx);
    }

    pub(crate) fn select_toc(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_toc_id = Some(id.to_string());
        self.begin_action(PendingEditorAction::SelectToc, window, cx);
    }

    pub(crate) fn current_toc_id(&self) -> Option<String> {
        let document = self.canonical_document.as_ref()?;
        let unit = self.unit_states.get(self.selected)?;
        let rows = editor_toc_rows(&document.toc);
        self.selected_toc_id
            .as_ref()
            .and_then(|id| {
                rows.iter()
                    .find(|row| row.id == *id && row.unit_id == unit.id)
            })
            .or_else(|| rows.iter().find(|row| row.unit_id == unit.id))
            .map(|row| row.id.clone())
    }

    pub(crate) fn perform_select_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.pending_toc_id.take() else {
            return;
        };
        let target = self.canonical_document.as_ref().and_then(|document| {
            editor_toc_rows(&document.toc)
                .into_iter()
                .find(|row| row.id == id)
        });
        let Some(target) = target else {
            self.set_error("目录项已不存在，请重新选择", cx);
            return;
        };
        let Some(index) = self
            .unit_states
            .iter()
            .position(|unit| unit.id == target.unit_id)
        else {
            self.set_error("目录对应的章节已不存在，请重新打开编辑窗口", cx);
            return;
        };
        self.perform_select_chapter(index, window, cx);
        // Source validation may veto switching. Never highlight a destination
        // whose content did not become the current editable unit.
        if self.selected == index {
            self.selected_toc_id = Some(id);
            cx.notify();
        }
    }

    pub(crate) fn perform_select_chapter(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index == self.selected || index >= self.chapters.len() {
            return;
        }
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source {
            if !self.flush_body(cx) {
                return;
            }
        }
        self.selected = index;
        self.selected_toc_id = None;
        self.ai_selected_text = None;
        self.sync_chapter_title_input(index, window, cx);
        match self.tab {
            EditorTab::Source => self.sync_body_input(index, window, cx),
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.sync_ai_reference(cx);
        self.notice = None;
        cx.notify();
    }

    pub(crate) fn perform_open_citation(
        &mut self,
        requested_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.pending_citation_source.take() else {
            self.set_error("引用定位状态已失效，请重新点击来源", cx);
            return;
        };
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(document) = self.canonical_document.as_ref() else {
            self.set_error("编辑器统一文档已失配，无法安全定位引用", cx);
            return;
        };
        let target = match source.current_navigation_target(document) {
            Ok(target) if target.unit_index == requested_index => target,
            Ok(_) => {
                self.set_error("引用内容单元与编辑器请求位置不匹配", cx);
                return;
            }
            Err(error) => {
                self.set_error(format!("引用已失效：{error}"), cx);
                return;
            }
        };
        if target.unit_index >= self.chapters.len() {
            self.set_error("引用对应的编辑章节已不存在", cx);
            return;
        }
        self.selected = target.unit_index;
        self.ai_selected_text = None;
        self.sync_chapter_title_input(target.unit_index, window, cx);
        self.tab = EditorTab::Preview;
        self.load_webview_for_citation(target, window, cx);
        self.sync_ai_reference(cx);
        cx.notify();
    }

    /// Copies the chapter-title input into the selected chapter.
    pub(crate) fn flush_chapter_title(&mut self, cx: &mut Context<Self>) {
        let value = self.chapter_title_input.read(cx).value().to_string();
        if let Some(chapter) = self.chapters.get_mut(self.selected) {
            chapter.title = value;
        }
    }

    /// Parses, cleans and copies the source editor into the canonical model.
    /// Invalid source never replaces the last valid AST or preview.
    pub(crate) fn flush_body(&mut self, cx: &mut Context<Self>) -> bool {
        let value = self.body_input.read(cx).value().to_string();
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            if let Some(chapter) = self.chapters.get_mut(self.selected) {
                chapter.html = value;
            }
            return true;
        };
        // Preserve the canonical AST and stable block IDs when the source
        // text has not changed.
        if value == state.source {
            return true;
        }
        let template = self
            .chapters
            .get(self.selected)
            .map(|chapter| chapter.html.as_str())
            .unwrap_or(DEFAULT_CHAPTER_HTML);
        let title = self
            .chapters
            .get(self.selected)
            .map(|chapter| chapter.title.as_str())
            .unwrap_or("章节");
        let (preview, source, blocks) =
            match preview_document_from_source(template, title, &state.id, &value) {
                Ok(parsed) => parsed,
                Err(error) => {
                    self.notice = Some(Notice {
                        text: format!("正文源码无效，尚未保存：{error:#}"),
                        error: true,
                    });
                    cx.notify();
                    return false;
                }
            };
        if let Some(unit) = self
            .canonical_document
            .as_mut()
            .and_then(|document| document.units.iter_mut().find(|unit| unit.id == state.id))
        {
            unit.source = source.clone();
            unit.document = blocks;
        }
        if let Some(unit) = self.unit_states.get_mut(self.selected) {
            unit.source = source;
        }
        if let Some(chapter) = self.chapters.get_mut(self.selected) {
            chapter.html = preview;
        }
        true
    }

    pub(crate) fn update_unit_from_rich_text(&mut self, index: usize, html: &str) -> Result<()> {
        let state = self
            .unit_states
            .get(index)
            .cloned()
            .context("当前内容单元没有稳定 ID")?;
        let parsed = parse_rich_text_snapshot(html, &state.id)?;
        if let Some(unit) = self
            .canonical_document
            .as_mut()
            .and_then(|document| document.units.iter_mut().find(|unit| unit.id == state.id))
        {
            unit.source = parsed.canonical_source.clone();
            unit.document = parsed.document;
        }
        if let Some(unit) = self.unit_states.get_mut(index) {
            unit.source = parsed.canonical_source;
        }
        Ok(())
    }
}
