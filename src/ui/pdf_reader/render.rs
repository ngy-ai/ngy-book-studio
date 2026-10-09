//! The rendered PDF reader window.

use super::*;

impl Render for PdfReaderApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.closing && self.progress_close_ready && !self.removal_scheduled {
            self.removal_scheduled = true;
            remove_window_after_current_frame(window, cx, self.closing_webview.take());
        }
        if self
            .pane_layout
            .constrain(window.viewport_size().width, self.ai_sidebar_collapsed)
        {
            let width = self.pane_layout.ai_width();
            self.ai_sidebar
                .update(cx, |sidebar, cx| sidebar.set_expanded_width(width, cx));
        }
        let view = cx.entity().clone();
        let previous_view = view.clone();
        let next_view = view.clone();
        let notes_view = view.clone();
        let current_page = self.current_page;
        let total = self.actual_page_count.max(1);
        let progress = current_page as f32 / total as f32;
        let page_title = self
            .current_page_info()
            .map(|page| page.title.clone())
            .unwrap_or_else(|| format!("第 {current_page} 页"));
        let (status_icon, status_text, status_color) =
            if self.closing || self.webview_build_gate.close_requested {
                (
                    IconName::BookOpen,
                    "正在安全关闭 PDF 阅读器…",
                    palette(cx).accent,
                )
            } else if self.webview_build_gate.building || !self.pdf_ready {
                (IconName::BookOpen, "正在加载本地 PDF…", palette(cx).accent)
            } else if self.webview.is_some() {
                (IconName::CircleCheck, "PDF.js 视图已连接", 0x376441)
            } else {
                (
                    IconName::TriangleAlert,
                    "PDF 视图不可用",
                    palette(cx).danger,
                )
            };
        let status_bar = render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            format!("第 {current_page} / {total} 页 · {page_title}"),
            if self.search_query.is_empty() {
                if self.persist_progress_enabled {
                    format!("页面进度 · {:.0}%", progress * 100.)
                } else {
                    "临时预览 · 不修改阅读进度".to_string()
                }
            } else {
                format!("全文检索 · {} 个结果", self.search_results.len())
            },
            palette(cx),
        );

        let toolbar = div()
            .v_flex()
            .bg(rgb(palette(cx).surface))
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .when_some(self.notice.as_ref(), |toolbar, notice| {
                let (background, foreground, icon) = if notice.error {
                    (
                        rgb(palette(cx).danger_soft),
                        rgb(palette(cx).danger),
                        IconName::TriangleAlert,
                    )
                } else {
                    (
                        rgb(palette(cx).success_soft),
                        rgb(palette(cx).success),
                        IconName::CircleCheck,
                    )
                };
                toolbar.child(
                    div()
                        .h_flex()
                        .min_h(px(36.))
                        .px_5()
                        .gap_2()
                        .border_t_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(background)
                        .text_xs()
                        .text_color(foreground)
                        .child(Icon::new(icon).small())
                        .child(notice.text.clone()),
                )
            })
            .child(
                div()
                    .h_flex()
                    .h(px(66.))
                    .px_5()
                    .justify_between()
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .gap_0p5()
                            .child(
                                div()
                                    .max_w(px(560.))
                                    .truncate()
                                    .font_semibold()
                                    .text_color(rgb(palette(cx).ink))
                                    .child(self.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(format!("{} · {page_title}", self.preview_label)),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("pdf-book-notes")
                                    .ghost()
                                    .icon(IconName::Menu)
                                    .label("本书笔记")
                                    .on_click(move |_, _, cx| {
                                        notes_view.update(cx, |this, cx| {
                                            if let Err(error) = open_notes_window(
                                                Arc::clone(&this.services),
                                                this.library_view.clone(),
                                                Some((this.book_id.clone(), this.title.clone())),
                                                cx,
                                            ) {
                                                this.set_error(
                                                    format!("无法打开本书笔记：{error:#}"),
                                                    cx,
                                                );
                                            }
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .mr_2()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(format!("{current_page} / {total}")),
                            )
                            .child(
                                Button::new("previous-pdf-page")
                                    .ghost()
                                    .icon(IconName::ChevronLeft)
                                    .tooltip("上一页")
                                    .disabled(!self.pdf_ready || current_page <= 1)
                                    .on_click(move |_, _, cx| {
                                        previous_view.update(cx, |this, cx| {
                                            this.request_page(current_page.saturating_sub(1), cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("next-pdf-page")
                                    .ghost()
                                    .icon(IconName::ChevronRight)
                                    .tooltip("下一页")
                                    .disabled(!self.pdf_ready || current_page >= total)
                                    .on_click(move |_, _, cx| {
                                        next_view.update(cx, |this, cx| {
                                            this.request_page(current_page.saturating_add(1), cx)
                                        });
                                    }),
                            ),
                    ),
            )
            .child(
                div().h(px(3.)).w_full().bg(rgb(palette(cx).border)).child(
                    div()
                        .h_full()
                        .w(gpui_kit::relative(progress))
                        .bg(rgb(palette(cx).accent)),
                ),
            );

        // Every page gets a row so the whole document can be navigated from the
        // sidebar. Pages without a canonical unit (ephemeral Office previews)
        // still get a plain "第 N 页" row.
        let page_entries = self
            .pages
            .iter()
            .map(|page| (page.page_number, page.title.clone()))
            .collect::<Vec<_>>();
        let page_entries = if page_entries.is_empty() {
            (1..=total)
                .map(|page_number| (page_number, format!("第 {page_number} 页")))
                .collect::<Vec<_>>()
        } else {
            page_entries
        };
        let navigation_items = if self.search_query.is_empty() {
            Vec::new()
        } else if self.search_results.is_empty() {
            vec![
                div()
                    .p_4()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(palette(cx).muted))
                    .child("当前 PDF 没有匹配页面")
                    .into_any_element(),
            ]
        } else {
            self.search_results
                .iter()
                .enumerate()
                .map(|(index, hit)| {
                    let result_view = view.clone();
                    let unit_index = hit.spine_index;
                    let page_number = self.page_for_unit_index(unit_index);
                    let selected = page_number == Some(current_page);
                    Button::new(("pdf-search-result", index))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(66.))
                        .justify_start()
                        .px_3()
                        .py_2()
                        .rounded(px(7.))
                        .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                        .disabled(unit_index.is_none())
                        .on_click(move |_, window, cx| {
                            if let Some(unit_index) = unit_index {
                                result_view.update(cx, |this, cx| {
                                    this.open_canonical_unit(unit_index, window, cx)
                                });
                            }
                        })
                        .child(
                            div()
                                .v_flex()
                                .w_full()
                                .min_w(px(0.))
                                .gap_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_left()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(if selected {
                                            rgb(palette(cx).accent)
                                        } else {
                                            rgb(palette(cx).ink)
                                        })
                                        .child(
                                            hit.chapter_title
                                                .clone()
                                                .or_else(|| {
                                                    page_number.map(|page| format!("第 {page} 页"))
                                                })
                                                .unwrap_or_else(|| "PDF".to_string()),
                                        ),
                                )
                                .child(
                                    div()
                                        .line_clamp(2)
                                        .text_left()
                                        .text_xs()
                                        .text_color(rgb(palette(cx).muted))
                                        .child(hit.snippet.clone()),
                                ),
                        )
                        .into_any_element()
                })
                .collect()
        };

        let navigation_collapse_view = view.clone();
        let navigation = div()
            .v_flex()
            .w(self.pane_layout.navigation_width())
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).sidebar))
            .child(
                div()
                    .h_flex()
                    .h(px(54.))
                    .pl_5()
                    .pr_2()
                    .justify_between()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(palette(cx).border))
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(palette(cx).ink))
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .gap_2()
                            .child(Icon::new(IconName::Menu).small())
                            .child(div().truncate().child(if self.search_query.is_empty() {
                                format!("页面（{}）", page_entries.len())
                            } else {
                                format!("搜索结果（{}）", self.search_results.len())
                            })),
                    )
                    .child(
                        Button::new("pdf-navigation-collapse")
                            .ghost()
                            .xsmall()
                            .icon(IconName::PanelLeftClose)
                            .tooltip("收起页面列表")
                            .on_click(move |_, _, cx| {
                                navigation_collapse_view
                                    .update(cx, |this, cx| this.toggle_navigation_collapsed(cx));
                            }),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(palette(cx).border))
                    .child(
                        Input::new(&self.search_input)
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    ),
            )
            .child(if self.search_query.is_empty() {
                // Keep the list centered on the page the WebView is actually on
                // (initial open, prev/next, search and AI citation jumps). While
                // the pane is collapsed the list is not mounted, so leave the
                // sync to the first frame that shows it again.
                if !self.pane_layout.is_navigation_collapsed()
                    && self.page_list_synced_page != Some(current_page)
                {
                    if let Some(index) = page_entries
                        .iter()
                        .position(|(page_number, _)| *page_number == current_page)
                    {
                        self.page_list_scroll
                            .scroll_to_item(index, gpui_kit::ScrollStrategy::Center);
                    }
                    self.page_list_synced_page = Some(current_page);
                }
                let entries = page_entries.clone();
                let entry_view = view.clone();
                let pdf_ready = self.pdf_ready;
                // 虚拟列表是 `move` 闭包，捕获 `cx` 会把借用带出方法体；
                // `Palette` 是 `Copy`，取一次值即可。
                let theme = palette(cx);
                gpui_kit::uniform_list("pdf-page-list", entries.len(), move |range, _, _| {
                    range
                        .map(|index| {
                            let (page_number, title) = entries[index].clone();
                            let selected = page_number == current_page;
                            let item_view = entry_view.clone();
                            Button::new(("pdf-page-item", page_number as usize))
                                .ghost()
                                .w_full()
                                .h(px(50.))
                                .justify_start()
                                .px_3()
                                .rounded(px(7.))
                                .when(selected, |this| this.bg(rgb(theme.accent_soft)))
                                .disabled(!pdf_ready)
                                .on_click(move |_, _, cx| {
                                    item_view
                                        .update(cx, |this, cx| this.request_page(page_number, cx));
                                })
                                .child(
                                    div()
                                        .v_flex()
                                        .w_full()
                                        .min_w(px(0.))
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .truncate()
                                                .text_left()
                                                .text_sm()
                                                .font_medium()
                                                .text_color(if selected {
                                                    rgb(theme.accent)
                                                } else {
                                                    rgb(theme.ink)
                                                })
                                                .child(format!("第 {page_number} 页")),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_left()
                                                .text_xs()
                                                .text_color(rgb(theme.muted))
                                                .child(title),
                                        ),
                                )
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.page_list_scroll.clone())
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .px_2()
                .py_2()
                .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .p_3()
                    .overflow_y_scrollbar()
                    .child(div().v_flex().gap_1().children(navigation_items))
                    .into_any_element()
            });

        let body = match &self.webview {
            Some(webview) => webview.clone().into_any_element(),
            None => div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(rgb(palette(cx).muted))
                .child(Icon::new(IconName::BookOpen).large())
                .child(div().text_sm().child("正在加载 PDF.js…"))
                .into_any_element(),
        };
        let content = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            // PDF 外壳与页面卡片由 viewer 自己画；这层是页面到达前以及左右留白的
            // 底色，必须和 viewer 的根背景同色，否则会露出一圈旧皮肤的边。
            .bg(rgb(palette(cx).paper))
            .child(body);
        let navigation: gpui_kit::AnyElement = if self.pane_layout.is_navigation_collapsed() {
            self.render_collapsed_navigation(cx)
        } else {
            navigation.into_any_element()
        };
        let navigation_resize_handle = (!self.pane_layout.is_navigation_collapsed())
            .then(|| self.render_pane_resize_handle(ReaderResizablePane::Navigation, cx));
        let ai_resize_handle = (!self.ai_sidebar_collapsed)
            .then(|| self.render_pane_resize_handle(ReaderResizablePane::Ai, cx));

        div()
            .v_flex()
            .size_full()
            .bg(rgb(palette(cx).paper))
            .on_key_down(move |event: &gpui_kit::KeyDownEvent, window, cx| {
                let view = view.clone();
                let current_page = current_page;
                view.update(cx, |this, cx| {
                    if this
                        .search_input
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window)
                    {
                        return;
                    }
                    if event.keystroke.modifiers != gpui_kit::Modifiers::control() {
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "left" | "up" => this.request_page(current_page.saturating_sub(1), cx),
                        "right" | "down" => this.request_page(current_page.saturating_add(1), cx),
                        _ => {}
                    }
                });
            })
            .child(toolbar)
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .flex_1()
                    .min_h(px(0.))
                    .child(navigation)
                    .when_some(navigation_resize_handle, |this, handle| this.child(handle))
                    .child(content)
                    .when_some(ai_resize_handle, |this, handle| this.child(handle))
                    .child(self.ai_sidebar.clone()),
            )
            .child(status_bar)
            .into_any_element()
    }
}
