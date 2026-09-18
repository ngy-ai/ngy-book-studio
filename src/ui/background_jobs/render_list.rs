//! The job list column.

use super::*;

impl BackgroundJobsWindow {
    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let jobs = self.filtered_jobs();
        let count = jobs.len();
        let mut list = div()
            .id("background-jobs-list")
            .debug_selector(|| "jobs-layout-list".into())
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .px_3()
            .py_2()
            .gap_1();
        if count == 0 {
            list = list.child(
                div()
                    .p_6()
                    .text_sm()
                    .text_color(rgb(palette(cx).muted))
                    .child(if self.loading && !self.loaded {
                        "正在读取任务…"
                    } else if !self.loaded && self.refresh_error.is_some() {
                        "读取失败，请刷新重试。"
                    } else if self.books.is_empty() {
                        "当前范围没有图书。"
                    } else if self.jobs.is_empty() {
                        "当前范围还没有后台任务。"
                    } else {
                        "没有符合搜索或筛选条件的任务。"
                    }),
            );
        }
        for job in jobs
            .into_iter()
            .skip(self.page * JOBS_PER_PAGE)
            .take(JOBS_PER_PAGE)
        {
            let id = job.id.clone();
            let selected = self.selected_job_id.as_deref() == Some(&job.id);
            let (label, color, background) = status_presentation(job, palette(cx));
            let title = self
                .books
                .iter()
                .find(|book| book.id == job.book_id)
                .map(|book| book.title.as_str())
                .unwrap_or("图书已移除");
            list = list.child(
                div()
                    .id(SharedString::from(format!("job-row-{}", job.id)))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(rgb(if selected {
                        palette(cx).accent
                    } else {
                        palette(cx).border
                    }))
                    .bg(rgb(if selected {
                        palette(cx).accent_soft
                    } else {
                        palette(cx).surface
                    }))
                    .cursor_pointer()
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .truncate()
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .truncate()
                                    .child(format!("{} · {}", job_kind_label(&job.kind), job.id)),
                            ),
                    )
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .items_end()
                            .flex_none()
                            .child(
                                div()
                                    .px_2()
                                    .rounded_full()
                                    .bg(background)
                                    .text_xs()
                                    .text_color(color)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(progress_label(job)),
                            ),
                    )
                    .child(Icon::new(IconName::ChevronRight).small())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.selected_job_id.as_ref() != Some(&id) {
                            this.selected_job_id = Some(id.clone());
                            this.clear_detail_cache();
                        }
                        this.refresh_logs(window, cx);
                        this.refresh_blocks(window, cx);
                        cx.notify();
                    })),
            );
        }
        let pages = count.div_ceil(JOBS_PER_PAGE).max(1);
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(list.size_full())
                    .vertical_scrollbar(&self.list_scroll),
            )
            .child(
                div()
                    .debug_selector(|| "jobs-layout-footer".into())
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(palette(cx).border))
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!(
                        "共 {count} 个任务 · 第 {} / {pages} 页",
                        self.page + 1
                    ))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("jobs-previous")
                                    .small()
                                    .ghost()
                                    .label("上一页")
                                    .disabled(self.page == 0)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.page = this.page.saturating_sub(1);
                                        this.list_scroll.set_offset(gpui::point(px(0.), px(0.)));
                                        this.reconcile_selection();
                                        this.refresh_logs(window, cx);
                                        this.refresh_blocks(window, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("jobs-next")
                                    .debug_selector(|| "jobs-next".into())
                                    .small()
                                    .ghost()
                                    .label("下一页")
                                    .disabled(self.page + 1 >= pages)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.page += 1;
                                        this.list_scroll.set_offset(gpui::point(px(0.), px(0.)));
                                        this.reconcile_selection();
                                        this.refresh_logs(window, cx);
                                        this.refresh_blocks(window, cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
