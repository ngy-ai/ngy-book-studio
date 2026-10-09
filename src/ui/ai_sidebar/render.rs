//! The rendered sidebar and collapsed rail.

use super::*;

impl Render for AiSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.layout.collapsed {
            return self.render_collapsed(cx);
        }

        let view = cx.entity().clone();
        let collapse_view = view.clone();
        let sessions_view = view.clone();
        let new_session_view = view.clone();
        let sending = self.conversation.is_sending();
        let session_busy = self.sessions.busy || self.pending_scope_sync;
        let active_session_title = self.sessions.active_title().to_string();
        let session_picker_tooltip = format!("切换会话 · {}", self.conversation.scope.label());
        let question_empty = self.input.read(cx).value().trim().is_empty();
        let messages = self
            .conversation
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| self.render_message(index, message, window, cx))
            .collect::<Vec<_>>();
        let send_view = view.clone();
        let cancel_view = view.clone();

        div()
            .v_flex()
            .w(self.expanded_width)
            .h_full()
            .min_h(px(0.))
            .flex_none()
            .border_l_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).sidebar))
            .child(
                div()
                    .h_flex()
                    .h(px(52.))
                    .flex_none()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(palette(cx).border))
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .gap_2()
                            .text_color(rgb(palette(cx).ink))
                            .child(
                                Icon::new(IconName::Bot)
                                    .small()
                                    .text_color(rgb(palette(cx).accent)),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .min_w(px(0.))
                                    .child(div().text_sm().font_semibold().child("AI 问答"))
                                    .child(
                                        div()
                                            .max_w(px(250.))
                                            .truncate()
                                            .text_xs()
                                            .text_color(rgb(palette(cx).muted))
                                            .child(active_session_title),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .child(
                                Button::new("ai-session-list")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::GalleryVerticalEnd)
                                    .tooltip(session_picker_tooltip)
                                    .disabled(sending || session_busy)
                                    .on_click(move |_, _, cx| {
                                        sessions_view
                                            .update(cx, |this, cx| this.toggle_session_picker(cx));
                                    }),
                            )
                            .child(
                                Button::new("ai-new-session")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Plus)
                                    .tooltip("新建会话")
                                    .disabled(sending || session_busy)
                                    .on_click(move |_, _, cx| {
                                        new_session_view
                                            .update(cx, |this, cx| this.new_session(cx));
                                    }),
                            )
                            .child(
                                Button::new("ai-sidebar-collapse")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::PanelRightClose)
                                    .tooltip("收起 AI 问答")
                                    .on_click(move |_, _, cx| {
                                        collapse_view
                                            .update(cx, |this, cx| this.toggle_collapsed(cx));
                                    }),
                            ),
                    ),
            )
            .when_some(self.render_session_picker(cx), |this, picker| {
                this.child(picker)
            })
            .child(self.render_endpoint_status(palette(cx)))
            .child(self.render_scope(cx))
            .when_some(self.render_references(cx), |this, references| {
                this.child(references)
            })
            .child(
                div()
                    .id("ai-message-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scrollbar()
                    .p_3()
                    .when(messages.is_empty(), |this| {
                        this.child(
                            div()
                                .v_flex()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .py_8()
                                .text_center()
                                .text_color(rgb(palette(cx).muted))
                                .child(Icon::new(IconName::Bot).large())
                                .child(div().text_sm().child("围绕当前图书范围提问"))
                                .child(div().text_xs().line_height(gpui_kit::relative(1.5)).child(
                                    "回答将显示可点击来源；图书内容只通过宿主授权的只读工具读取。",
                                )),
                        )
                    })
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .min_w(px(0.))
                            .gap_3()
                            .children(messages),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_none()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(palette(cx).border))
                    .bg(rgb(palette(cx).surface))
                    .child(Textarea::new(&self.input).disabled(sending || session_busy))
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .gap_2()
                            .child(div().text_xs().text_color(rgb(palette(cx).muted)).child(
                                if sending {
                                    "正在生成，可随时取消"
                                } else if session_busy {
                                    "正在切换会话"
                                } else {
                                    "Ctrl+Enter 发送"
                                },
                            ))
                            .when(!sending, |this| {
                                this.child(
                                    Button::new("ai-send")
                                        .small()
                                        .primary()
                                        .icon(IconName::ArrowUp)
                                        .label("发送")
                                        .disabled(question_empty || session_busy)
                                        .on_click(move |_, window, cx| {
                                            send_view
                                                .update(cx, |this, cx| this.submit(window, cx));
                                        }),
                                )
                            })
                            .when(sending, |this| {
                                this.child(
                                    Button::new("ai-cancel")
                                        .small()
                                        .outline()
                                        .icon(IconName::CircleX)
                                        .label("取消")
                                        .on_click(move |_, _, cx| {
                                            cancel_view.update(cx, |this, cx| this.cancel(cx));
                                        }),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}
