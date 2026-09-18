//! The group sidebar and its rows.

use super::*;

impl EpubReaderApp {
    /// The small folder button on a book card: opens the group tree dialog so
    /// the book can be filed under any group, or taken out of every group.
    pub(super) fn render_book_group_menu(
        &self,
        book: &BookRecord,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let book_id = book.id.clone();
        Button::new(SharedString::from(format!("book-group-{book_id}")))
            .ghost()
            .xsmall()
            .icon(IconName::Folder)
            .bg(Palette::tint(palette(cx).surface, 0.9))
            .border_1()
            .border_color(rgb(palette(cx).border))
            .rounded(px(7.))
            .tooltip("设置分组")
            .on_click(move |_, _, cx| {
                // The card underneath opens the book on click; keep this
                // button's own click from reaching it.
                cx.stop_propagation();
                view.update(cx, |this, cx| {
                    this.open_book_group_picker(book_id.clone(), cx);
                });
            })
            .into_any_element()
    }

    pub(super) fn render_group_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().clone();

        let mut rows: Vec<gpui::AnyElement> = vec![
            Button::new("open-learning-center")
                .ghost()
                .icon(IconName::BookOpen)
                .label("学习中心 · AI Agent")
                .tooltip("亲手构建并修好一个资料研究 Agent")
                .w_full()
                .on_click({
                    let view = view.clone();
                    move |_, _, cx| view.update(cx, |this, cx| this.open_learning(cx))
                })
                .into_any_element(),
            Button::new("open-all-notes")
                .ghost()
                .icon(IconName::Menu)
                .label("全部笔记")
                .tooltip("查看所有图书的划线、人工想法和 AI 想法")
                .w_full()
                .on_click({
                    let view = view.clone();
                    move |_, _, cx| view.update(cx, |this, cx| this.open_all_notes(cx))
                })
                .into_any_element(),
            div()
                .h(px(1.))
                .my_2()
                .mx_2()
                .bg(rgb(palette(cx).border))
                .into_any_element(),
            div()
                .h_flex()
                .h(px(28.))
                .px_2()
                .text_xs()
                .font_semibold()
                .text_color(rgb(palette(cx).muted))
                .child("书架")
                .into_any_element(),
        ];
        rows.push(self.render_sidebar_entry(
            "shelf-all",
            IconName::BookOpen,
            "全部图书",
            self.library.books().len(),
            self.group_filter == GroupFilter::All,
            GroupFilter::All,
            cx,
        ));
        rows.push(self.render_sidebar_entry(
            "shelf-ungrouped",
            IconName::Inbox,
            "未分组",
            self.library.ungrouped_book_count(),
            self.group_filter == GroupFilter::Ungrouped,
            GroupFilter::Ungrouped,
            cx,
        ));
        rows.push(
            div()
                .h(px(1.))
                .my_1()
                .mx_2()
                .bg(rgb(palette(cx).border))
                .into_any_element(),
        );

