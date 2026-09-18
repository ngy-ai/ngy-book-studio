//! The library request lifecycle: coalescing, error reporting and the search
//! input.

use super::*;

impl EpubReaderApp {
    pub(crate) fn refresh_after_projected_mutation(
        &mut self,
        generation: u64,
        snapshot: LibraryStore,
        cx: &mut Context<Self>,
    ) {
        if self.apply_shared_library_projection(generation, snapshot) {
            self.sync_ai_scope(cx);
            cx.notify();
        }
    }

    pub(super) fn set_library_request_error(
        &mut self,
        request_id: u64,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if self.library_notice_request != Some(request_id) {
            tracing::warn!(request_id, %message, "较新的书库操作已开始，仍向用户显示较早操作的失败");
        }
        // A newer request may replace a progress/success notice, but it must
        // never make an independent failed import or mutation disappear.
        self.library_notice_request = Some(request_id);
        self.set_error(message, cx);
    }

    pub(super) fn finish_name_modal_request(
        &mut self,
        request_id: u64,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (current, return_to) = match self.group_modal.as_mut() {
            Some(GroupModal::NameInput {
                pending_request,
                return_to,
                ..
            }) => (*pending_request == Some(request_id), return_to.take()),
            _ => return,
        };
        if !current {
            return;
        }
        match error {
            None => self.group_modal = return_to.map(|modal| *modal),
            Some(message) => {
                if let Some(GroupModal::NameInput {
                    pending_request,
                    error: modal_error,
                    ..
                }) = self.group_modal.as_mut()
                {
                    *pending_request = None;
                    *modal_error = Some(message);
                }
            }
        }
        cx.notify();
    }

    pub(super) fn visible_book_ids(&self) -> Vec<String> {
        self.visible_book_indices()
            .into_iter()
            .map(|index| self.library.books()[index].id.clone())
            .collect()
    }

    pub(super) fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.search_mode == mode {
            return;
        }
        self.search_mode = mode;
        self.start_search(cx);
    }

    pub(super) fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_results.clear();
        self.search_results_generation = None;
        self.search_loading = false;
        self.search_error = None;
        cx.notify();
    }

    pub(super) fn start_search(&mut self, cx: &mut Context<Self>) {
        if self.library_mutations.close_requested() {
            self.search_loading = false;
            cx.notify();
            return;
        }
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        self.search_results.clear();
        self.search_results_generation = None;
        self.search_error = None;

        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.search_loading = false;
            cx.notify();
            return;
        }

        let book_ids = self.visible_book_ids();
        if book_ids.is_empty() {
            self.search_loading = false;
            self.search_results_generation = Some(generation);
            cx.notify();
            return;
        }

        let search = match self.services.search() {
            Ok(search) => search,
            Err(error) => {
                self.search_loading = false;
                self.search_error = Some(format!("无法读取搜索服务：{error:#}"));
                cx.notify();
                return;
            }
        };
        self.search_loading = true;
        let mode = self.search_mode;
        let library = self.library.clone();
        let runtime = self.services.runtime();
        let task = runtime.spawn(async move {
            tokio::time::sleep(SEARCH_DEBOUNCE).await;
            let passages = search
                .search(SearchRequest {
                    query,
                    book_ids,
                    mode,
                    limit: LIBRARY_SEARCH_LIMIT,
                })
                .await?;
            tokio::task::spawn_blocking(move || resolve_search_hits(&library, passages))
                .await
                .context("搜索结果定位任务已停止")?
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let _ = view.update(cx, |this, cx| {
                if !is_current_search_completion(generation, this.search_generation) {
                    return;
                }
                this.search_loading = false;
                match outcome {
                    Ok(Ok(results)) => {
                        this.search_results = results;
                        this.search_results_generation = Some(generation);
                    }
                    Ok(Err(error)) => {
                        this.search_error = Some(format!("搜索失败：{error:#}"));
                    }
                    Err(error) => {
                        this.search_error = Some(format!("搜索任务已停止：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn on_search_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = input.read(cx).value().trim().to_string();
                self.search_query = query;
                if self.search_query.is_empty() {
                    self.clear_search(cx);
                } else {
                    self.start_search(cx);
                }
            }
            InputEvent::PressEnter { .. } => {
                if self.search_results_generation == Some(self.search_generation)
                    && let Some(hit) = self.search_results.first().cloned()
                {
                    self.open_search_hit(hit, window, cx);
                }
            }
            _ => {}
        }
    }
}
