//! Rendering one message.

use super::*;

impl AiSidebar {
    pub(super) fn render_message(
        &self,
        message: &AiMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity().clone();
        let user = message.role == AiMessageRole::User;
        let show_no_source_warning = should_show_no_knowledge_base_source_warning(message);
        // 回答卡片必须是皮肤色：写死的 `white()` 背景配 `ink` 文字，在深色皮肤里
        // 就是"浅底浅字"（回答几乎看不见），浅色皮肤里也只是看不出卡片层次。
        // 用户气泡用 `paper` 而不是白色文字，理由与 `theme.primary_foreground`
        // 相同：两套皮肤下它离 `accent` 都比白色更远。
        let (background, foreground): (gpui::Hsla, gpui::Hsla) = match message.role {
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
                                        .line_height(gpui::relative(1.5))
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
                                    div().text_xs().line_height(gpui::relative(1.5)).child(body),
                                ),
                        )
                    })
                    .child(
                        div()
                            .h_flex()
                            .min_w(px(0.))
                            .items_start()
                            .gap_1()
                            .line_height(gpui::relative(1.5))
                            .child(div().min_w(px(0.)).flex_1().child(selectable_message_text(
                                SharedString::from(format!("ai-message-text-{}", message.id)),
                                message.role,
                                &message.content,
                                message_text_width(self.expanded_width, window.rem_size()),
                                window,
                                cx,
                            )))
                            .child(
                                div().w(px(24.)).flex_none().child(
                                    Clipboard::new(SharedString::from(format!(
                                        "ai-message-copy-{}",
                                        message.id
                                    )))
                                    .value(message.content.clone()),
                                ),
                            ),
                    )
                    .when(!sources.is_empty(), |this| {
                        this.child(div().v_flex().items_start().gap_1().children(sources))
                    }),
            )
            .into_any_element()
    }
}
