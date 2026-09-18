//! Adding, removing, moving and re-typing chapters and TOC entries.

use super::*;

impl EditorApp {
    pub(crate) fn add_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::AddChapter, window, cx);
    }

    pub(crate) fn perform_add_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(document) = self.canonical_document.clone() else {
            self.set_error("当前图书缺少统一文档模型，无法安全新增章节", cx);
            return;
        };
        let new_position = document.units.len();
        let title = format!("新章节 {}", new_position + 1);
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑图书结构：{error:#}"), cx);
                return;
            }
        };
        let unit_id =
            match editor.add_unit(NewContentUnit::html_chapter(title.clone(), new_position)) {
                Ok(unit_id) => unit_id,
                Err(error) => {
                    self.set_error(format!("无法新增章节：{error:#}"), cx);
                    return;
                }
            };
        let next_document = editor.into_document();
        let Some(unit) = next_document.units.iter().find(|unit| unit.id == unit_id) else {
            self.set_error("新增章节没有生成稳定内容单元", cx);
            return;
        };

        // Pick a URL that does not collide with the protocol resources. This
        // href is only a view identity; persistence uses the stable unit ID.
        let mut index = self.chapters.len() + 1;
        let mut href = format!("chapter-{index}.xhtml");
        while self
            .chapters
            .iter()
            .any(|chapter| editor_hrefs_match(&chapter.href, &href))
        {
            index += 1;
            href = format!("chapter-{index}.xhtml");
        }
        let preview =
            preview_document_from_source(DEFAULT_CHAPTER_HTML, &title, &unit.id, &unit.source)
                .map(|(preview, _, _)| preview)
                .unwrap_or_else(|_| DEFAULT_CHAPTER_HTML.to_string());
        self.chapters.push(EditorChapter {
            title,
            href,
            spine_index: None,
            html: preview,
        });
        self.unit_states.push(EditorUnitState {
            id: unit.id.clone(),
            kind: unit.kind,
            source: unit.source.clone(),
        });
        self.canonical_document = Some(next_document);
        let new_index = self.chapters.len() - 1;
        self.selected = new_index;
        self.sync_chapter_title_input(new_index, window, cx);
        match self.tab {
            EditorTab::Source => self.sync_body_input(new_index, window, cx),
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.sync_ai_reference(cx);
        self.notice = None;
        if self.search_query.is_empty() {
            cx.notify();
        } else {
            self.refresh_editor_search(cx);
        }
    }

    pub(crate) fn remove_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chapters.is_empty() {
            return;
        }
        self.begin_action(PendingEditorAction::RemoveChapter, window, cx);
    }

    pub(crate) fn perform_remove_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            self.set_error("当前章节缺少稳定内容单元，无法安全删除", cx);
            return;
        };
        let Some(document) = self.canonical_document.clone() else {
            self.set_error("当前图书缺少统一文档模型，无法安全删除章节", cx);
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑图书结构：{error:#}"), cx);
                return;
            }
        };
        if let Err(error) = editor.remove_unit(&state.id) {
            self.set_error(format!("无法删除章节：{error:#}"), cx);
            return;
        }
        self.canonical_document = Some(editor.into_document());
        self.chapters.remove(self.selected);
        self.unit_states.remove(self.selected);
        self.modified_chapter_ids.remove(&state.id);
        if self.selected >= self.chapters.len() {
            self.selected = self.chapters.len().saturating_sub(1);
        }
        self.sync_chapter_title_input(self.selected, window, cx);
        match self.tab {
            EditorTab::Source => self.sync_body_input(self.selected, window, cx),
            EditorTab::Preview => self.load_webview(EditorTab::Preview, window, cx),
            EditorTab::RichText => self.load_webview(EditorTab::RichText, window, cx),
        }
        self.sync_ai_reference(cx);
        if self.search_query.is_empty() {
            cx.notify();
        } else {
            self.refresh_editor_search(cx);
        }
    }

    pub(crate) fn move_chapter(
        &mut self,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (offset < 0 && self.selected == 0)
            || (offset > 0 && self.selected + 1 >= self.chapters.len())
        {
            return;
        }
        self.begin_action(
            if offset < 0 {
                PendingEditorAction::MoveChapterUp
            } else {
                PendingEditorAction::MoveChapterDown
            },
            window,
            cx,
        );
    }

    pub(crate) fn perform_move_chapter(
        &mut self,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let destination = self.selected.saturating_add_signed(offset);
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑图书结构：{error:#}"), cx);
                return;
            }
        };
        if let Err(error) = editor.move_unit(&state.id, destination) {
            self.set_error(format!("无法移动内容单元：{error:#}"), cx);
            return;
        }
        self.canonical_document = Some(editor.into_document());
        let chapter = self.chapters.remove(self.selected);
        self.chapters.insert(destination, chapter);
        let state = self.unit_states.remove(self.selected);
        self.unit_states.insert(destination, state);
        self.selected = destination;
        self.sync_chapter_title_input(destination, window, cx);
        if self.tab == EditorTab::Source {
            self.sync_body_input(destination, window, cx);
        } else {
            self.load_webview(self.tab, window, cx);
        }
        self.sync_ai_reference(cx);
        self.notice = Some(Notice {
            text: "已调整线性阅读顺序，保存后生效。".to_string(),
            error: false,
        });
        cx.notify();
    }

    pub(crate) fn indent_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::IndentToc, window, cx);
    }

    pub(crate) fn perform_indent_toc(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let Some(toc_id) = self.current_toc_id() else {
            self.set_error("请先选择要调整的目录项", cx);
            return;
        };
        let Some((parent_id, position)) = toc_indent_destination(&document.toc, &toc_id) else {
            self.set_error("当前目录项没有可作为父目录的前一项", cx);
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑目录：{error:#}"), cx);
                return;
            }
        };
        match editor.move_toc_node(&toc_id, Some(&parent_id), position) {
            Ok(()) => {
                self.canonical_document = Some(editor.into_document());
                self.selected_toc_id = Some(toc_id);
                self.notice = Some(Notice {
                    text: "已将本项缩进为上一项的子目录，保存后生效。".to_string(),
                    error: false,
                });
                cx.notify();
            }
            Err(error) => self.set_error(format!("无法缩进目录：{error:#}"), cx),
        }
    }

    pub(crate) fn outdent_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::OutdentToc, window, cx);
    }

    pub(crate) fn perform_outdent_toc(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let Some(toc_id) = self.current_toc_id() else {
            return;
        };
        let Some((parent_id, position)) = toc_outdent_destination(&document.toc, &toc_id) else {
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑目录：{error:#}"), cx);
                return;
            }
        };
        match editor.move_toc_node(&toc_id, parent_id.as_deref(), position) {
            Ok(()) => {
                self.canonical_document = Some(editor.into_document());
                self.selected_toc_id = Some(toc_id);
                self.notice = Some(Notice {
                    text: "已将当前目录项提升一级，保存后生效。".to_string(),
                    error: false,
                });
                cx.notify();
            }
            Err(error) => self.set_error(format!("无法提升目录：{error:#}"), cx),
        }
    }

    pub(crate) fn cycle_unit_kind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::CycleUnitKind, window, cx);
    }

    pub(crate) fn perform_cycle_unit_kind(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        let kind = next_unit_kind(state.kind);
        let title = self.chapter_title_input.read(cx).value().trim().to_string();
        let Some(document) = self.canonical_document.clone() else {
            return;
        };
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法编辑内容单元：{error:#}"), cx);
                return;
            }
        };
        if let Err(error) = editor.update_unit_identity(&state.id, title, kind) {
            self.set_error(format!("无法修改内容类型：{error:#}"), cx);
            return;
        }
        self.canonical_document = Some(editor.into_document());
        if let Some(unit) = self.unit_states.get_mut(self.selected) {
            unit.kind = kind;
        }
        self.notice = Some(Notice {
            text: format!("内容类型已改为{}，保存后生效。", unit_kind_label(kind)),
            error: false,
        });
        cx.notify();
    }
}
