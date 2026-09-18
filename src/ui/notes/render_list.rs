//! The footer and the paged list of cards.

use super::*;

impl NotesWindow {
    #[inline(never)]
    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let previous = cx.entity();
        let next = cx.entity();
        div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_3()
            .bg(rgb(palette(cx).surface))
            .border_t_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!(
                        "找到 {} 条 · 最近更新在前",
                        self.listing.filtered.len()
                    )),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("notes-previous")
                            .small()
                            .outline()
                            .label("上一页")
                            .disabled(self.listing.page == 0)
                            .on_click(move |_, _, cx| {
                                previous.update(cx, |this, cx| {
                                    this.listing.set_page(this.listing.page.saturating_sub(1));
                                    this.reset_scroll();
                                    cx.notify();
                                });
                            }),
                    )
                    .child(div().text_sm().child(format!(
                        "{} / {}",
                        self.listing.page + 1,
                        self.listing.page_count()
                    )))
                    .child(
                        Button::new("notes-next")
                            .small()
                            .outline()
                            .label("下一页")
                            .disabled(self.listing.page + 1 >= self.listing.page_count())
                            .on_click(move |_, _, cx| {
                                next.update(cx, |this, cx| {
                                    this.listing.set_page(this.listing.page + 1);
                                    this.reset_scroll();
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_list(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // Explicit width avoids TextView's clipped short rows on Windows. The
        // card sits inside 48px outer padding and 34px card padding/borders.
        let width = (window.viewport_size().width - px(84.)).max(px(0.));
        let mut list = div()
            .id("notes-scroll")
            .v_flex()
            .size_full()
            .gap_4()
            .p_6()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        if self.listing.cards.is_empty() {
            let message = if self.loading && !self.loaded {
                "正在读取笔记…"
            } else if self.error.is_some() && !self.loaded {
                "读取笔记失败，请点击刷新重试。"
            } else if !self.listing.notes.is_empty() {
                "没有符合搜索或类型条件的笔记。"
            } else if self.book.is_some() {
                "这本书还没有笔记。阅读时选择正文即可划线或写想法。"
            } else {
                "还没有笔记。阅读时选择正文即可划线或写想法。"
            };
            list = list.child(
                div()
                    .v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(rgb(palette(cx).muted))
                    .child(Icon::new(IconName::Inbox))
                    .child(message),
            );
        } else {
            for card in &self.listing.cards {
                list = list.child(self.render_card(card, width, window, cx));
            }
        }
        div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }
}
