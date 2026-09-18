//! The rendered reader window.

use super::*;

impl Render for ReaderApp {
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
        let current = self.current_spine;
        let total = self.book.spine.len();
        let chapter_label = self
            .selected_toc
            .and_then(|index| self.book.toc.get(index))
            .map(|item| item.label.clone())
            .unwrap_or_else(|| self.book.spine[current].title.clone());
        let progress = if total == 0 {
            0.0
        } else {
            (current + 1) as f32 / total as f32
        };
        let (status_icon, status_text, status_color) =
            if self.closing || self.webview_build_gate.close_requested {
                (
                    IconName::BookOpen,
                    "正在安全关闭阅读器…",
                    palette(cx).accent,
                )
            } else if self.webview_build_gate.building {
                (IconName::BookOpen, "正在创建正文视图…", palette(cx).accent)
            } else if self.webview.is_some() {
                (IconName::CircleCheck, "正文视图已连接", 0x376441)
            } else {
                (
                    IconName::TriangleAlert,
                    "正文视图不可用",
                    palette(cx).danger,
                )
            };
        let reader_summary = format!("第 {} / {} 章 · {chapter_label}", current + 1, total);
        let reader_context = if self.search_query.is_empty() {
            format!("章节进度 · {:.0}%", progress * 100.)
        } else {
            format!("全文检索 · {} 个结果", self.search_results.len())
        };
        let status_bar = render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            reader_summary,
            reader_context,
            palette(cx),
        );

        let previous_view = view.clone();
        let next_view = view.clone();
        let notes_view = view.clone();
        // The reading window owns the display choice for this book: once chosen it
        // wins over the global preference from the system configuration and is
        // stored per book, and "跟随全局" hands it back to that default. Without a
        // target language nothing is ever translated, so the switch stays hidden
        // instead of offering a no-op.
        let provider_settings = self.services.provider_settings().ok();
        let global_display_mode = provider_settings
            .as_ref()
            .map(|settings| settings.translation_display_mode)
            .unwrap_or_default();
        let follow_global = self.translations.overrides_global().then(|| {
            let follow_view = view.clone();
            Button::new("reader-translation-follow-global")
                .ghost()
                .xsmall()
                .label("跟随全局")
                .tooltip(format!(
                    "恢复跟随系统配置（当前为{}）",
                    translation_display_mode_name(global_display_mode)
                ))
                .on_click(move |_, _, cx| {
                    follow_view.update(cx, |this, cx| this.follow_global_translation_display(cx));
                })
        });
        let display_switch = provider_settings
            .as_ref()
            .is_some_and(|settings| settings.default_language.is_some())
            .then(|| {
                let effective = self.translations.effective();
                TabBar::new("reader-translation-display")
                    .segmented()
                    .small()
                    .selected_index(
                        TRANSLATION_DISPLAY_MODES
                            .iter()
                            .position(|mode| *mode == effective)
                            .unwrap_or(TRANSLATION_DISPLAY_MODES.len() - 1),
                    )
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.set_translation_display(translation_display_mode(*index), cx);
                    }))
                    .children(
                        TRANSLATION_DISPLAY_MODES
                            .into_iter()
                            .map(|mode| Tab::new().label(translation_display_mode_name(mode))),
                    )
            });
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
                        div().h_flex().min_w(px(0.)).gap_3().child(
                            div()
                                .v_flex()
                                .min_w(px(0.))
                                .gap_0p5()
                                .child(
                                    div()
                                        .max_w(px(520.))
                                        .truncate()
                                        .font_semibold()
                                        .text_color(rgb(palette(cx).ink))
                                        .child(self.book.title.clone()),
                                )
                                .child(
                                    div()
                                        .max_w(px(520.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(palette(cx).muted))
                                        .child(chapter_label),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .when_some(display_switch, |row, switch| row.child(switch))
                            .when_some(follow_global, |row, button| row.child(button))
                            .child(
                                Button::new("reader-book-notes")
                                    .ghost()
                                    .icon(IconName::Menu)
                                    .label("本书笔记")
                                    .on_click(move |_, _, cx| {
                                        notes_view.update(cx, |this, cx| {
                                            if let Err(error) = open_notes_window(
                                                Arc::clone(&this.services),
                                                this.library_view.clone(),
                                                Some((
                                                    this.book_id.clone(),
                                                    this.book.title.clone(),
                                                )),
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
                                    .child(format!("{} / {}", current + 1, total)),
                            )
                            .child(
                                Button::new("previous-chapter")
                                    .ghost()
                                    .icon(IconName::ChevronLeft)
                                    .tooltip("上一章")
                                    .disabled(current == 0)
                                    .on_click(move |_, _, cx| {
                                        previous_view
                                            .update(cx, |this, cx| this.change_chapter(-1, cx));
                                    }),
                            )
                            .child(
                                Button::new("next-chapter")
                                    .ghost()
                                    .icon(IconName::ChevronRight)
                                    .tooltip("下一章")
                                    .disabled(current + 1 >= total)
                                    .on_click(move |_, _, cx| {
                                        next_view.update(cx, |this, cx| this.change_chapter(1, cx));
                                    }),
                            ),
                    ),
            )
            .child(
                div().h(px(3.)).w_full().bg(rgb(palette(cx).border)).child(
                    div()
                        .h_full()
                        .w(gpui::relative(progress))
                        .bg(rgb(palette(cx).accent)),
                ),
            );

        let navigation_items = if self.search_query.is_empty() {
            self.book
                .toc
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let selected = self.selected_toc == Some(index);
                    let navigable = item.href.is_some();
                    let padding = px(18. + item.depth.min(4) as f32 * 14.);
                    if navigable {
                        let toc_view = view.clone();
                        Button::new(("toc", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(px(38.))
                            .justify_start()
                            .pl(padding)
                            .pr_3()
                            .py_2()
                            .rounded(px(7.))
                            .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                            .on_click(move |_, _, cx| {
                                toc_view.update(cx, |this, cx| this.go_to_toc(index, cx));
                            })
                            .child(
                                div()
                                    .w_full()
                                    .text_left()
                                    .text_sm()
                                    .line_clamp(2)
                                    .text_color(if selected {
                                        rgb(palette(cx).accent)
                                    } else {
                                        rgb(palette(cx).ink)
                                    })
                                    .child(item.label.clone()),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .min_h(px(38.))
                            .pl(padding)
                            .pr_3()
                            .py_2()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(palette(cx).muted))
                            .child(div().line_clamp(2).child(item.label.clone()))
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>()
        } else {
            if self.search_results.is_empty() {
                vec![
                    div()
                        .p_4()
                        .text_center()
                        .text_sm()
                        .text_color(rgb(palette(cx).muted))
                        .child("当前图书没有匹配章节")
                        .into_any_element(),
                ]
            } else {
                self.search_results
                    .iter()
                    .enumerate()
                    .map(|(index, hit)| {
                        let result_view = view.clone();
                        let spine_index = hit.spine_index;
                        let selected = spine_index == Some(self.current_spine);
                        Button::new(("reader-search-result", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(px(66.))
                            .justify_start()
                            .px_3()
                            .py_2()
                            .rounded(px(7.))
                            .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                            .on_click(move |_, _, cx| {
                                if let Some(spine_index) = spine_index {
                                    result_view.update(cx, |this, cx| {
                                        this.navigate_to_spine(spine_index, cx)
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
                                                    .unwrap_or_else(|| "章节".to_string()),
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
                    .collect::<Vec<_>>()
            }
        };

        let navigation_collapse_view = view.clone();
        let toc = div()
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
                                "目录".to_string()
                            } else {
                                format!("搜索结果（{}）", self.search_results.len())
                            })),
                    )
                    .child(
                        Button::new("reader-navigation-collapse")
                            .ghost()
                            .xsmall()
                            .icon(IconName::PanelLeftClose)
                            .tooltip("收起目录")
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
            .child(
                div()
                    .id("toc-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .p_3()
                    .overflow_y_scrollbar()
                    .child(div().v_flex().gap_1().children(navigation_items)),
            );

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
                .child(div().text_sm().child("正在加载正文…"))
                .into_any_element(),
        };
        let content = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(rgb(palette(cx).neutral_soft))
            .child(
                div().size_full().p_3().child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(rgb(palette(cx).paper))
                        .child(body),
                ),
            );
        let navigation: gpui::AnyElement = if self.pane_layout.is_navigation_collapsed() {
            self.render_collapsed_navigation(cx)
        } else {
            toc.into_any_element()
        };
        let navigation_resize_handle = (!self.pane_layout.is_navigation_collapsed())
            .then(|| self.render_pane_resize_handle(ReaderResizablePane::Navigation, cx));
        let ai_resize_handle = (!self.ai_sidebar_collapsed)
            .then(|| self.render_pane_resize_handle(ReaderResizablePane::Ai, cx));

        div()
            .v_flex()
            .size_full()
            .bg(rgb(palette(cx).paper))
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
