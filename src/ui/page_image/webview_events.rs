//! Reacting to the AI sidebar and opening its sources.

use super::*;

impl PageImageApp {
    pub(super) fn on_ai_sidebar_event(
        &mut self,
        _sidebar: &Entity<AiSidebar>,
        event: &AiSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AiSidebarEvent::Submit(request) => {
                self.ai_controller
                    .submit(request.clone(), self.ai_sidebar.clone(), cx)
            }
            AiSidebarEvent::Cancel { request_id } => self.ai_controller.cancel(*request_id),
            AiSidebarEvent::NewSession => {
                self.ai_controller.new_session(self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::SwitchSession { thread_id } => {
                self.ai_controller
                    .switch_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::DeleteSession { thread_id } => {
                self.ai_controller
                    .delete_session(thread_id.clone(), self.ai_sidebar.clone(), cx);
            }
            AiSidebarEvent::ScopeChanged { book_ids } => {
                self.ai_controller
                    .reconcile_scope(book_ids.clone(), self.ai_sidebar.clone(), cx)
            }
            AiSidebarEvent::OpenSource(source) => self.open_ai_source(source.clone(), window, cx),
        }
    }

    pub(super) fn open_ai_source(
        &mut self,
        source: AiSourceLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let lookup = source.clone();
        let task = services.spawn_library_read(move |library| {
            let document = library.document(&lookup.book_id)?;
            let index = current_canonical_source_unit_index(&lookup, &document)
                .map_err(anyhow::Error::msg)?;
            Ok(index)
        });
        let kind = self.kind;
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match outcome {
                    Ok(Ok(index)) => {
                        if source.book_id == this.book_id {
                            if let Some(page_index) =
                                page_index_for_source(&this.pages, kind, this.current, &source)
                            {
                                this.set_current(page_index, cx);
                            } else {
                                this.notice = Some(Notice {
                                    text: "引用对应的页面已失效，未跳转到其它页面。".to_string(),
                                    error: true,
                                });
                                cx.notify();
                            }
                        } else {
                            this.library_view.update(cx, |library, cx| {
                                library.open_book_at_source(source, Some(index), window, cx);
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        this.notice = Some(Notice {
                            text: format!("引用已失效：{error:#}"),
                            error: true,
                        });
                        cx.notify();
                    }
                    Err(error) => {
                        this.notice = Some(Notice {
                            text: format!("引用定位任务已停止：{error}"),
                            error: true,
                        });
                        cx.notify();
                    }
                });
            });
        })
        .detach();
    }
}
