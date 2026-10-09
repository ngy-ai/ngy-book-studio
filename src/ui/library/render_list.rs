//! The one-book-per-row list view. Every row shows the cover thumbnail, the
//! title and author, the group and format tags, the import date and the
//! group button; clicking a row opens the same context menu as a card.

use super::*;

impl EpubReaderApp {
    pub(super) fn render_book_list(
        &self,
        indices: &[usize],
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let rows = indices
            .iter()
            .map(|index| self.render_book_row(&self.library.books()[*index], cx))
            .collect::<Vec<_>>();

        div()
            .id("library-list-scroll")
            .debug_selector(|| "library-book-list".into())
            .flex_1()
            .min_h(px(0.))
            .pb_8()
            .overflow_y_scrollbar()
            .child(div().v_flex().gap_0p5().children(rows))
            .into_any_element()
    }

    pub(super) fn render_book_row(
        &self,
        book: &BookRecord,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        let row_id = SharedString::from(format!("book-row-{book_id}"));
        let row_selector = row_id.clone();
        let open_view = view.clone();
        let open_id = book_id.clone();
        let context_menu = self.book_context_menu(book, cx);
        let group = book
            .group_id
            .as_deref()
            .and_then(|group_id| self.library.group(group_id));
        let cover = self.book_cover_image(book, 6.).unwrap_or_else(|| {
            div()
                .v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .bg(rgb(theme.sidebar))
                .text_color(rgb(theme.accent))
                .child(Icon::new(IconName::BookOpen).small())
                .into_any_element()
        });

        div()
            .id(row_id)
            .debug_selector(move || row_selector.to_string())
            .h_flex()
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(12.))
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme.paper)).border_color(rgb(theme.border)))
            .active(|style| style.bg(rgb(theme.sidebar)))
            // Same rule as the card grid: a row opens on double click, so a
            // stray click while browsing cannot jump into a book.
            .on_double_click(move |_, window, cx| {
                open_view.update(cx, |this, cx| this.open_book(open_id.clone(), window, cx));
            })
            .context_menu(context_menu)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(44.))
                    .h(px(60.))
                    .overflow_hidden()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .bg(rgb(theme.surface))
                    .child(cover),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(theme.ink))
                            .child(book.title.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child(book.author.clone()),
                    ),
            )
            .when_some(group, |this, group| {
                this.child(
                    div()
                        .h_flex()
                        .flex_none()
                        .max_w(px(140.))
                        .gap_1()
                        .text_xs()
                        .text_color(rgb(theme.accent))
                        .child(
                            Icon::new(IconName::Folder)
                                .small()
                                .text_color(rgb(theme.accent)),
                        )
                        .child(div().min_w(px(0.)).truncate().child(group.name.clone())),
                )
            })
            .child(
                div()
                    .flex_none()
                    .px_1p5()
                    .py_0p5()
                    .rounded(px(4.))
                    .bg(rgb(theme.accent_soft))
                    .text_xs()
                    .font_medium()
                    .text_color(rgb(theme.accent_strong))
                    .child(book.format.to_ascii_uppercase()),
            )
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .w(px(112.))
                    .justify_end()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(theme.muted))
                    .child(Icon::new(IconName::Calendar).small())
                    .child(format_added_date(book.added_at)),
            )
            .child(self.render_book_group_menu(book, cx))
            .into_any_element()
    }
}
