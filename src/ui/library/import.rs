//! Importing a source file into the library.

use super::*;

impl EpubReaderApp {
    pub(super) fn import_book(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_importing {
            return;
        }
        // Construct the native dialog while the GPUI window is available, but
        // show it only after this entity update has returned. Windows file
        // dialogs run a nested message loop; showing one while GPUI owns its
        // App borrow makes every requested frame fail with BorrowMutError.
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title("导入图书或文档")
            .add_filter(
                "支持的图书与文档",
                [
                    "epub", "pdf", "doc", "docx", "pptx", "xlsx", "mobi", "azw", "azw3", "kfx",
                    "djvu",
                ],
            )
            .open_single_file();
        self.is_importing = true;
        let Some(request_id) = self.begin_library_request("请选择要导入的图书或文档…", cx)
        else {
            self.is_importing = false;
            return;
        };
        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            let path = match dialog.show() {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let close_window = view.update_in(cx, |this, _window, cx| {
                        this.is_importing = false;
                        if this.library_notice_request == Some(request_id) {
                            this.library_notice_request = None;
                            this.notice = None;
                        }
                        this.finish_library_request(cx)
                    });
                    if let Ok(Some(window)) = close_window {
                        schedule_library_window_removal(window, cx);
                    }
                    return;
                }
                Err(error) => {
                    let close_window = view.update_in(cx, |this, _window, cx| {
                        this.is_importing = false;
                        this.set_library_request_error(
                            request_id,
                            format!("无法打开文件选择器：{error}"),
                            cx,
                        );
                        this.finish_library_request(cx)
                    });
                    if let Ok(Some(window)) = close_window {
                        schedule_library_window_removal(window, cx);
                    }
                    return;
                }
            };
            let may_import = view.update_in(cx, |this, _window, cx| {
                if this.library_mutations.close_requested() {
                    return false;
                }
                if this.library_notice_request == Some(request_id) {
                    this.notice = Some(Notice {
                        text: "正在检查并导入图书…".to_string(),
                        error: false,
                    });
                }
                cx.notify();
                true
            });
            if !matches!(may_import, Ok(true)) {
                let close_window = view.update_in(cx, |this, _window, cx| {
                    this.is_importing = false;
                    this.finish_library_request(cx)
                });
                if let Ok(Some(window)) = close_window {
                    schedule_library_window_removal(window, cx);
                }
                return;
            };
            let selected_path = path.clone();
            tracing::info!(
                target: "ngy_import",
                path = %selected_path.display(),
                "图书库界面：已选择文件，开始导入"
            );
            let task = services.spawn_library_projected(move |library| library.import(&path));
            let outcome = task.await;
            let close_window = view.update_in(cx, |this, _window, cx| {
                this.is_importing = false;
                match outcome {
                    Ok(Ok(mutation)) => {
                        let (message, book_id) = match &mutation.value {
                            ImportOutcome::Added(book) => {
                                (format!("《{}》已加入图书库", book.title), book.id.clone())
                            }
                            ImportOutcome::AlreadyExists(book) => {
                                (format!("《{}》已经在图书库中", book.title), book.id.clone())
                            }
                        };
                        tracing::info!(
                            target: "ngy_import",
                            path = %selected_path.display(),
                            book_id = %book_id,
                            result = %message,
                            "图书库界面：导入成功"
                        );
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: message,
                                error: false,
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::error!(
                            target: "ngy_import",
                            path = %selected_path.display(),
                            error = %format!("{error:#}"),
                            "图书库界面：导入失败"
                        );
                        this.set_library_request_error(
                            request_id,
                            format!("导入失败：{error:#}"),
                            cx,
                        );
                    }
                    Err(error) => {
                        tracing::error!(
                            target: "ngy_import",
                            path = %selected_path.display(),
                            error = %error,
                            "图书库界面：导入任务异常停止"
                        );
                        this.set_library_request_error(
                            request_id,
                            format!("导入任务异常停止：{error}"),
                            cx,
                        );
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
}
