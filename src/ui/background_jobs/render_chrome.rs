//! The window chrome: header, kind bar, resize handle and filters.

use super::*;

impl BackgroundJobsWindow {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .debug_selector(|| "jobs-layout-header".into())
            .flex_none()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).surface))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_1()
                    .child(div().text_lg().font_semibold().child("后台任务"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .truncate()
                            .child(format!(
                                "{} · {} 本图书 · {} 个任务",
                                self.scope_label,
                                self.books.len(),
                                self.jobs.len()
                            )),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .flex_none()
                    .child(
                        Button::new("jobs-auto-refresh")
                            .small()
                            .ghost()
                            .label(if self.auto_refresh {
                                "自动刷新 · 2 秒"
                            } else {
                                "自动刷新已暂停"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.auto_refresh = !this.auto_refresh;
                                if this.auto_refresh {
                                    this.refresh(window, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("background-jobs-refresh")
                            .small()
                            .outline()
                            .icon(IconName::Redo2)
                            .label(if self.loading {
                                "刷新中…"
                            } else {
                                "刷新"
                            })
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.notice = None;
                                this.refresh(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Task-kind switcher pinned to the top of the window, above the list and
    /// detail columns. Wraps instead of clipping so every category stays
    /// reachable at the minimum window width.
    pub(super) fn render_kinds_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut bar = div()
            .debug_selector(|| "jobs-layout-kinds".into())
            .flex_none()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).surface))
            .child(
                div()
                    .mr_1()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("任务分类"),
            );
        for &(kind, label) in JOB_KINDS {
            let count = self
                .jobs
                .iter()
                .filter(|job| kind_matches(&job.kind, kind))
                .count();
            if kind == "other" && count == 0 {
                continue;
            }
            let selected = self.kind == kind;
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("jobs-kind-{kind}")))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_sm()
                    .bg(rgb(if selected {
                        palette(cx).accent_soft
                    } else {
                        palette(cx).sidebar
                    }))
                    .text_color(rgb(if selected {
                        palette(cx).accent_strong
                    } else {
                        palette(cx).ink
                    }))
                    .child(label)
                    .child(div().text_xs().child(count.to_string()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.kind = kind;
                        this.filters_changed(window, cx);
                    })),
            );
        }
        bar.into_any_element()
    }

    /// Left column of the body: the status filters, the paged task list and its
    /// footer. Its width is user-draggable; the divider on its right draws the
    /// separation from the detail panel.
    pub(super) fn render_list_column(
        &self,
        viewport_width: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .debug_selector(|| "jobs-layout-list-column".into())
            .v_flex()
            .w(px(self.effective_list_width(viewport_width)))
            .flex_none()
            .min_h(px(0.))
            .overflow_hidden()
            .child(self.render_filters(cx))
            .child(self.render_list(cx))
            .into_any_element()
    }

    /// Draggable divider between the list column and the detail panel. The
    /// pointer grabs it, follows it horizontally, and releases it in place.
    pub(super) fn render_resize_handle(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let handle_view = cx.entity().clone();
        let active = self.resizing_list;
        div()
            .id("jobs-list-resize-handle")
            .debug_selector(|| "jobs-layout-resize-handle".into())
            .h_full()
            .w(px(JOBS_RESIZE_HANDLE_WIDTH))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .bg(rgb(palette(cx).surface))
            .hover(|this| this.bg(rgb(palette(cx).accent_soft)))
            .on_drag(JobsListResizeDrag, move |_, _, _, cx| {
                cx.stop_propagation();
                handle_view.update(cx, |this, cx| {
                    if this.begin_list_resize() {
                        cx.notify();
                    }
                });
                cx.new(|_| EmptyView)
            })
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<JobsListResizeDrag>, window, cx| {
                    if this.resize_list_from_pointer(
                        event.event.position.x,
                        window.viewport_size().width,
                    ) {
                        cx.notify();
                    }
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.finish_list_resize() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.finish_list_resize() {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .h_full()
                    .w(px(if active { 2. } else { 1. }))
                    .bg(rgb(if active {
                        palette(cx).accent
                    } else {
                        palette(cx).border
                    })),
            )
            .into_any_element()
    }

    /// Status filters and the task search box. The kind switcher above already
    /// names and counts the current scope, so no extra summary line is drawn
    /// here.
    pub(super) fn render_filters(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut filters = div().h_flex().flex_wrap().gap_1();
        for &(status, label) in STATUS_FILTERS {
            filters = filters.child(
                Button::new(SharedString::from(format!("jobs-filter-{label}")))
                    .small()
                    .ghost()
                    .label(label)
                    .when(self.status == status, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(palette(cx).accent_soft).into())
                                .foreground(rgb(palette(cx).accent_strong).into()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.status = status;
                        this.filters_changed(window, cx);
                    })),
            );
        }
        div()
            .debug_selector(|| "jobs-layout-filters".into())
            .flex_none()
            .v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .w_full()
                    .child(Input::new(&self.search).small().prefix(IconName::Search)),
            )
            .child(filters)
            .into_any_element()
    }
}
