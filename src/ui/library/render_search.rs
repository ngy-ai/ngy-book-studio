//! The empty state and the search result list.

use super::*;

impl EpubReaderApp {
    /// Indices into `library.books()` for the currently selected shelf, in the
    /// order the user picked (name, author or import time). A group shows its
    /// own books plus everything filed under its descendants.
    pub(super) fn visible_book_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = match &self.group_filter {
            GroupFilter::All => (0..self.library.books().len()).collect(),
            GroupFilter::Ungrouped => self
                .library
                .books()
                .iter()
                .enumerate()
                .filter(|(_, book)| book.group_id.is_none())
                .map(|(index, _)| index)
                .collect(),
            GroupFilter::Group(id) => {
                let subtree = self.library.group_subtree_ids(id);
                let ids: HashSet<&str> = subtree.iter().map(String::as_str).collect();
                self.library
                    .books()
                    .iter()
                    .enumerate()
                    .filter(|(_, book)| {
                        book.group_id
                            .as_deref()
                            .is_some_and(|group_id| ids.contains(group_id))
                    })
                    .map(|(index, _)| index)
                    .collect()
            }
        };
        sort_book_indices(
            self.library.books(),
            &mut indices,
            self.book_sort,
            self.book_sort_direction,
        );
        indices
    }

    pub(super) fn render_empty_library(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();
        div()
            .flex_1()
            .v_flex()
            .items_center()
            .justify_center()
            .pb_6()
            .child(
                div()
                    .v_flex()
                    .items_center()
                    .w(px(420.))
                    .p_8()
                    .gap_3()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(palette(cx).border))
                    .bg(rgb(palette(cx).paper))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(56.))
                            .rounded_full()
                            .bg(rgb(palette(cx).accent_soft))
                            .text_color(rgb(palette(cx).accent))
                            .child(Icon::new(IconName::BookOpen).large()),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .text_color(rgb(palette(cx).ink))
                            .child("你的书架还是空的"),
                    )
                    .child(
                        div()
                            .max_w(px(330.))
                            .text_center()
                            .text_sm()
                            .line_height(gpui::relative(1.6))
                            .text_color(rgb(palette(cx).muted))
                            .child("导入 EPUB、PDF、Office 或 DRM-free Kindle 图书，原件会完整保存在本地。"),
                    )
                    .child(
                        self.accent_button("empty-import", cx)
                            .icon(IconName::Plus)
                            .label(if self.is_importing {
                                "正在导入…"
                            } else {
                                "选择图书文件"
                            })
                            .disabled(self.is_importing)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.import_book(window, cx));
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_library_search_results(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.search_loading {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .pb_6()
                .child(
                    div()
                        .v_flex()
                        .items_center()
                        .w(px(400.))
                        .p_8()
                        .gap_3()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(rgb(palette(cx).paper))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(56.))
                                .rounded_full()
                                .bg(rgb(palette(cx).accent_soft))
                                .text_color(rgb(palette(cx).accent))
                                .child(Icon::new(IconName::Search).large()),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child(format!(
                                    "正在进行{}检索…",
                                    search_mode_label(self.search_mode)
                                )),
                        )
                        .child(
                            div()
                                .max_w(px(330.))
                                .text_center()
                                .text_sm()
                                .line_height(gpui::relative(1.6))
                                .text_color(rgb(palette(cx).muted))
                                .child("搜索范围只包含当前分组及其子分组中的图书。"),
                        ),
                )
                .into_any_element();
        }

        if let Some(error) = self.search_error.as_ref() {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .pb_6()
                .child(
                    div()
                        .v_flex()
                        .items_center()
                        .w(px(420.))
                        .p_8()
                        .gap_3()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(Palette::tint(palette(cx).danger, 0.18))
                        .bg(rgb(palette(cx).danger_soft))
                        .child(
                            div()
                                .text_color(rgb(palette(cx).danger))
                                .child(Icon::new(IconName::TriangleAlert).large()),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child("无法完成搜索"),
                        )
                        .child(
                            div()
                                .max_w(px(350.))
                                .text_center()
                                .text_sm()
                                .line_height(gpui::relative(1.6))
                                .text_color(rgb(palette(cx).muted))
                                .child(error.clone()),
                        ),
                )
                .into_any_element();
        }

        if self.search_results.is_empty() {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .pb_6()
                .child(
                    div()
                        .v_flex()
                        .items_center()
                        .w(px(400.))
                        .p_8()
                        .gap_3()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(rgb(palette(cx).paper))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(56.))
                                .rounded_full()
                                .bg(rgb(palette(cx).accent_soft))
                                .text_color(rgb(palette(cx).accent))
                                .child(Icon::new(IconName::Search).large()),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child("没有找到相关内容"),
                        )
                        .child(
                            div()
                                .max_w(px(320.))
                                .text_center()
                                .text_sm()
                                .line_height(gpui::relative(1.6))
                                .text_color(rgb(palette(cx).muted))
                                .child(match self.search_mode {
                                    SearchMode::Keyword => "试试更短的关键词，或检查正文中的拼写。",
                                    SearchMode::Semantic | SearchMode::Hybrid => {
                                        "可换一种表达，或切换到关键词检索。"
                                    }
                                }),
                        ),
                )
                .into_any_element();
        }

        let view = cx.entity().clone();
        let rows = self
            .search_results
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let open_view = view.clone();
                let open_hit = hit.clone();
                let location = hit
                    .chapter_title
                    .as_deref()
                    .map(|chapter| format!("内容 · {chapter}"))
                    .unwrap_or_else(|| "内容位置".to_string());
                Button::new(("library-search-result", index))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .min_h(px(86.))
                    .justify_start()
                    .p_4()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(palette(cx).border))
                    .bg(rgb(palette(cx).surface))
                    // Same rule as the book grid: a hit is opened on double
                    // click, so a stray click while reading snippets cannot
                    // jump into a book. `Button` has no `on_double_click`, so
                    // the click count is checked here; keyboard activation is
                    // kept working.
                    .on_click(move |event, window, cx| {
                        if event.click_count() != 2 && !event.is_keyboard() {
                            return;
                        }
                        open_view.update(cx, |this, cx| {
                            this.open_search_hit(open_hit.clone(), window, cx)
                        });
                    })
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .w_full()
                            .gap_1()
                            .child(
                                div()
                                    .h_flex()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_semibold()
                                            .text_color(rgb(palette(cx).ink))
                                            .child(hit.book_title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(rgb(palette(cx).accent))
                                            .child(location),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(hit.author.clone()),
                            )
                            .child(
                                div()
                                    .line_clamp(2)
                                    .text_left()
                                    .text_sm()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(hit.snippet.clone()),
                            ),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .id("library-search-scroll")
            .flex_1()
            .min_h(px(0.))
            .pb_8()
            .overflow_y_scrollbar()
            .child(div().v_flex().gap_3().children(rows))
            .into_any_element()
    }
}
