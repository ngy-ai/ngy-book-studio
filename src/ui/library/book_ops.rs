//! Creating, exporting and opening books from the grid.

use super::*;

impl EpubReaderApp {
    pub(super) fn create_book(&mut self, title: String, cx: &mut Context<Self>) {
        if self.is_importing {
            return;
        }
        self.is_importing = true;
        let Some(request_id) = self.begin_library_request("正在创建可编辑图书…", cx)
        else {
            self.is_importing = false;
            return;
        };
        if let Some(GroupModal::NameInput {
            pending_request,
            error,
            ..
        }) = self.group_modal.as_mut()
        {
            *pending_request = Some(request_id);
            *error = None;
        }
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| library.create_book(&title, ""));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                this.is_importing = false;
                match outcome {
                    Ok(Ok(mutation)) => {
                        let title = mutation.value.title.clone();
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("《{title}》已创建，可从右键菜单进入编辑器"),
                                error: false,
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        let message = format!("新建图书失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("新建图书任务异常停止：{error}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn export_book(
        &mut self,
        book_id: String,
        kind: LibraryExportKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind != LibraryExportKind::Original {
            self.open_export_dialog(book_id, kind, None, window, cx);
            return;
        }

        let library = self.library.clone();
        let lookup_id = book_id.clone();
        self.notice = Some(Notice {
            text: "正在读取原件信息…".to_string(),
            error: false,
        });
        let task = cx.background_spawn(async move {
            let document = library.document(&lookup_id)?;
            canonical_original_extension(&document).map(str::to_owned)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(extension) => {
                        this.notice = None;
                        this.open_export_dialog(book_id, kind, Some(extension), window, cx);
                    }
                    Err(error) => this.set_error(format!("无法导出原件：{error:#}"), cx),
                });
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_export_dialog(
        &mut self,
        book_id: String,
        kind: LibraryExportKind,
        original_extension: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        let selection = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出《{}》{}", book.title, kind.label()))
            .set_filename(suggested_library_export_filename(
                &book,
                kind,
                original_extension.as_deref(),
            ))
            .add_filter(
                kind.label(),
                [export_extension(kind, original_extension.as_deref())],
            )
            .save_single_file();
        let title = book.title;
        let library = self.library.clone();
        // As with import, `show` must run outside the entity update that
        // created the dialog so its nested Windows message loop can draw GPUI.
        cx.spawn_in(window, async move |view, cx| {
            let target = match selection.show() {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        this.notice = Some(Notice {
                            text: format!("无法打开保存文件选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let target_display = target.display().to_string();
            let _ = view.update_in(cx, |this, _window, cx| {
                this.notice = Some(Notice {
                    text: format!("正在导出《{title}》…"),
                    error: false,
                });
                cx.notify();
            });
            let task =
                cx.background_spawn(
                    async move { library.export_as(&book_id, kind.format(), &target) },
                );
            let outcome = task.await;
            let _ = view.update_in(cx, |this, _window, cx| {
                match outcome {
                    Ok(_) => {
                        this.notice = Some(Notice {
                            text: format!("《{title}》已导出到「{target_display}」"),
                            error: false,
                        });
                    }
                    Err(error) => {
                        this.set_error(format!("导出《{title}》失败：{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_book(
        &mut self,
        book_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_at(book_id, None, window, cx);
    }

    pub(super) fn open_search_hit(
        &mut self,
        hit: SearchHit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_at(hit.book_id, hit.spine_index, window, cx);
    }
}
