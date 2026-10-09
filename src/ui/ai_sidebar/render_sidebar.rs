//! Rendering the sidebar chrome: endpoint status, scope, references and the
//! session picker.

use super::*;

impl AiSidebar {
    pub(super) fn render_endpoint_status(&self, theme: Palette) -> gpui_kit::AnyElement {
        let warning = self.endpoint_status.is_warning();
        let (background, foreground, icon) = if warning {
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
            .v_flex()
            .min_h(px(34.))
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(rgb(theme.border))
            .bg(background)
            .text_xs()
            .text_color(foreground)
            .child(
                div()
                    .h_flex()
                    .min_w(px(0.))
                    .gap_1p5()
                    .child(Icon::new(icon).small())
                    .child("OpenAI-compatible"),
            )
            .child(
                div()
                    .w_full()
                    .font_semibold()
                    .child(self.endpoint_status.label()),
            )
            .into_any_element()
    }

    pub(super) fn render_collapsed(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let warning = self.endpoint_status.is_warning();
        let (status_color, status_text) = if warning {
            (palette(cx).danger, "远")
        } else {
            (0x376441, "本")
        };
        div()
            .v_flex()
            .w(px(AI_SIDEBAR_COLLAPSED_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .border_l_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).sidebar))
            .py_3()
            .child(
                Button::new("ai-sidebar-expand")
                    .ghost()
                    .icon(IconName::PanelRightOpen)
                    .tooltip("展开 AI 问答")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle_collapsed(cx));
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .text_color(rgb(palette(cx).accent))
                    .child(Icon::new(IconName::Bot).small()),
            )
            .child(
                div()
                    .v_flex()
                    .mt_3()
                    .items_center()
                    .gap_1()
                    .text_color(rgb(status_color))
                    .child(
                        Icon::new(if warning {
                            IconName::TriangleAlert
                        } else {
                            IconName::CircleCheck
                        })
                        .small(),
                    )
                    .child(div().text_xs().font_semibold().child(status_text)),
            )
            .into_any_element()
    }

    pub(super) fn render_scope(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let selected = self.conversation.scope.selected_books();
        let is_book_scope = matches!(self.conversation.scope, AiSidebarScope::Book { .. });
        let chips = selected
            .iter()
            .enumerate()
            .take(4)
            .map(|(index, book)| {
                let removable = is_book_scope && index > 0;
                let book_id = book.id.clone();
                Button::new(SharedString::from(format!("ai-scope-book-{book_id}")))
                    .xsmall()
                    .ghost()
                    .label(book.title.clone())
                    .when(removable, |button| button.icon(IconName::Close))
                    .when(removable, |button| {
                        let view = view.clone();
                        button.on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.remove_book(&book_id, cx));
                        })
                    })
            })
            .collect::<Vec<_>>();
        let hidden = selected.len().saturating_sub(4);
        let books_to_add = self.conversation.scope.available_books_to_add();
        let add_button = (!books_to_add.is_empty()).then(|| {
            let menu_view = view.clone();
            Button::new("ai-scope-add-book")
                .xsmall()
                .outline()
                .icon(IconName::Plus)
                .label("添加其它图书")
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for book in &books_to_add {
                        let view = menu_view.clone();
                        let book_id = book.id.clone();
                        menu = menu.item(PopupMenuItem::new(book.title.clone()).on_click(
                            move |_, _, cx| {
                                view.update(cx, |this, cx| this.add_book(&book_id, cx));
                            },
                        ));
                    }
                    menu
                })
        });

        div()
            .v_flex()
            .gap_2()
            .px_3()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("图书范围")
                    .child(format!("{} 本", self.conversation.scope.book_ids().len())),
            )
            .child(div().h_flex().flex_wrap().gap_1().children(chips))
            .when(hidden > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child(format!("另有 {hidden} 本图书")),
                )
            })
            .when_some(add_button, |this, button| this.child(button))
            .into_any_element()
    }

    pub(super) fn render_references(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        if self.conversation.reference_hints.is_empty() {
            return None;
        }
        let view = cx.entity().clone();
        let selected_count = self.conversation.selected_reference_count();
        let selection_full = selected_count >= MAX_SELECTED_REFERENCES;
        let reference_count = self.conversation.reference_hints.len();
        let rows = self
            .conversation
            .reference_hints
            .iter()
            .enumerate()
            .map(|(index, reference)| {
                let included = self.conversation.reference_is_included(reference);
                let automatic = self
                    .conversation
                    .automatic_reference
                    .as_ref()
                    .is_some_and(|automatic| automatic.key == reference_key(reference));
                let reference_for_click = reference.clone();
                let view = view.clone();
                div()
                    .h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_sm()
                            .text_color(rgb(palette(cx).ink))
                            .child(reference.label.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("ai-toggle-reference-{index}")))
                            .xsmall()
                            .when(included, |button| button.primary())
                            .when(!included, |button| button.outline())
                            .label(if included { "已引用" } else { "引用" })
                            .disabled(selection_full && !included)
                            .tooltip(if selection_full && !included {
                                format!("最多选择 {MAX_SELECTED_REFERENCES} 个引用")
                            } else if automatic && included {
                                "取消本次高亮引用".to_string()
                            } else if automatic {
                                "重新加入本次高亮引用".to_string()
                            } else if included {
                                "取消引用".to_string()
                            } else {
                                "加入本次问题".to_string()
                            })
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.toggle_reference(&reference_for_click, cx)
                                });
                            }),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .v_flex()
                .gap_2()
                .py_2()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
                .child(
                    div()
                        .h_flex()
                        .justify_between()
                        .gap_2()
                        .px_3()
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(rgb(palette(cx).muted))
                                .child(
                                    Icon::new(IconName::BookOpen)
                                        .small()
                                        .text_color(rgb(palette(cx).accent)),
                                )
                                .child("可选引用"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette(cx).muted))
                                .child(format!(
                                    "已选 {selected_count}/{}",
                                    MAX_SELECTED_REFERENCES
                                )),
                        ),
                )
                .child(
                    div()
                        .v_flex()
                        .max_h(reference_list_max_height(reference_count))
                        .overflow_y_scrollbar()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_session_picker(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        if !self.sessions.open && !self.sessions.busy && self.sessions.error.is_none() {
            return None;
        }

        let view = cx.entity().clone();
        let active_thread_id = self.sessions.active_thread_id.as_deref();
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let session_locked =
            self.sessions.busy || self.pending_scope_sync || self.conversation.is_sending();
        let pending_delete = self.sessions.pending_delete.clone();
        let rows = self
            .sessions
            .threads
            .iter()
            .filter(|thread| thread_scope_is_authorized(thread, &allowed))
            .map(|thread| {
                let thread_id = thread.id.clone();
                let thread_title = thread.title.clone();
                let active = active_thread_id == Some(thread.id.as_str());
                let switch_view = view.clone();
                let delete_view = view.clone();
                let delete_thread_id = thread.id.clone();
                let delete_title = thread_title.clone();
                div()
                    .h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!(
                            "ai-session-option-{}",
                            thread.id
                        )))
                        .ghost()
                        .small()
                        .flex_1()
                        .min_w(px(0.))
                        .when(active, |this| this.icon(IconName::Check))
                        .label(thread_title.clone())
                        .tooltip(thread_title)
                        .disabled(session_locked || active)
                        .on_click(move |_, _, cx| {
                            switch_view.update(cx, |this, cx| {
                                this.switch_session(&thread_id, cx);
                            });
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "ai-session-delete-{}",
                            thread.id
                        )))
                        .ghost()
                        .small()
                        .icon(IconName::Delete)
                        .tooltip(format!("删除会话“{delete_title}”"))
                        .disabled(session_locked)
                        .on_click(move |_, _, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.confirm_delete_session(&delete_thread_id, cx);
                            });
                        }),
                    )
            })
            .collect::<Vec<_>>();

        Some(
            div()
                .v_flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
                .when(self.sessions.busy, |this| {
                    this.child(
                        div()
                            .h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child(Icon::new(IconName::LoaderCircle).xsmall())
                            .child("正在处理会话…"),
                    )
                })
                .when_some(self.sessions.error.clone(), |this, error| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette(cx).danger))
                            .child(error),
                    )
                })
                .when(self.sessions.open, |this| {
                    this.when_some(pending_delete, |this, pending| {
                        let cancel_view = view.clone();
                        let delete_view = view.clone();
                        let thread_id = pending.thread_id.clone();
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .gap_2()
                                .p_3()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(palette(cx).danger))
                                .bg(rgb(palette(cx).danger_soft))
                                .child(div().font_semibold().child("删除 AI 会话？"))
                                .child(div().text_sm().child(format!("会话：“{}”", pending.title)))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(palette(cx).muted))
                                        .child("问题、回答和引用都会被永久删除，且无法恢复。"),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("ai-session-delete-cancel")
                                                .ghost()
                                                .small()
                                                .label("取消")
                                                .on_click(move |_, _, cx| {
                                                    cancel_view.update(cx, |this, cx| {
                                                        this.cancel_delete_session(cx);
                                                    });
                                                }),
                                        )
                                        .child(
                                            Button::new("ai-session-delete-confirm")
                                                .small()
                                                .danger()
                                                .label("永久删除")
                                                .on_click(move |_, _, cx| {
                                                    delete_view.update(cx, |this, cx| {
                                                        this.delete_session(&thread_id, cx);
                                                    });
                                                }),
                                        ),
                                ),
                        )
                    })
                    .when(self.sessions.pending_delete.is_none(), |this| {
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .max_h(px(184.))
                                .overflow_y_scrollbar()
                                .when(rows.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .px_2()
                                            .py_3()
                                            .text_xs()
                                            .text_color(rgb(palette(cx).muted))
                                            .child(
                                                "还没有历史会话。发送第一条消息后会保存在这里。",
                                            ),
                                    )
                                })
                                .children(rows),
                        )
                    })
                })
                .into_any_element(),
        )
    }
}
