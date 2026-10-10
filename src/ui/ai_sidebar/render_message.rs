//! Rendering one message.

use super::*;

/// The chat action icons this project maintains itself (see `ui::assets`).
/// GPUI Component's fixed `IconName` set has no pencil or circular-arrow
/// glyph, and a wrong existing icon reads worse than a dedicated one.
const EDIT_ICON: &str = "icons/pencil.svg";
const REGENERATE_ICON: &str = "icons/refresh-cw.svg";
const READ_ALOUD_ICON: &str = "icons/volume-2.svg";
const STOP_READ_ALOUD_ICON: &str = "icons/square.svg";

impl AiSidebar {
    pub(super) fn render_message(
        &self,
        index: usize,
        message: &AiMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let user = message.role == AiMessageRole::User;
        let show_no_source_warning = should_show_no_knowledge_base_source_warning(message);
        // 回答卡片必须是皮肤色：写死的 `white()` 背景配 `ink` 文字，在深色皮肤里
        // 就是"浅底浅字"（回答几乎看不见），浅色皮肤里也只是看不出卡片层次。
        // 用户气泡用 `paper` 而不是白色文字，理由与 `theme.primary_foreground`
        // 相同：两套皮肤下它离 `accent` 都比白色更远。
        let (background, foreground): (gpui_kit::Hsla, gpui_kit::Hsla) = match message.role {
            AiMessageRole::User => (
                rgb(palette(cx).accent).into(),
                rgb(palette(cx).paper).into(),
            ),
            AiMessageRole::Assistant => {
                (rgb(palette(cx).surface).into(), rgb(palette(cx).ink).into())
            }
            AiMessageRole::Status => (
                rgb(palette(cx).neutral_soft).into(),
                rgb(palette(cx).muted).into(),
            ),
            AiMessageRole::Error => (
                rgb(palette(cx).danger_soft).into(),
                rgb(palette(cx).danger).into(),
            ),
        };
        let sources = message
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let source = source.clone();
                let source_event = source.clone();
                let mut label = if source.label.trim().is_empty() {
                    format!("来源 {}", index + 1)
                } else {
                    source.label.clone()
                };
                if source.stale {
                    label.push_str("（引用已失效）");
                }
                Button::new(SharedString::from(format!(
                    "ai-message-source-{}-{index}",
                    message.id
                )))
                .xsmall()
                .ghost()
                .icon(IconName::BookOpen)
                .label(label)
                .tooltip(if source.stale {
                    "该来源对应的文档或内容版本已变化，无法继续跳转。".to_string()
                } else {
                    source.quote.unwrap_or_else(|| "打开来源".to_string())
                })
                .on_click({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |_, cx| {
                            cx.emit(AiSidebarEvent::OpenSource(source_event.clone()));
                        });
                    }
                })
            })
            .collect::<Vec<_>>();
        let message_max_width = (self.expanded_width - px(24.)).max(px(0.));

        div()
            .v_flex()
            .w_full()
            .min_w(px(0.))
            .items_start()
            .when(user, |this| this.items_end())
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .min_w(px(0.))
                    .max_w(message_max_width)
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if user {
                        rgb(palette(cx).accent)
                    } else {
                        rgb(palette(cx).border)
                    })
                    .bg(background)
                    .text_sm()
                    .text_color(foreground)
                    .when(show_no_source_warning, |this| {
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .gap_1()
                                .px_2()
                                .py_2()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(palette(cx).warning))
                                .bg(rgb(palette(cx).warning_soft))
                                .text_color(rgb(palette(cx).warning))
                                .child(
                                    div()
                                        .h_flex()
                                        .gap_1()
                                        .font_semibold()
                                        .child(Icon::new(IconName::TriangleAlert).small())
                                        .child(NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .line_height(gpui_kit::relative(1.5))
                                        .child(NO_KNOWLEDGE_BASE_SOURCE_WARNING_BODY),
                                ),
                        )
                    })
                    .when_some(source_basis_badge(message), |this, (title, body)| {
                        this.child(
                            div()
                                .v_flex()
                                .w_full()
                                .gap_0p5()
                                .px_2()
                                .py_2()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(palette(cx).info))
                                .bg(rgb(palette(cx).info_soft))
                                .text_color(rgb(palette(cx).info))
                                .child(
                                    div()
                                        .h_flex()
                                        .gap_1()
                                        .font_semibold()
                                        .child(Icon::new(IconName::CircleCheck).small())
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .line_height(gpui_kit::relative(1.5))
                                        .child(body),
                                ),
                        )
                    })
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .items_start()
                            .line_height(gpui_kit::relative(1.5))
                            .child(div().min_w(px(0.)).flex_1().child(selectable_message_text(
                                SharedString::from(format!("ai-message-text-{}", message.id)),
                                message.role,
                                &message.content,
                                message_text_width(self.expanded_width, window.rem_size()),
                                window,
                                cx,
                            ))),
                    )
                    .when(!sources.is_empty(), |this| {
                        this.child(div().v_flex().items_start().gap_1().children(sources))
                    }),
            )
            .when_some(
                self.render_message_actions(index, message, cx),
                |this, actions| this.child(actions),
            )
            .into_any_element()
    }

    /// The action row under one bubble: copy and read aloud on every message,
    /// plus edit and delete for a user question, plus regenerate for an
    /// assistant answer.
    ///
    /// Status and error notices carry no actions. While a request is running
    /// (or the session is changing) the mutating actions are disabled, because
    /// editing, regenerating or deleting rewrites the stored thread and must
    /// never race an answer that is still being written. Copying and reading
    /// stay available.
    fn render_message_actions(
        &self,
        index: usize,
        message: &AiMessage,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let busy = self.conversation.is_sending() || self.sessions.busy || self.pending_scope_sync;
        let mut row = div().h_flex().gap_0p5().items_center().pt_1().child(
            Clipboard::new(SharedString::from(format!(
                "ai-message-copy-{}",
                message.id
            )))
            .value(message.content.clone()),
        );
        // Reading the message currently being read aloud stops it; any other
        // message (including the other role's) starts a fresh session. The
        // icon doubles as the state indicator, because the sidebar has no
        // per-paragraph highlight.
        let reading = self.is_reading_message(message.id);
        let view = cx.entity().clone();
        let selector = format!("ai-message-read-aloud-{}", message.id);
        let message_id = message.id;
        let role = message.role;
        let content = message.content.clone();
        row = row.child(
            Button::new(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .xsmall()
                .ghost()
                .icon(Icon::empty().path(if reading {
                    STOP_READ_ALOUD_ICON
                } else {
                    READ_ALOUD_ICON
                }))
                .tooltip(if reading {
                    "停止朗读"
                } else {
                    "朗读这条消息"
                })
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.toggle_message_read_aloud(message_id, role, &content, cx);
                    });
                }),
        );
        match message.role {
            AiMessageRole::User => {
                let view = cx.entity().clone();
                let selector = format!("ai-message-edit-{}", message.id);
                row = row.child(
                    Button::new(SharedString::from(selector.clone()))
                        .debug_selector(move || selector)
                        .xsmall()
                        .ghost()
                        .icon(Icon::empty().path(EDIT_ICON))
                        .tooltip("修改这条提问并重新发送")
                        .disabled(busy)
                        .on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| this.edit_message(index, window, cx));
                        }),
                );
                let view = cx.entity().clone();
                let selector = format!("ai-message-delete-{}", message.id);
                row = row.child(
                    Button::new(SharedString::from(selector.clone()))
                        .debug_selector(move || selector)
                        .xsmall()
                        .ghost()
                        .icon(IconName::Delete)
                        .tooltip("删除这条提问和它的回答")
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.delete_turn(index, cx));
                        }),
                );
            }
            AiMessageRole::Assistant => {
                let view = cx.entity().clone();
                let selector = format!("ai-message-regenerate-{}", message.id);
                row = row.child(
                    Button::new(SharedString::from(selector.clone()))
                        .debug_selector(move || selector)
                        .xsmall()
                        .ghost()
                        .icon(Icon::empty().path(REGENERATE_ICON))
                        .tooltip("重新生成这条回答（它之后的对话会被替换）")
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.regenerate_message(index, cx));
                        }),
                );
            }
            AiMessageRole::Status | AiMessageRole::Error => return None,
        }
        Some(row.into_any_element())
    }
}
