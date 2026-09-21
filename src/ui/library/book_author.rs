//! Changing a book's author from the grid.

use super::*;

impl EpubReaderApp {
    /// Opens the shared name input on the book's current author. A blank value
    /// is accepted and clears the authors, which the grid then shows as
    /// 「未知作者」.
    pub(super) fn open_set_book_author_dialog(
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
        // `未知作者` is the catalog placeholder for a book that has no author,
        // and the value the importer filters back out. Start the input empty so
        // the user does not have to delete a placeholder.
        let current = if book.author == "未知作者" {
            String::new()
        } else {
            book.author.clone()
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.group_modal = Some(GroupModal::NameInput {
            title: format!("修改「{}」的作者", book.title),
            confirm_label: "保存".to_string(),
            action: NameInputAction::SetBookAuthor(book_id),
            input,
            pending_request: None,
            error: None,
            return_to: None,
        });
        cx.notify();
    }

    pub(super) fn set_book_author(
        &mut self,
        book_id: String,
        author: String,
        cx: &mut Context<Self>,
    ) {
        let title = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .map(|book| book.title.clone())
            .unwrap_or_default();
        let Some(request_id) = self.begin_library_request("正在修改图书作者…", cx) else {
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
        let task = services
            .spawn_library_projected(move |library| library.set_book_author(&book_id, &author));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        let saved = mutation.value.author.clone();
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: if saved == "未知作者" {
                                    format!("已清除《{title}》的作者")
                                } else {
                                    format!("《{title}》的作者已改为「{saved}」")
                                },
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => {
                        let message = format!("修改图书作者失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("修改图书作者任务异常停止：{error}");
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
