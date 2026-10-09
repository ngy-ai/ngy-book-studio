//! The book grid and the individual book card. The context menu shared with
//! the list rows lives in `book_menu.rs`, the one-book-per-row layout in
//! `render_list.rs`.

use super::*;

impl EpubReaderApp {
    /// The cover pixels of one book, or `None` when the library has no cover
    /// for it. Binary cover data is hydrated from object storage before render.
    /// The image itself is rounded: it is laid out by its own bounds, so a
    /// `rounded` style on the parent box would clip nothing — the raw pixels
    /// are drawn into the parent's corner area. Callers that get `None` render
    /// their own fallback sized for their surface.
    pub(super) fn book_cover_image(
        &self,
        book: &BookRecord,
        radius: f32,
    ) -> Option<gpui_kit::AnyElement> {
        let bytes = self.library.cover_bytes_cached(&book.id)?;
        let format = book
            .cover_mime
            .as_deref()
            .and_then(image_format_from_mime)?;
        let image = Arc::new(Image::from_bytes(format, (*bytes).clone()));
        Some(
            img(image)
                .size_full()
                .rounded(px(radius))
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
        )
    }

    pub(super) fn render_book_grid(
        &self,
        indices: &[usize],
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let cards = indices
            .iter()
            .map(|index| self.render_book_card(&self.library.books()[*index], cx))
            .collect::<Vec<_>>();

        div()
            .id("library-scroll")
            .debug_selector(|| "library-book-grid".into())
            .flex_1()
            .min_h(px(0.))
            .pb_8()
            .overflow_y_scrollbar()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_5()
                    .children(cards),
            )
            .into_any_element()
    }

    pub(super) fn render_book_card(
        &self,
        book: &BookRecord,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        // 悬停/按下样式是 `'static` 闭包，借用 `cx` 会逃逸，`move` 闭包又会夺走它；
        // 取一次值（`Palette` 是 `Copy`）供整张卡片共用。
        let theme = palette(cx);
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        let cover = self.book_cover_image(book, 10.).unwrap_or_else(|| {
            div()
                .v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(rgb(theme.sidebar))
                .text_color(rgb(theme.accent))
                .child(Icon::new(IconName::BookOpen).large())
                .child(
                    div()
                        .text_center()
                        .text_xs()
                        .font_medium()
                        .child(book.format.to_ascii_uppercase()),
                )
                .into_any_element()
        });

        let open_view = view.clone();
        let open_id = book_id.clone();
        let card_id = SharedString::from(format!("book-card-{book_id}"));
        let context_menu = self.book_context_menu(book, cx);

        // A plain `div` is used instead of a `Button` so the children stretch
        // to the wrapper's 178px width and we don't inherit any button chrome
        // (padding, hover background, etc.).
        let card = div()
            .id(card_id)
            .v_flex()
            .w_full()
            .gap_2()
            .p_2()
            .rounded(px(14.))
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme.paper)).border_color(rgb(theme.border)))
            .active(|style| style.bg(rgb(theme.sidebar)))
            // Books open on double click, matching the shell's file list: a
            // single click only focuses the card, so browsing the grid or
            // reaching for the card's own buttons cannot open a reader.
            .on_double_click(move |_, window, cx| {
                open_view.update(cx, |this, cx| this.open_book(open_id.clone(), window, cx));
            })
            .context_menu(context_menu)
            .child(
                div()
                    .flex()
                    .w_full()
                    .h(px(252.))
                    .overflow_hidden()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .bg(rgb(theme.surface))
                    .child(cover),
            )
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap_1()
                    .text_left()
                    .child(
                        div()
                            .w_full()
                            .text_sm()
                            .font_semibold()
                            .line_clamp(2)
                            .text_color(rgb(palette(cx).ink))
                            .child(book.title.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .line_clamp(1)
                            .text_color(rgb(palette(cx).muted))
                            .child(book.author.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_1p5()
                            .py_0p5()
                            .rounded(px(4.))
                            .bg(rgb(palette(cx).accent_soft))
                            .text_xs()
                            .font_medium()
                            .text_color(rgb(palette(cx).accent_strong))
                            .child(book.format.to_ascii_uppercase()),
                    )
                    .when_some(
                        book.group_id
                            .as_deref()
                            .and_then(|group_id| self.library.group(group_id)),
                        |this, group| {
                            this.child(
                                div()
                                    .h_flex()
                                    .w_full()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).accent))
                                    .child(
                                        Icon::new(IconName::Folder)
                                            .small()
                                            .text_color(rgb(palette(cx).accent)),
                                    )
                                    .child(
                                        div().min_w(px(0.)).truncate().child(group.name.clone()),
                                    ),
                            )
                        },
                    ),
            );

        div()
            .relative()
            .w(px(190.))
            .h_auto()
            .flex_none()
            .child(card)
            .child(
                div()
                    .id(SharedString::from(format!("book-group-overlay-{book_id}")))
                    .absolute()
                    .top_2()
                    .right_2()
                    // The folder button sits on top of the card that opens the
                    // book. Stop the propagation at this overlay so pressing it
                    // doesn't also open the book.
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(self.render_book_group_menu(book, cx)),
            )
            .into_any_element()
    }

    // Note: the previous version of this card relied on the outer wrapper being
    // a `relative` block for the floating folder button, and the Button taking
    // `w_full()`. The `items_start` on the Button stops the inner v_flex from
    // stretching, so the cover row and the text column collapsed onto whatever
    // their content sized to. The text column also lacked `w_full`, so it
    // shrank to the longest line and clipped the rest. Forcing both the cover
    // row and the text column to be flex containers with `w_full` makes the
    // children fill the available 178px and the `img().size_full()` actually
    // fills the cover box.
}
