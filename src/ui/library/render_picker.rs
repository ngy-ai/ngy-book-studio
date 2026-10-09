//! The group picker and the group modal.

use super::*;

impl EpubReaderApp {
    /// Body of the "编辑分组" dialog: an indented, scrollable group tree. A row
    /// click only stages the choice; `确定` commits it, so a stray click while
    /// browsing deep groups cannot move the book.
    pub(super) fn render_book_group_picker_body(
        &self,
        book_id: &str,
        book_title: &str,
        selected: &Option<String>,
        expanded: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let ungrouped_selected = selected.is_none();
        let ungrouped_view = view.clone();
        let ungrouped_row = div()
            .id("picker-ungrouped")
            .h_flex()
            .w_full()
            .h(px(32.))
            .items_center()
            .gap_2()
            .px_2()
            .rounded(px(8.))
            .cursor_pointer()
            .when(ungrouped_selected, |this| {
                this.bg(rgb(palette(cx).accent_soft))
            })
            .when(!ungrouped_selected, |this| {
                this.hover(|style| style.bg(Palette::tint(palette(cx).ink, 0.04)))
            })
            .child(
                Icon::new(IconName::Inbox)
                    .small()
                    .text_color(if ungrouped_selected {
                        rgb(palette(cx).accent)
                    } else {
                        rgb(palette(cx).muted)
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .text_color(if ungrouped_selected {
                        rgb(palette(cx).accent_strong)
                    } else {
                        rgb(palette(cx).ink)
                    })
                    .child("未分组"),
            )
            .when(ungrouped_selected, |this| {
                this.child(
                    Icon::new(IconName::Check)
                        .small()
                        .text_color(rgb(palette(cx).accent)),
                )
            })
            .on_click(move |_, _, cx| {
                ungrouped_view.update(cx, |this, cx| this.select_picker_group(None, cx));
            })
            .into_any_element();

        let mut rows = Vec::new();
        self.collect_picker_rows(None, 0, selected, expanded, &mut rows, cx);
        let tree = if rows.is_empty() {
            div()
                .p_3()
                .text_sm()
                .text_color(rgb(palette(cx).muted))
                .child("还没有分组，可用下方「新建分组」创建一个。")
                .into_any_element()
        } else {
            div()
                .v_flex()
                .w_full()
                .gap_0p5()
                .children(rows)
                .into_any_element()
        };

        let new_parent = selected
            .clone()
            .filter(|group_id| self.library.group(group_id).is_some());
        let dirty = selected.as_deref()
            != self
                .library
                .books()
                .iter()
                .find(|book| book.id == book_id)
                .and_then(|book| book.group_id.as_deref());
        let new_group_view = view.clone();
        let cancel_view = view.clone();
        let confirm_view = view.clone();

        div()
            .v_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!("为《{book_title}》选择分组")),
            )
            .child(
                div()
                    .id("book-group-picker-scroll")
                    .w_full()
                    .max_h(px(320.))
                    .overflow_y_scrollbar()
                    .border_1()
                    .border_color(rgb(palette(cx).border))
                    .rounded(px(10.))
                    .bg(rgb(palette(cx).surface))
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .gap_0p5()
                            .p_1()
                            .child(ungrouped_row)
                            .child(tree),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("picker-new-group")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("新建分组")
                            .tooltip(if new_parent.is_some() {
                                "在选中分组下新建子分组"
                            } else {
                                "新建顶级分组"
                            })
                            .on_click(move |_, window, cx| {
                                new_group_view.update(cx, |this, cx| {
                                    this.open_create_group_dialog(new_parent.clone(), window, cx);
                                });
                            }),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("picker-cancel")
                                    .label("取消")
                                    .outline()
                                    .on_click(move |_, _, cx| {
                                        cancel_view
                                            .update(cx, |this, cx| this.close_group_modal(cx));
                                    }),
                            )
                            .child(
                                Button::new("picker-confirm")
                                    .label("确定")
                                    .primary()
                                    .disabled(!dirty)
                                    .on_click(move |_, _, cx| {
                                        confirm_view
                                            .update(cx, |this, cx| this.confirm_group_modal(cx));
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn collect_picker_rows(
        &self,
        parent_id: Option<&str>,
        depth: usize,
        selected: &Option<String>,
        expanded: &HashSet<String>,
        out: &mut Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) {
        for group in self.library.child_groups(parent_id) {
            out.push(self.render_picker_row(group, depth, selected, expanded, cx));
            if expanded.contains(&group.id) {
                self.collect_picker_rows(Some(&group.id), depth + 1, selected, expanded, out, cx);
            }
        }
    }

    pub(super) fn render_picker_row(
        &self,
        group: &BookGroup,
        depth: usize,
        selected: &Option<String>,
        expanded: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let group_id = group.id.clone();
        let has_children = !self.library.child_groups(Some(&group.id)).is_empty();
        let is_expanded = expanded.contains(&group.id);
        let is_selected = selected.as_deref() == Some(group.id.as_str());

        let toggle = if has_children {
            let toggle_view = view.clone();
            let toggle_id = group_id.clone();
            Button::new(SharedString::from(format!("picker-toggle-{}", group.id)))
                .ghost()
                .xsmall()
                .icon(if is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .on_click(move |_, _, cx| {
                    // Expanding a node must not also select it.
                    cx.stop_propagation();
                    toggle_view.update(cx, |this, cx| {
                        this.toggle_picker_group(toggle_id.clone(), cx);
                    });
                })
                .into_any_element()
        } else {
            div().size(px(22.)).flex_none().into_any_element()
        };

        let row_view = view.clone();
        let row_id = group_id.clone();
        div()
            .id(SharedString::from(format!("picker-row-{}", group.id)))
            .h_flex()
            .w_full()
            .h(px(32.))
            .items_center()
            .gap_1p5()
            .pr_2()
            .pl(px(4. + depth as f32 * 16.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(is_selected, |this| this.bg(rgb(palette(cx).accent_soft)))
            .when(!is_selected, |this| {
                this.hover(|style| style.bg(Palette::tint(palette(cx).ink, 0.04)))
            })
            .child(toggle)
            .child(
                Icon::new(if is_selected {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                })
                .small()
                .text_color(if is_selected {
                    rgb(palette(cx).accent)
                } else {
                    rgb(palette(cx).muted)
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .text_color(if is_selected {
                        rgb(palette(cx).accent_strong)
                    } else {
                        rgb(palette(cx).ink)
                    })
                    .child(group.name.clone()),
            )
            .when(is_selected, |this| {
                this.child(
                    Icon::new(IconName::Check)
                        .small()
                        .text_color(rgb(palette(cx).accent)),
                )
            })
            .on_click(move |_, _, cx| {
                row_view.update(cx, |this, cx| {
                    this.select_picker_group(Some(row_id.clone()), cx);
                });
            })
            .into_any_element()
    }

    pub(super) fn render_group_modal(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let modal = self.group_modal.clone()?;
        let view = cx.entity().clone();
        let modal_pending = matches!(
            &modal,
            GroupModal::NameInput {
                pending_request: Some(_),
                ..
            }
        );

        let (title, body) = match &modal {
            GroupModal::NameInput {
                title,
                input,
                confirm_label,
                pending_request,
                error,
                action: _,
                return_to: _,
            } => {
                let title = title.clone();
                let pending = pending_request.is_some();
                let confirm_label = if pending {
                    "处理中…".to_string()
                } else {
                    confirm_label.clone()
                };
                let input = input.clone();
                let error = error.clone();
                let body = div()
                    .w_full()
                    .v_flex()
                    .gap_2()
                    .child(Input::new(&input).disabled(pending))
                    .when_some(error, |body, error| {
                        body.child(
                            div()
                                .text_sm()
                                .text_color(rgb(palette(cx).danger))
                                .child(error),
                        )
                    })
                    .into_any_element();
                let cancel_view = view.clone();
                let confirm_view = view.clone();
                let buttons = div()
                    .h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("modal-cancel")
                            .label("取消")
                            .outline()
                            .disabled(pending)
                            .on_click(move |_, _, cx| {
                                cancel_view.update(cx, |this, cx| this.close_group_modal(cx));
                            }),
                    )
                    .child(
                        Button::new("modal-confirm")
                            .label(confirm_label)
                            .primary()
                            .disabled(pending)
                            .on_click(move |_, _, cx| {
                                confirm_view.update(cx, |this, cx| this.confirm_group_modal(cx));
                            }),
                    );
                (
                    title,
                    div()
                        .v_flex()
                        .gap_4()
                        .child(body)
                        .child(buttons)
                        .into_any_element(),
                )
            }
            GroupModal::DeleteConfirm {
                name, description, ..
            } => {
                let title = format!("删除分组「{name}」？");
                let description = description.clone();
                (
                    title,
                    render_delete_confirm_body(description, &view, palette(cx)).into_any_element(),
                )
            }
            GroupModal::DeleteBook {
                title, description, ..
            } => {
                let title = format!("删除图书「{title}」？");
                let description = description.clone();
                (
                    title,
                    render_delete_confirm_body(description, &view, palette(cx)).into_any_element(),
                )
            }
            GroupModal::OfficeTrust {
                book_id,
                title,
                description,
            } => {
                let title = format!("信任并启用「{title}」的 Office 增强预览？");
                let description = description.clone();
                let book_id = book_id.clone();
                let cancel_view = view.clone();
                let confirm_view = view.clone();
                let body = div()
                    .v_flex()
                    .gap_4()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette(cx).ink))
                            .line_height(gpui::relative(1.6))
                            .child(description),
                    )
                    .child(
                        div()
                            .h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("modal-office-cancel")
                                    .label("取消")
                                    .outline()
                                    .on_click(move |_, _, cx| {
                                        cancel_view
                                            .update(cx, |this, cx| this.close_group_modal(cx));
                                    }),
                            )
                            .child(
                                Button::new("modal-office-confirm")
                                    .label("信任并生成")
                                    .primary()
                                    .on_click(move |_, window, cx| {
                                        confirm_view.update(cx, |this, cx| {
                                            this.group_modal = None;
                                            this.start_office_preview(book_id.clone(), window, cx);
                                        });
                                    }),
                            ),
                    )
                    .into_any_element();
                (title, body)
            }
            GroupModal::BookGroupPicker {
                book_id,
                book_title,
                selected,
                expanded,
            } => (
                "编辑分组".to_string(),
                self.render_book_group_picker_body(book_id, book_title, selected, expanded, cx),
            ),
        };

        let panel_width = match &modal {
            GroupModal::BookGroupPicker { .. } => px(420.),
            _ => px(380.),
        };
        let cancel_view = view.clone();
        Some(
            div()
                .id("group-modal-overlay")
                .absolute()
                .inset_0()
                // Painting the overlay last does not exclude hitboxes from the
                // library below it. Make the modal a real mouse barrier so a
                // dismissal can never continue into a book card.
                .occlude()
                .bg(palette(cx).scrim_with_alpha(0.5))
                .v_flex()
                .items_center()
                .justify_center()
                .on_any_mouse_down(move |event, _, cx| {
                    cx.stop_propagation();
                    if event.button == gpui::MouseButton::Left && !modal_pending {
                        cancel_view.update(cx, |this, cx| this.close_group_modal(cx));
                    }
                })
                .child(
                    div()
                        // Keep clicks inside the panel away from the overlay's
                        // dismissal handler while leaving its controls active.
                        .occlude()
                        .v_flex()
                        .w(panel_width)
                        .p_5()
                        .gap_4()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(rgb(palette(cx).surface))
                        .shadow_lg()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child(title),
                        )
                        .child(body),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_notice(&self, notice: &Notice, theme: Palette) -> gpui::AnyElement {
        let (background, foreground, icon) = if notice.error {
            (
                rgb(theme.danger_soft),
                rgb(theme.danger),
                IconName::TriangleAlert,
            )
        } else {
            (
                rgb(theme.success_soft),
                rgb(theme.success),
                IconName::CircleCheck,
            )
        };
        div()
            .absolute()
            .right_6()
            .bottom(px(STATUS_BAR_HEIGHT + 16.))
            .h_flex()
            .items_start()
            .max_w(px(520.))
            .gap_2()
            .px_4()
            .py_3()
            .rounded(px(10.))
            .border_1()
            .border_color(if notice.error {
                Palette::tint(theme.danger, 0.18)
            } else {
                Palette::tint(theme.success, 0.18)
            })
            .bg(background)
            .text_sm()
            .text_color(foreground)
            .child(Icon::new(icon).small())
            // 分享包导入这类提示可以很长。提示文字作为行的主轴子项时，自动最小尺寸
            // 按整段文字计算，flex 收缩不生效，文字会溢出提示框而不是换行；显式把最小
            // 宽度设为 0 才能让它在 `max_w` 内收缩并按可用宽度折行。
            .child(
                div()
                    .min_w(px(0.))
                    .line_height(gpui::relative(1.4))
                    .child(notice.text.clone()),
            )
            .into_any_element()
    }
}
