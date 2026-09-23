//! Renaming a book from the grid.

use super::*;

impl EpubReaderApp {
    /// Opens the shared name input on the book's current title. The rename is
    /// a normal library mutation: the canonical document owns the title, so it
    /// is published as a new revision instead of writing the catalog row alone.
    pub(super) fn open_rename_book_dialog(
        &mut self,
        book_id: String,
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
        let input = cx.new(|cx| InputState::new(window, cx).default_value(book.title.clone()));
        input.update(cx, |state, cx| state.focus(window, cx));
        tracing::debug!(
            target: "ngy_library",
            book_id = %book_id,
            title = %book.title,
            "书库：已打开修改名称弹窗"
        );
        self.group_modal = Some(GroupModal::NameInput {
            title: format!("修改「{}」的名称", book.title),
            confirm_label: "保存".to_string(),
            action: NameInputAction::RenameBook(book_id),
            input,
            pending_request: None,
            error: None,
            return_to: None,
        });
        cx.notify();
    }

    pub(super) fn rename_book(&mut self, book_id: String, title: String, cx: &mut Context<Self>) {
        let Some(request_id) = self.begin_library_request("正在修改图书名称…", cx) else {
            tracing::warn!(
                target: "ngy_library",
                book_id = %book_id,
                "书库：窗口正在关闭，拒绝修改图书名称"
            );
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
        let completed_title = title.clone();
        let started = std::time::Instant::now();
        tracing::info!(
            target: "ngy_library",
            request_id,
            book_id = %book_id,
            title = %completed_title,
            "书库：开始修改图书名称"
        );
        let task =
            services.spawn_library_projected(move |library| library.rename_book(&book_id, &title));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        tracing::info!(
                            target: "ngy_library",
                            request_id,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "书库：图书名称已修改"
                        );
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("图书已更名为《{completed_title}》"),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => {
                        let message = format!("修改图书名称失败：{error:#}");
                        tracing::warn!(
                            target: "ngy_library",
                            request_id,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            error = %format!("{error:#}"),
                            "书库：修改图书名称失败"
                        );
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("修改图书名称任务异常停止：{error}");
                        tracing::warn!(
                            target: "ngy_library",
                            request_id,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            error = %error,
                            "书库：修改图书名称任务异常停止"
                        );
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
}
