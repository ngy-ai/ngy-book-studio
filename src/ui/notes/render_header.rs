//! The window header and the filter row.

use super::*;

impl NotesWindow {
    #[inline(never)]
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title = if self.book.is_some() {
            "本书笔记"
        } else {
            "全部笔记"
        };
        let subtitle = self
            .book
            .as_ref()
            .map(|(_, title)| format!("《{title}》"))
            .unwrap_or_else(|| "汇集所有图书的划线、人工想法与 AI 想法".into());
        let view = cx.entity();
        div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_4()
            .bg(rgb(palette(cx).surface))
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_1()
                    .child(div().text_lg().font_semibold().child(title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette(cx).muted))
                            .child(subtitle),
                    ),
            )
            .child(
                Button::new("notes-refresh")
                    .outline()
                    .icon(IconName::Redo2)
                    .label(if self.loading {
                        "正在刷新…"
                    } else {
                        "刷新"
                    })
                    .disabled(self.loading)
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| this.refresh(window, cx));
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_filters(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut filters = div().h_flex().gap_2();
        for (index, filter) in NoteFilter::ALL.into_iter().enumerate() {
            let view = cx.entity();
            filters = filters.child(
                Button::new(("notes-filter", index))
                    .small()
                    .label(filter.label())
                    .when(self.listing.filter == filter, |button| button.primary())
                    .when(self.listing.filter != filter, |button| button.ghost())
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.listing.filter = filter;
                            this.listing.apply_filter();
                            this.reset_scroll();
                            cx.notify();
                        });
                    }),
            );
        }
        div()
            .v_flex()
            .flex_none()
            .gap_3()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(Input::new(&self.search).w_full())
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(filters)
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child(format!(
                                "{} 条笔记 · {} 本图书",
                                self.listing.notes.len(),
                                self.listing.book_count
                            )),
                    ),
            )
            .into_any_element()
    }
}
