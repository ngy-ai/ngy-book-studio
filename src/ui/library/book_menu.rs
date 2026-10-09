//! The book context menu, shared by the card grid and the list rows so both
//! surfaces always offer the same actions on a book.

use super::*;

impl EpubReaderApp {
    /// Right-click menu of one book: open / group / Office preview / edit /
    /// rename / author / export / share / delete. Groups are edited in a tree
    /// dialog instead of a flat entry per group, so the menu stays short
    /// whatever the library size is.
    pub(super) fn book_context_menu(
        &self,
        book: &BookRecord,
        cx: &mut Context<Self>,
    ) -> impl Fn(
        gpui_kit::component::menu::PopupMenu,
        &mut Window,
        &mut gpui_kit::Context<gpui_kit::component::menu::PopupMenu>,
    ) -> gpui_kit::component::menu::PopupMenu
    + 'static {
        // 菜单在 `move` 闭包里构造，闭包要求 `'static`，借用 `cx` 会逃逸；
        // 取一次值（`Palette` 是 `Copy`）供整份菜单共用。
        let theme = palette(cx);
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        let office_supported = is_office_format_name(&book.format);
        let office_running = self
            .office_preview
            .as_ref()
            .is_some_and(|active| active.book_id == book.id);
        let office_enabled = self.office_enabled_books.contains(&book.id);
        let office_title = book.title.clone();

        move |mut menu, _window, _cx| {
            let open_view = view.clone();
            let open_id = book_id.clone();
            menu = menu
                .item(
                    PopupMenuItem::new("打开图书")
                        .icon(Icon::new(IconName::BookOpen))
                        .on_click(move |_, window, cx| {
                            open_view
                                .update(cx, |this, cx| this.open_book(open_id.clone(), window, cx));
                        }),
                )
                .separator();

            let group_view = view.clone();
            let group_book_id = book_id.clone();
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

            let office_view = view.clone();
            let office_book_id = book_id.clone();
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
                            this.open_persisted_office_preview(office_book_id.clone(), window, cx);
                        } else {
                            this.open_office_trust_dialog(office_book_id.clone(), cx);
                        }
                    });
                }),
            );
            if office_enabled && !office_running {
                let disable_view = view.clone();
                let disable_book_id = book_id.clone();
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

            let edit_view = view.clone();
            let edit_book_id = book_id.clone();
            menu = menu.item(
                PopupMenuItem::new("编辑图书")
                    .icon(Icon::new(IconName::Settings2))
                    .on_click(move |_, window, cx| {
                        edit_view.update(cx, |this, cx| {
                            this.open_book_editor(edit_book_id.clone(), window, cx);
                        });
                    }),
            );

            let rename_view = view.clone();
            let rename_book_id = book_id.clone();
            menu = menu.item(
                PopupMenuItem::new("修改名称…")
                    .icon(Icon::new(IconName::CaseSensitive))
                    .on_click(move |_, window, cx| {
                        rename_view.update(cx, |this, cx| {
                            this.open_rename_book_dialog(rename_book_id.clone(), window, cx);
                        });
                    }),
            );

            let author_view = view.clone();
            let author_book_id = book_id.clone();
            menu = menu.item(
                PopupMenuItem::new("修改作者…")
                    .icon(Icon::new(IconName::User))
                    .on_click(move |_, window, cx| {
                        author_view.update(cx, |this, cx| {
                            this.open_set_book_author_dialog(author_book_id.clone(), window, cx);
                        });
                    }),
            );

            for (label, kind) in [
                ("导出 EPUB", LibraryExportKind::Epub),
                ("导出 PDF", LibraryExportKind::Pdf),
                ("导出原件", LibraryExportKind::Original),
            ] {
                let export_view = view.clone();
                let export_book_id = book_id.clone();
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

            let share_view = view.clone();
            let share_book_id = book_id.clone();
            menu = menu.item(
                PopupMenuItem::new("导出分享包…")
                    .icon(Icon::new(IconName::ExternalLink))
                    .on_click(move |_, window, cx| {
                        share_view.update(cx, |this, cx| {
                            this.export_share_package(share_book_id.clone(), window, cx);
                        });
                    }),
            );

            let delete_view = view.clone();
            let delete_book_id = book_id.clone();
            menu.separator()
                .item(
                    PopupMenuItem::new("删除图书")
                        .icon(Icon::new(IconName::Delete).text_color(rgb(theme.danger)))
                        .on_click(move |_, window, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.open_delete_book_dialog(delete_book_id.clone(), window, cx);
                            });
                        }),
                )
                .max_h(px(320.))
                .scrollable(true)
        }
    }
}
