//! Saving, exporting and the write job that keeps edits off the UI thread.

use super::*;

impl EditorApp {
    pub(crate) fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_action(PendingEditorAction::Save, window, cx);
    }

    pub(crate) fn export_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_write.is_some() || self.export_dialog_open {
            self.notice = Some(Notice {
                text: "已有保存或导出操作正在后台执行，请稍候。".to_string(),
                error: false,
            });
            cx.notify();
            return;
        }
        let title = self.title_input.read(cx).value().trim().to_string();
        // Do not call `show` while this entity update owns GPUI's App borrow.
        // The native save dialog pumps Windows messages and would otherwise
        // re-enter GPUI's frame callback with a conflicting mutable borrow.
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出《{title}》"))
            .set_filename(suggested_epub_filename(&title))
            .add_filter("EPUB 图书", ["epub"])
            .save_single_file();
        self.export_dialog_open = true;
        cx.spawn_in(window, async move |view, cx| {
            let target = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.export_dialog_open = false;
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.export_dialog_open = false;
                        this.notice = Some(Notice {
                            text: format!("无法打开保存文件选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.export_dialog_open = false;
                this.pending_export_path = Some(target);
                this.notice = Some(Notice {
                    text: "正在同步修改并导出 EPUB…".to_string(),
                    error: false,
                });
                this.begin_action(PendingEditorAction::Export, window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn perform_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_export_path.take() else {
            self.notice = Some(Notice {
                text: "导出失败：缺少目标文件路径".to_string(),
                error: true,
            });
            cx.notify();
            return;
        };
        self.start_editor_write(EditorWriteIntent::Export, Some(target), window, cx);
    }

    pub(crate) fn perform_save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_editor_write(EditorWriteIntent::Save, None, window, cx);
    }

    /// Freezes and validates the current UI draft. The expensive canonical EPUB
    /// generation, object-store writes, SQLite transaction and optional export
    /// are deliberately absent from this GPUI callback and run through
    /// `spawn_editor_write` on the application's dedicated runtime.
    pub(crate) fn freeze_editor_write(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(String, EditorWriteJob)> {
        let title = self.title_input.read(cx).value().trim().to_string();
        let author = self.author_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            self.notice = Some(Notice {
                text: "书名不能为空".to_string(),
                error: true,
            });
            cx.notify();
            return None;
        }
        // Refresh the selected chapter title. The body only needs flushing from
        // the source input: rich text has already crossed the exact
        // session/chapter/revision/request-id snapshot barrier above.
        self.flush_chapter_title(cx);
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return None;
        }
        let Some(document) = self.canonical_document.clone() else {
            self.notice = Some(Notice {
                text: "当前图书缺少统一文档模型，已拒绝使用旧 EPUB 草稿覆盖数据。".to_string(),
                error: true,
            });
            cx.notify();
            return None;
        };
        if document.units.len() != self.unit_states.len()
            || document.units.len() != self.chapters.len()
        {
            self.notice = Some(Notice {
                text: "编辑器内容单元与统一模型不一致，请重新打开编辑窗口。".to_string(),
                error: true,
            });
            cx.notify();
            return None;
        }

        let language = document.language.clone();
        let description = document.description.clone();
        let mut editor = match DocumentEditor::new(document) {
            Ok(editor) => editor,
            Err(error) => {
                self.set_error(format!("无法验证统一文档模型：{error:#}"), cx);
                return None;
            }
        };
        if let Err(error) = editor.set_metadata(
            title.clone(),
            (!author.is_empty()).then_some(author).into_iter().collect(),
            language,
            description,
        ) {
            self.set_error(format!("无法更新图书元数据：{error:#}"), cx);
            return None;
        }
        for (chapter, state) in self.chapters.iter().zip(&self.unit_states) {
            if let Err(error) =
                editor.update_unit_identity(&state.id, chapter.title.clone(), state.kind)
            {
                self.set_error(format!("无法更新内容单元：{error:#}"), cx);
                return None;
            }
        }
        let mut document = editor.into_document();
        let mut new_asset_bytes = self.pending_asset_bytes.clone();
        if self.cover_dirty {
            clear_editor_cover_assignment(&mut document);
            if let Some(cover) = self.cover.clone() {
                let mut asset = AssetRef::from_bytes(
                    AssetRole::Cover,
                    cover.mime(),
                    None,
                    cover.bytes().as_slice(),
                );
                asset.add_role(AssetRole::ContentImage);
                let asset_id = asset.id.clone();
                if let Some(existing) = document
                    .assets
                    .iter_mut()
                    .find(|existing| existing.id == asset.id)
                {
                    existing.add_role(AssetRole::Cover);
                    existing.add_role(AssetRole::ContentImage);
                } else {
                    document.assets.push(asset);
                }
                document.cover_asset_id = Some(asset_id.clone());
                new_asset_bytes.insert(asset_id, cover.bytes().clone());
            }
        }
        prune_editor_assets(&mut document);
        retain_referenced_pending_assets(&mut new_asset_bytes, &document);
        if let Err(error) = document.validate() {
            self.set_error(format!("编辑后的图书结构无效：{error}"), cx);
            return None;
        }
        Some((
            title,
            EditorWriteJob {
                document,
                new_asset_bytes,
                export_target: None,
            },
        ))
    }

    pub(crate) fn start_editor_write(
        &mut self,
        intent: EditorWriteIntent,
        export_target: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_write.is_some() {
            self.begin_action(
                if intent == EditorWriteIntent::Close {
                    PendingEditorAction::Close
                } else {
                    PendingEditorAction::Save
                },
                window,
                cx,
            );
            return;
        }
        let Some((title, mut job)) = self.freeze_editor_write(cx) else {
            if intent == EditorWriteIntent::Close {
                cancel_application_exit(cx);
            }
            return;
        };
        let target_display = export_target
            .as_ref()
            .map(|target| target.display().to_string());
        job.export_target = export_target;
        self.write_sequence = self.write_sequence.wrapping_add(1).max(1);
        let operation = ActiveEditorWrite {
            id: self.write_sequence,
            intent,
            draft_generation: self.draft_generation,
            title,
            target_display,
        };
        let operation_id = operation.id;
        self.active_write = Some(operation);
        self.close_after_write = intent == EditorWriteIntent::Close;
        self.set_rich_text_write_locked(true, cx);
        self.notice = Some(Notice {
            text: match intent {
                EditorWriteIntent::Save => "正在后台生成并保存 EPUB…",
                EditorWriteIntent::Export => "正在后台保存并导出 EPUB…",
                EditorWriteIntent::Close => "正在后台保存，成功后将安全关闭…",
            }
            .to_string(),
            error: false,
        });
        cx.notify();

        let task = spawn_editor_write(&self.services, job);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.finish_editor_write(operation_id, outcome, window, cx);
                });
            });
        })
        .detach();
    }

    pub(crate) fn finish_editor_write(
        &mut self,
        operation_id: u64,
        outcome: std::result::Result<
            Result<LibraryMutation<EditorWriteWorkerResult>>,
            tokio::task::JoinError,
        >,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_write.as_ref().map(|operation| operation.id) != Some(operation_id) {
            return;
        }
        let operation = self
            .active_write
            .take()
            .expect("matching editor write operation exists");
        let close_requested = std::mem::take(&mut self.close_after_write);
        match outcome {
            Ok(Ok(mutation)) => {
                let LibraryMutation {
                    value: result,
                    snapshot,
                    generation,
                } = mutation;
                let export_succeeded = result.export.succeeded();
                if !self.apply_persisted_editor_state(
                    result.document,
                    result.cover_bytes,
                    generation,
                    snapshot,
                    cx,
                ) {
                    if close_requested || operation.intent == EditorWriteIntent::Close {
                        cancel_application_exit(cx);
                    }
                    self.set_rich_text_write_locked(false, cx);
                    self.resume_close_confirmation(window, cx);
                    return;
                }
                // The persisted state is now authoritative for every chapter;
                // the in-memory dirty flags are reset in lockstep.
                self.modified_chapter_ids.clear();
                match &result.export {
                    EditorExportOutcome::NotRequested => {
                        self.notice = Some(Notice {
                            text: "已保存统一文档、目录和媒体修改。".to_string(),
                            error: false,
                        });
                    }
                    EditorExportOutcome::Exported(bytes_written) => {
                        let target = operation.target_display.as_deref().unwrap_or("目标文件");
                        self.notice = Some(Notice {
                            text: format!(
                                "《{}》已保存并导出到「{target}」（{} 字节）",
                                operation.title, bytes_written
                            ),
                            error: false,
                        });
                    }
                    EditorExportOutcome::Failed(error) => {
                        self.notice = Some(Notice {
                            text: format!("已保存修改，但导出《{}》失败：{error}", operation.title),
                            error: true,
                        });
                    }
                }
                match editor_write_follow_up(
                    &operation,
                    close_requested,
                    self.draft_generation,
                    true,
                    export_succeeded,
                ) {
                    EditorWriteFollowUp::StayOpen => {
                        if close_requested || operation.intent == EditorWriteIntent::Close {
                            cancel_application_exit(cx);
                        }
                        self.set_rich_text_write_locked(false, cx);
                        cx.notify();
                    }
                    EditorWriteFollowUp::Close => self.schedule_window_removal(window, cx),
                    EditorWriteFollowUp::SaveThenClose => {
                        self.set_rich_text_write_locked(false, cx);
                        self.notice = Some(Notice {
                            text: "保存期间草稿又发生变化，正在重新同步后关闭。".to_string(),
                            error: false,
                        });
                        self.begin_action(PendingEditorAction::Close, window, cx);
                    }
                }
            }
            Ok(Err(error)) => {
                if close_requested || operation.intent == EditorWriteIntent::Close {
                    cancel_application_exit(cx);
                }
                self.set_rich_text_write_locked(false, cx);
                self.notice = Some(Notice {
                    text: format!("保存失败，编辑器保持打开：{error:#}"),
                    error: true,
                });
                debug_assert_eq!(
                    editor_write_follow_up(
                        &operation,
                        close_requested,
                        self.draft_generation,
                        false,
                        false,
                    ),
                    EditorWriteFollowUp::StayOpen
                );
                cx.notify();
            }
            Err(error) => {
                if close_requested || operation.intent == EditorWriteIntent::Close {
                    cancel_application_exit(cx);
                }
                self.set_rich_text_write_locked(false, cx);
                self.notice = Some(Notice {
                    text: format!("保存任务异常停止，编辑器保持打开：{error}"),
                    error: true,
                });
                cx.notify();
            }
        }
        self.resume_close_confirmation(window, cx);
    }

    pub(crate) fn apply_persisted_editor_state(
        &mut self,
        document: BookDocument,
        cover_bytes: Option<Arc<Vec<u8>>>,
        generation: u64,
        snapshot: LibraryStore,
        cx: &mut Context<Self>,
    ) -> bool {
        if apply_editor_library_projection(
            &mut self.library,
            &mut self.library_projection_generation,
            generation,
            snapshot.clone(),
        ) {
            self.sync_ai_scope(cx);
        }
        let _ = self.library_view.update(cx, |library, cx| {
            library.refresh_after_projected_mutation(generation, snapshot, cx);
        });
        self.cover = cover_bytes.and_then(|bytes| CoverDraft::from_arc(bytes).ok());
        self.cover_preview = self.cover.as_ref().and_then(|cover| {
            image_format_from_mime(cover.mime())
                .map(|format| Arc::new(Image::from_bytes(format, (**cover.bytes()).clone())))
        });
        self.cover_dirty = false;
        self.pending_asset_bytes.clear();
        if document.units.len() != self.chapters.len() {
            tracing::warn!(
                book_id = self.book_id,
                canonical_units = document.units.len(),
                editor_chapters = self.chapters.len(),
                "saved canonical document no longer aligns with editor projection"
            );
            self.canonical_document = Some(document);
            self.unit_states.clear();
            self.notice = Some(Notice {
                text: "图书已保存，但编辑器内容单元已失配；请重新打开编辑窗口。".to_string(),
                error: true,
            });
            cx.notify();
            return false;
        }
        if let Err(error) = self.web_state.authorize_media(&document) {
            tracing::warn!(book_id = self.book_id, %error, "cannot refresh saved media authorization");
            self.canonical_document = Some(document);
            self.unit_states.clear();
            self.notice = Some(Notice {
                text: "图书已保存，但无法刷新媒体授权；请重新打开编辑窗口。".to_string(),
                error: true,
            });
            cx.notify();
            return false;
        }
        self.unit_states = EditorUnitState::from_document(&document);
        self.canonical_document = Some(document);
        for (index, chapter) in self.chapters.iter_mut().enumerate() {
            chapter.spine_index = Some(index);
            if let Some(unit) = self.unit_states.get(index) {
                chapter.title = self
                    .canonical_document
                    .as_ref()
                    .and_then(|document| document.units.iter().find(|item| item.id == unit.id))
                    .map(|unit| unit.title.clone())
                    .unwrap_or_else(|| chapter.title.clone());
            }
        }
        true
    }

    pub(crate) fn set_rich_text_write_locked(&self, locked: bool, cx: &mut Context<Self>) {
        let Some(webview) = self.editor_webview.as_ref() else {
            return;
        };
        let script = if locked {
            r#"document.activeElement && document.activeElement.blur(); document.querySelector('.ProseMirror')?.setAttribute('contenteditable','false'); document.querySelectorAll('[data-ngy-editor-ui="toolbar"] button').forEach((button) => button.disabled = true);"#
        } else {
            r#"document.querySelector('.ProseMirror')?.setAttribute('contenteditable','true'); document.querySelectorAll('[data-ngy-editor-ui="toolbar"] button').forEach((button) => button.disabled = false);"#
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(script) {
            tracing::warn!(locked, %error, "cannot update rich-text write lock");
        }
    }
}
