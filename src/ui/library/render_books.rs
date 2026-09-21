//! The book grid and the individual book card.

use super::*;

impl EpubReaderApp {
    pub(super) fn render_book_grid(
        &self,
        indices: &[usize],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cards = indices
            .iter()
            .map(|index| self.render_book_card(&self.library.books()[*index], cx))
            .collect::<Vec<_>>();

        div()
            .id("library-scroll")
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
    ) -> gpui::AnyElement {
        // 悬停/按下样式是 `'static` 闭包，借用 `cx` 会逃逸，`move` 闭包又会夺走它；
        // 取一次值（`Palette` 是 `Copy`）供整张卡片共用。
        let theme = palette(cx);
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        let cover_bytes = self.library.cover_bytes_cached(&book.id);
        let cover = if let (Some(bytes), Some(format)) = (
            cover_bytes.as_ref(),
            book.cover_mime.as_deref().and_then(image_format_from_mime),
        ) {
            // Binary cover data is hydrated from object storage before render.
            // The image element is laid out by its own bounds, so a `rounded`
            // style on the parent box would clip nothing — the raw pixels are
            // drawn into the parent's corner area. We have to round the image
            // itself so it stays within the 12px corner radius.
            let image = Arc::new(Image::from_bytes(format, (**bytes).clone()));
            img(image)
                .size_full()
                .rounded(px(10.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(rgb(palette(cx).sidebar))
                .text_color(rgb(palette(cx).accent))
                .child(Icon::new(IconName::BookOpen).large())
                .child(
                    div()
                        .text_center()
                        .text_xs()
                        .font_medium()
                        .child(book.format.to_ascii_uppercase()),
                )
                .into_any_element()
        };

        let open_view = view.clone();
        let open_id = book_id.clone();
        let card_id = SharedString::from(format!("book-card-{book_id}"));

        // Right-click menu: open / group / title / author / edit / export /
        // delete. Groups are edited in a tree dialog instead of a flat entry
        // per group, so the menu stays short whatever the library size is.
        let office_supported = is_office_format_name(&book.format);
        let office_running = self
            .office_preview
            .as_ref()
            .is_some_and(|active| active.book_id == book.id);
        let office_enabled = self.office_enabled_books.contains(&book.id);
        let office_title = book.title.clone();

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
            .context_menu({
                let menu_view = view.clone();
                let menu_book_id = book_id.clone();
                move |mut menu, _window, _cx| {
                    let open_view = menu_view.clone();
                    let open_id = menu_book_id.clone();
                    menu = menu
                        .item(
                            PopupMenuItem::new("打开图书")
                                .icon(Icon::new(IconName::BookOpen))
                                .on_click(move |_, window, cx| {
                                    open_view.update(cx, |this, cx| {
                                        this.open_book(open_id.clone(), window, cx)
                                    });
                                }),
                        )
                        .separator();

                    let group_view = menu_view.clone();
                    let group_book_id = menu_book_id.clone();
                    menu = menu
                        .item(
                            PopupMenuItem::new("编辑分组…")
                                .icon(Icon::new(IconName::Folder))
                                .on_click(move |_, _, cx| {
                                    group_view.update(cx, |this, cx| {
                                        this.open_book_group_picker(group_book_id.clone(), cx);
                                    });
                                }),
                        )
                        .separator();

                    let office_view = menu_view.clone();
                    let office_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(if office_running {
                            "取消 Microsoft Office 增强预览"
                        } else if office_enabled {
                            "打开 Microsoft Office 增强预览"
                        } else {
                            "可选：Microsoft Office 增强预览"
                        })
                        .icon(Icon::new(IconName::BookOpen))
                        .disabled(!office_supported)
                        .on_click(move |_, window, cx| {
                            office_view.update(cx, |this, cx| {
                                if office_running {
                                    this.cancel_office_preview(Some(&office_book_id), true, cx);
                                } else if office_enabled {
                                    this.open_persisted_office_preview(
                                        office_book_id.clone(),
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.open_office_trust_dialog(office_book_id.clone(), cx);
                                }
                            });
                        }),
                    );
                    if office_enabled && !office_running {
                        let disable_view = menu_view.clone();
                        let disable_book_id = menu_book_id.clone();
                        let disable_title = office_title.clone();
                        menu = menu.item(
                            PopupMenuItem::new("禁用 Microsoft Office 增强预览")
                                .icon(Icon::new(IconName::Close))
                                .on_click(move |_, _, cx| {
                                    disable_view.update(cx, |this, cx| {
                                        this.disable_office_enhancement(
                                            disable_book_id.clone(),
                                            disable_title.clone(),
                                            true,
                                            cx,
                                        );
                                    });
                                }),
                        );
                    }

                    let edit_view = menu_view.clone();
                    let edit_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("编辑图书")
                            .icon(Icon::new(IconName::Settings2))
                            .on_click(move |_, window, cx| {
                                edit_view.update(cx, |this, cx| {
                                    this.open_book_editor(edit_book_id.clone(), window, cx);
                                });
                            }),
                    );

                    let rename_view = menu_view.clone();
                    let rename_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("修改名称…")
                            .icon(Icon::new(IconName::CaseSensitive))
                            .on_click(move |_, window, cx| {
                                rename_view.update(cx, |this, cx| {
                                    this.open_rename_book_dialog(
                                        rename_book_id.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    );

                    let author_view = menu_view.clone();
                    let author_book_id = menu_book_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("修改作者…")
                            .icon(Icon::new(IconName::User))
                            .on_click(move |_, window, cx| {
                                author_view.update(cx, |this, cx| {
                                    this.open_set_book_author_dialog(
                                        author_book_id.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    );

                    for (label, kind) in [
                        ("导出 EPUB", LibraryExportKind::Epub),
                        ("导出 PDF", LibraryExportKind::Pdf),
                        ("导出原件", LibraryExportKind::Original),
                    ] {
                        let export_view = menu_view.clone();
                        let export_book_id = menu_book_id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .icon(Icon::new(IconName::ExternalLink))
                                .on_click(move |_, window, cx| {
                                    export_view.update(cx, |this, cx| {
                                        this.export_book(export_book_id.clone(), kind, window, cx);
                                    });
                                }),
                        );
                    }

                    let delete_view = menu_view.clone();
                    let delete_book_id = menu_book_id.clone();
                    menu.separator()
                        .item(
                            PopupMenuItem::new("删除图书")
                                .icon(Icon::new(IconName::Delete).text_color(rgb(theme.danger)))
                                .on_click(move |_, window, cx| {
                                    delete_view.update(cx, |this, cx| {
                                        this.open_delete_book_dialog(
                                            delete_book_id.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                        .max_h(px(320.))
                        .scrollable(true)
                }
            })
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
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
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
