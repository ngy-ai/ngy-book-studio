//! Inserting, replacing and deleting media, and the cover commands.

use super::*;

impl EditorApp {
    pub(crate) fn insert_media(
        &mut self,
        kind: MediaKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = None;
        self.begin_action(PendingEditorAction::InsertMedia(kind), window, cx);
    }

    pub(crate) fn replace_media(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = Some(target);
        self.begin_action(PendingEditorAction::ReplaceMedia, window, cx);
    }

    pub(crate) fn edit_media_metadata(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = Some(target);
        self.begin_action(PendingEditorAction::EditMediaMetadata, window, cx);
    }

    pub(crate) fn request_delete_media(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media_loading || self.media_modal.is_some() {
            return;
        }
        self.media_target = Some(target);
        self.begin_action(PendingEditorAction::DeleteMedia, window, cx);
    }

    pub(crate) fn choose_media_file(
        &mut self,
        kind: MediaKind,
        replace_target: Option<EditorMediaBlock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, label, extensions): (&str, &str, &[&str]) = match kind {
            MediaKind::Image => (
                "选择正文图片",
                "图片",
                &["jpg", "jpeg", "png", "gif", "webp"],
            ),
            MediaKind::Audio => (
                "选择音频",
                "音频",
                &["mp3", "m4a", "aac", "ogg", "wav", "flac"],
            ),
            MediaKind::Video => ("选择视频", "视频", &["mp4", "webm", "ogv", "mov"]),
        };
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(title)
            .add_filter(label, extensions)
            .open_single_file();
        self.media_loading = true;
        self.notice = Some(Notice {
            text: format!("正在读取{label}…"),
            error: false,
        });
        let book_id = self.book_id.clone();
        cx.spawn_in(window, async move |view, cx| {
            let path = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = view.update_in(cx, |this, window, cx| {
                        this.media_loading = false;
                        this.media_target = None;
                        if let Some(action) = this.pending_media_action.take() {
                            // A cancelled insertion has no new state to save,
                            // but a previously requested save/close still runs.
                            this.begin_action(action, window, cx);
                        } else {
                            cx.notify();
                        }
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update_in(cx, |this, window, cx| {
                        this.media_loading = false;
                        this.media_target = None;
                        this.set_error(format!("无法打开媒体文件选择器：{error}"), cx);
                        if let Some(action) = this.pending_media_action.take() {
                            this.begin_action(action, window, cx);
                        }
                    });
                    return;
                }
            };
            let task = cx.background_spawn(async move {
                let bytes = read_editor_media_file(&path, kind)?;
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
                let media_type = mime_guess::from_path(&path)
                    .first_raw()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let title = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or(label)
                    .to_string();
                MediaDraft::from_bytes(kind, media_type, file_name, bytes)
                    .map(|draft| PreparedEditorMedia::new(&book_id, draft.with_title(title)))
            });
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.media_loading = false;
                this.media_target = None;
                match result {
                    Ok(media) => {
                        let initial_title = replace_target
                            .as_ref()
                            .and_then(|target| target.title.clone())
                            .or_else(|| media.draft.title.clone());
                        let initial_description = replace_target
                            .as_ref()
                            .and_then(|target| target.description.clone())
                            .or_else(|| media.draft.description.clone());
                        this.open_media_metadata_modal(
                            EditorMediaModalAction::InsertOrReplace {
                                media,
                                replace_block_id: replace_target
                                    .as_ref()
                                    .map(|target| target.id.clone()),
                            },
                            kind,
                            initial_title.as_deref().unwrap_or_default(),
                            initial_description.as_deref().unwrap_or_default(),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        this.set_error(format!("无法使用所选媒体：{error:#}"), cx);
                        if let Some(action) = this.pending_media_action.take() {
                            this.begin_action(action, window, cx);
                        }
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn open_media_metadata_editor(
        &mut self,
        target: EditorMediaBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = target.title.clone().unwrap_or_default();
        let description = target.description.clone().unwrap_or_default();
        let kind = target.kind;
        self.open_media_metadata_modal(
            EditorMediaModalAction::Edit { target },
            kind,
            &title,
            &description,
            window,
            cx,
        );
    }

    pub(crate) fn open_media_metadata_modal(
        &mut self,
        action: EditorMediaModalAction,
        kind: MediaKind,
        title: &str,
        description: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("可选，例如：人物关系图")
                .default_value(title.to_string())
        });
        let description_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder("可选；将进入正文纯文本、全文检索与 AI 引用")
                .default_value(description.to_string())
        });
        title_input.update(cx, |state, cx| state.focus(window, cx));
        self.media_modal = Some(EditorMediaModal {
            action,
            kind,
            title_input,
            description_input,
        });
        self.notice = Some(Notice {
            text: "请填写媒体标题与说明。".to_string(),
            error: false,
        });
        cx.notify();
    }

    pub(crate) fn close_media_metadata_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.media_modal = None;
        if let Some(action) = self.pending_media_action.take() {
            self.begin_action(action, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn confirm_media_metadata_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.media_modal.clone() else {
            return;
        };
        let metadata = match EditorMediaMetadataDraft::from_inputs(
            &modal.title_input.read(cx).value(),
            &modal.description_input.read(cx).value(),
        ) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.set_error(format!("媒体信息无效：{error}"), cx);
                return;
            }
        };
        let result = match modal.action {
            EditorMediaModalAction::InsertOrReplace {
                mut media,
                replace_block_id,
            } => {
                media.draft.title = metadata.title;
                media.draft.description = metadata.description;
                self.apply_media_draft(media, replace_block_id.as_deref(), window, cx)
            }
            EditorMediaModalAction::Edit { target } => {
                self.apply_media_metadata(&target.id, &metadata, window, cx)
            }
        };
        if let Err(error) = result {
            self.set_error(format!("无法更新媒体信息：{error:#}"), cx);
            return;
        }
        self.media_modal = None;
        if let Some(action) = self.pending_media_action.take() {
            self.begin_action(action, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn apply_media_draft(
        &mut self,
        media: PreparedEditorMedia,
        replace_block_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let PreparedEditorMedia { draft, asset } = media;
        let metadata = EditorMediaMetadataDraft {
            title: draft.title.clone(),
            description: draft.description.clone(),
        };
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            anyhow::bail!("正文源码尚未通过解析");
        }
        let state = self
            .unit_states
            .get(self.selected)
            .cloned()
            .context("当前内容单元不存在")?;
        let document = self
            .canonical_document
            .as_mut()
            .context("当前图书缺少统一文档模型")?;
        let asset_id = asset.id.clone();
        if let Some(existing) = document.assets.iter_mut().find(|item| item.id == asset.id) {
            for role in &asset.roles {
                existing.add_role(*role);
            }
        } else {
            document.assets.push(asset.clone());
        }
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == state.id)
            .context("当前内容单元不存在")?;
        if let Some(block_id) = replace_block_id {
            anyhow::ensure!(
                replace_media_asset(&mut unit.document.blocks, block_id, &asset_id),
                "媒体块不存在"
            );
            anyhow::ensure!(
                update_media_metadata(&mut unit.document.blocks, block_id, &metadata),
                "媒体块信息不存在"
            );
        } else {
            let block_id = deterministic_id(
                "block",
                format!(
                    "{}\0{}\0{}\0{}",
                    unit.id,
                    unit.revision.get(),
                    asset_id,
                    unit.document.blocks.len()
                )
                .as_bytes(),
            );
            let block = match draft.kind {
                MediaKind::Image => Block::Image {
                    id: block_id,
                    asset_id: asset_id.clone(),
                    alt: metadata.description.clone().unwrap_or_default(),
                    title: metadata.title.clone(),
                    caption: editor_caption_from_description(metadata.description.as_deref()),
                },
                MediaKind::Audio => Block::Audio {
                    id: block_id,
                    asset_id: asset_id.clone(),
                    title: metadata.title.clone(),
                    caption: editor_caption_from_description(metadata.description.as_deref()),
                },
                MediaKind::Video => Block::Video {
                    id: block_id,
                    asset_id: asset_id.clone(),
                    poster_asset_id: None,
                    title: metadata.title.clone(),
                    caption: editor_caption_from_description(metadata.description.as_deref()),
                },
            };
            unit.document.blocks.push(block);
        }
        unit.source = serialize_source(&unit.document)?;
        self.pending_asset_bytes
            .insert(asset_id, draft.bytes().clone());
        prune_editor_assets(document);
        retain_referenced_pending_assets(&mut self.pending_asset_bytes, document);
        document.validate()?;
        self.web_state
            .sync_media(document, Some((&asset, draft.bytes().clone())))?;
        self.sync_unit_state_from_document(self.selected);
        self.refresh_selected_editor_views(window, cx)?;
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.sync_ai_reference(cx);
        if !self.search_query.is_empty() {
            self.refresh_editor_search(cx);
        }
        self.notice = Some(Notice {
            text: if replace_block_id.is_some() {
                "已替换媒体，保存后写入对象存储。".to_string()
            } else {
                "已插入媒体，保存后写入对象存储。".to_string()
            },
            error: false,
        });
        cx.notify();
        Ok(())
    }

    pub(crate) fn apply_media_metadata(
        &mut self,
        block_id: &str,
        metadata: &EditorMediaMetadataDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            anyhow::bail!("正文源码尚未通过解析");
        }
        let state = self
            .unit_states
            .get(self.selected)
            .cloned()
            .context("当前内容单元不存在")?;
        let document = self
            .canonical_document
            .as_mut()
            .context("当前图书缺少统一文档模型")?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == state.id)
            .context("当前内容单元不存在")?;
        anyhow::ensure!(
            update_media_metadata(&mut unit.document.blocks, block_id, metadata),
            "媒体块不存在"
        );
        unit.source = serialize_source(&unit.document)?;
        document.validate()?;
        self.sync_unit_state_from_document(self.selected);
        self.refresh_selected_editor_views(window, cx)?;
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.sync_ai_reference(cx);
        if !self.search_query.is_empty() {
            self.refresh_editor_search(cx);
        }
        self.notice = Some(Notice {
            text: "已更新媒体标题与说明，保存后进入全文检索。".to_string(),
            error: false,
        });
        cx.notify();
        Ok(())
    }

    pub(crate) fn perform_delete_media(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.media_target.take() else {
            return;
        };
        if self.tab == EditorTab::Source && !self.flush_body(cx) {
            return;
        }
        let Some(state) = self.unit_states.get(self.selected).cloned() else {
            return;
        };
        let Some(document) = self.canonical_document.as_mut() else {
            return;
        };
        let Some(unit) = document.units.iter_mut().find(|unit| unit.id == state.id) else {
            return;
        };
        if !delete_media(&mut unit.document.blocks, &target.id) {
            self.set_error("媒体块不存在", cx);
            return;
        }
        match serialize_source(&unit.document) {
            Ok(source) => unit.source = source,
            Err(error) => {
                self.set_error(format!("无法更新正文源码：{error:#}"), cx);
                return;
            }
        }
        prune_editor_assets(document);
        retain_referenced_pending_assets(&mut self.pending_asset_bytes, document);
        if let Err(error) = self.web_state.sync_media(document, None) {
            self.set_error(format!("无法更新媒体预览：{error:#}"), cx);
            return;
        }
        self.sync_unit_state_from_document(self.selected);
        if let Err(error) = self.refresh_selected_editor_views(window, cx) {
            self.set_error(format!("无法刷新编辑视图：{error:#}"), cx);
            return;
        }
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.sync_ai_reference(cx);
        if !self.search_query.is_empty() {
            self.refresh_editor_search(cx);
        }
        self.notice = Some(Notice {
            text: "已删除媒体引用，保存后异步回收无引用对象。".to_string(),
            error: false,
        });
        cx.notify();
    }

    pub(crate) fn sync_unit_state_from_document(&mut self, index: usize) {
        let Some(state_id) = self.unit_states.get(index).map(|state| state.id.clone()) else {
            return;
        };
        let Some(unit) = self
            .canonical_document
            .as_ref()
            .and_then(|document| document.units.iter().find(|unit| unit.id == state_id))
        else {
            return;
        };
        if let Some(state) = self.unit_states.get_mut(index) {
            state.kind = unit.kind;
            state.source = unit.source.clone();
        }
    }

    pub(crate) fn refresh_selected_editor_views(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let state = self
            .unit_states
            .get(self.selected)
            .cloned()
            .context("当前内容单元不存在")?;
        let chapter = self
            .chapters
            .get(self.selected)
            .cloned()
            .context("当前章节不存在")?;
        let (preview, _, _) =
            preview_document_from_source(&chapter.html, &chapter.title, &state.id, &state.source)?;
        self.chapters[self.selected].html = preview;
        match self.tab {
            EditorTab::Source => self.sync_body_input(self.selected, window, cx),
            EditorTab::Preview | EditorTab::RichText => self.load_webview(self.tab, window, cx),
        }
        Ok(())
    }

    pub(crate) fn change_cover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cover_loading
            || self.canonical_document.is_none()
            || self.media_loading
            || self.media_modal.is_some()
            || self.active_write.is_some()
            || self.export_dialog_open
            || self.webview_build_gate.building
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing
        {
            return;
        }
        // Construct the native dialog while the GPUI window is available, but
        // defer `show` until this entity update has returned. On Windows the
        // dialog runs a nested message loop; showing it while GPUI holds its
        // app borrow makes every requested frame fail with a BorrowMutError.
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title("选择封面图片")
            .add_filter("封面图片", ["jpg", "jpeg", "png", "gif"])
            .open_single_file();

        self.cover_loading = true;
        cx.spawn_in(window, async move |view, cx| {
            let path = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = view.update_in(cx, |this, window, cx| {
                        this.cover_loading = false;
                        if let Some(action) = this.pending_cover_action.take() {
                            this.begin_action(action, window, cx);
                        } else {
                            cx.notify();
                        }
                    });
                    return;
                }
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.cover_loading = false;
                        let pending_action = this.pending_cover_action.take();
                        if pending_action == Some(PendingEditorAction::Close) {
                            cancel_application_exit(cx);
                        }
                        if pending_action == Some(PendingEditorAction::Export) {
                            this.pending_export_path = None;
                        }
                        this.notice = Some(Notice {
                            text: format!("无法打开封面文件选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    });
                    return;
                }
            };

            let task = cx.background_spawn(async move { CoverDraft::read(&path) });
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.cover_loading = false;
                let pending_action = this.pending_cover_action.take();
                match result {
                    Ok(cover) => {
                        this.cover_preview = image_format_from_mime(cover.mime()).map(|format| {
                            Arc::new(Image::from_bytes(format, (**cover.bytes()).clone()))
                        });
                        this.cover = Some(cover);
                        this.cover_dirty = true;
                        this.draft_generation = this.draft_generation.wrapping_add(1);
                        this.notice = Some(Notice {
                            text: "已选择新封面，保存后生效".to_string(),
                            error: false,
                        });
                        if let Some(action) = pending_action {
                            this.begin_action(action, window, cx);
                            return;
                        }
                    }
                    Err(error) => {
                        if pending_action == Some(PendingEditorAction::Close) {
                            cancel_application_exit(cx);
                        }
                        if pending_action == Some(PendingEditorAction::Export) {
                            this.pending_export_path = None;
                        }
                        this.notice = Some(Notice {
                            text: format!("无法使用所选封面：{error:#}"),
                            error: true,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn clear_cover(&mut self, cx: &mut Context<Self>) {
        if self.cover.is_none()
            || self.canonical_document.is_none()
            || self.cover_loading
            || self.media_loading
            || self.media_modal.is_some()
            || self.active_write.is_some()
            || self.export_dialog_open
            || self.webview_build_gate.building
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing
        {
            return;
        }
        self.cover = None;
        self.cover_preview = None;
        self.cover_dirty = true;
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.notice = Some(Notice {
            text: "已清除封面，保存后生效".to_string(),
            error: false,
        });
        cx.notify();
    }
}