        let new_group_view = view.clone();
        rows.push(
            div()
                .h_flex()
                .h(px(34.))
                .px_2()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .text_xs()
                        .font_semibold()
                        .text_color(rgb(palette(cx).muted))
                        .child(Icon::new(IconName::Folder).small())
                        .child("分组"),
                )
                .child(
                    Button::new("new-root-group")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("新建顶级分组")
                        .on_click(move |_, window, cx| {
                            new_group_view.update(cx, |this, cx| {
                                this.open_create_group_dialog(None, window, cx);
                            });
                        }),
                )
                .into_any_element(),
        );

        let roots = self.library.child_groups(None);
        if roots.is_empty() {
            rows.push(
                div()
                    .px_3()
                    .py_3()
                    .text_xs()
                    .line_height(gpui::relative(1.6))
                    .text_color(rgb(palette(cx).muted))
                    .child("还没有分组，点击右上角 + 新建一个。")
                    .into_any_element(),
            );
        } else {
            for group in roots {
                self.collect_group_rows(group, 0, &mut rows, cx);
            }
        }

        div()
            .v_flex()
            .w(px(240.))
            .h_full()
            .flex_none()
            .p_2()
            .rounded(px(14.))
            .border_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).sidebar))
            .child(
                div()
                    .id("group-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scrollbar()
                    .child(div().v_flex().gap_0p5().pb_2().children(rows)),
            )
            .into_any_element()
    }

    pub(super) fn render_sidebar_entry(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        count: usize,
        selected: bool,
        filter: GroupFilter,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        Button::new(id)
            .ghost()
            .w_full()
            .h(px(34.))
            .justify_start()
            .px_2()
            .gap_2()
            .rounded(px(8.))
            .when(selected, |this| {
                this.bg(rgb(palette(cx).accent_soft))
                    .border_l_2()
                    .border_color(rgb(palette(cx).accent))
            })
            .child(Icon::new(icon).small().text_color(if selected {
                rgb(palette(cx).accent)
            } else {
                rgb(palette(cx).muted)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_left()
                    .truncate()
                    .text_sm()
                    .text_color(if selected {
                        rgb(palette(cx).accent)
                    } else {
                        rgb(palette(cx).ink)
                    })
                    .when(selected, |this| this.font_medium())
                    .child(label),
            )
            .child(
                div()
                    .h_flex()
                    .h(px(20.))
                    .min_w(px(24.))
                    .justify_center()
                    .px(px(6.))
                    .rounded_full()
                    .bg(Palette::tint(palette(cx).surface, 0.6))
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(count.to_string()),
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.select_group(filter.clone(), cx));
            })
            .into_any_element()
    }

    pub(super) fn collect_group_rows(
        &self,
        group: &BookGroup,
        depth: usize,
        rows: &mut Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) {
        rows.push(self.render_group_row(group, depth, cx));
        if self.collapsed_groups.contains(&group.id) {
            return;
        }
        for child in self.library.child_groups(Some(&group.id)) {
            self.collect_group_rows(child, depth + 1, rows, cx);
        }
    }

    pub(super) fn render_group_row(
        &self,
        group: &BookGroup,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let group_id = group.id.clone();
        let has_children = !self.library.child_groups(Some(&group.id)).is_empty();
        let expanded = has_children && !self.collapsed_groups.contains(&group.id);
        let selected = self.group_filter.group_id() == Some(group.id.as_str());
        let count = self.library.group_subtree_book_count(&group.id);
        let row_id = SharedString::from(format!("group-row-{}", group.id));
        let accent = selected.then_some(rgb(palette(cx).accent));

        let toggle = if has_children {
            let toggle_view = view.clone();
            let toggle_id = group_id.clone();
            Button::new(SharedString::from(format!("group-toggle-{}", group.id)))
                .ghost()
                .xsmall()
                .icon(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .on_click(move |_, _, cx| {
                    toggle_view.update(cx, |this, cx| this.toggle_group(toggle_id.clone(), cx));
                })
                .into_any_element()
        } else {
            div().size(px(22.)).flex_none().into_any_element()
        };

        let select_view = view.clone();
        let select_id = group_id.clone();
        let label = Button::new(SharedString::from(format!("group-select-{}", group.id)))
            .ghost()
            .flex_1()
            .min_w(px(0.))
            .h(px(28.))
            .justify_start()
            .px_1()
            .gap_1p5()
            .child(
                Icon::new(if selected {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                })
                .small()
                .text_color(accent.unwrap_or(rgb(palette(cx).muted))),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .text_color(accent.unwrap_or(rgb(palette(cx).ink)))
                    .when(selected, |this| this.font_medium())
                    .child(group.name.clone()),
            )
            .on_click(move |_, _, cx| {
                select_view.update(cx, |this, cx| {
                    this.select_group(GroupFilter::Group(select_id.clone()), cx);
                });
            });

        let menu_view = view.clone();
        let menu_id = group_id.clone();
        let actions = Button::new(SharedString::from(format!("group-actions-{}", group.id)))
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .text_color(rgb(palette(cx).muted))
            .dropdown_menu(move |menu, _, _| {
                let view = menu_view.clone();
                let group_id = menu_id.clone();
                menu.item(
                    PopupMenuItem::new("新建子分组")
                        .icon(Icon::new(IconName::Plus))
                        .on_click({
                            let view = view.clone();
                            let group_id = group_id.clone();
                            move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.open_create_group_dialog(
                                        Some(group_id.clone()),
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                )
                .item(PopupMenuItem::new("重命名").on_click({
                    let view = view.clone();
                    let group_id = group_id.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.open_rename_group_dialog(group_id.clone(), window, cx);
                        });
                    }
                }))
                .separator()
                .item(PopupMenuItem::new("删除分组").on_click({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.open_delete_group_dialog(group_id.clone(), window, cx);
                        });
                    }
                }))
            });

        div()
            .id(row_id)
            .h_flex()
            .w_full()
            .h(px(34.))
            .items_center()
            .gap_0p5()
            .pl(px(4. + depth as f32 * 16.))
            .pr_1()
            .rounded(px(8.))
            .when(selected, |this| {
                this.bg(rgb(palette(cx).accent_soft))
                    .border_l_2()
                    .border_color(rgb(palette(cx).accent))
            })
            .when(!selected, |this| {
                this.hover(|style| style.bg(Palette::tint(palette(cx).ink, 0.04)))
            })
            .child(toggle)
            .child(label)
            .child(
                div()
                    .h_flex()
                    .h(px(20.))
                    .min_w(px(24.))
                    .justify_center()
                    .px(px(6.))
                    .rounded_full()
                    .bg(Palette::tint(palette(cx).surface, 0.6))
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(count.to_string()),
            )
            .child(actions)
            .into_any_element()
    }

    pub(super) fn render_group_actions(
        &self,
        group_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // 分组菜单在 `move` 闭包里构造，捕获 `cx` 会把借用带走；取一次值即可。
        let theme = palette(cx);
        let view = cx.entity().clone();
        let new_child_id = group_id.to_string();
        let new_child_view = view.clone();
        let menu_view = view.clone();
        let menu_group_id = group_id.to_string();

        div()
            .h_flex()
            .gap_2()
            .child(
                Button::new("group-new-child")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label("新建子分组")
                    .on_click(move |_, window, cx| {
                        new_child_view.update(cx, |this, cx| {
                            this.open_create_group_dialog(Some(new_child_id.clone()), window, cx);
                        });
                    }),
            )
            .child(
                Button::new("group-more-actions")
                    .ghost()
                    .small()
                    .icon(IconName::Ellipsis)
                    .tooltip("更多分组操作")
                    .dropdown_menu(move |menu, _, _| {
                        let rename_view = menu_view.clone();
                        let rename_id = menu_group_id.clone();
                        let delete_view = menu_view.clone();
                        let delete_id = menu_group_id.clone();
                        menu.item(
                            PopupMenuItem::new("重命名")
                                .icon(Icon::new(IconName::Settings2))
                                .on_click(move |_, window, cx| {
                                    rename_view.update(cx, |this, cx| {
                                        this.open_rename_group_dialog(
                                            rename_id.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("删除分组")
                                .icon(Icon::new(IconName::Delete).text_color(rgb(theme.danger)))
                                .on_click(move |_, window, cx| {
                                    delete_view.update(cx, |this, cx| {
                                        this.open_delete_group_dialog(
                                            delete_id.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_empty_group(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
                            .child(Icon::new(IconName::FolderOpen).large()),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .text_color(rgb(palette(cx).ink))
                            .child("这个分组还是空的"),
                    )
                    .child(
                        div()
                            .max_w(px(320.))
                            .text_center()
                            .text_sm()
                            .line_height(gpui::relative(1.6))
                            .text_color(rgb(palette(cx).muted))
                            .child("可在图书卡片右上角的文件夹菜单中，将图书移动到这里。"),
                    )
                    .child(
                        Button::new("empty-group-all-books")
                            .outline()
                            .icon(IconName::BookOpen)
                            .label("查看全部图书")
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.select_group(GroupFilter::All, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    }
}
