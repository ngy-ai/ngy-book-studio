//! The lesson body, its code blocks and the notes pane.

use super::*;

impl LearningWindow {
    #[inline(never)]
    pub(super) fn render_lesson(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let content = match self.lessons.get(self.selected_lesson) {
            Some(lesson) => scrollable_learning_text(
                SharedString::from(format!(
                    "learning-lesson-{}-{}",
                    self.service.chapter(),
                    lesson.id
                )),
                course_display_markdown(&lesson.markdown),
                window,
                cx,
            ),
            None => div()
                .p_5()
                .text_color(rgb(palette(cx).muted))
                .child(if self.operation == Some(Operation::Loading) {
                    "正在读取课程与学习记录…"
                } else {
                    "课程尚未载入，请点击重新检测。"
                })
                .into_any_element(),
        };
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .p_4()
            .child(div().flex_1().min_h(px(0.)).child(content))
            .when(!self.lessons.is_empty(), |body| {
                body.child(self.render_lesson_navigation(cx))
            })
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_code(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let reference_view = cx.entity();
        let copy_view = cx.entity();
        let header = div()
            .h_flex()
            .justify_between()
            .gap_2()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(rgb(palette(cx).muted))
            .child(if self.showing_reference {
                "参考实现 · 只读 · 运行仍使用你的代码"
            } else if self.implementation == "langgraph" {
                "我的实现 · 库版本 Python"
            } else {
                "我的实现 · manual.py"
            })
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .child(
                        Button::new("learning-reference-toggle")
                            .ghost()
                            .small()
                            .icon(IconName::Eye)
                            .label(if self.showing_reference { "返回我的代码" } else { "查看参考" })
                            .disabled(self.inputs_disabled())
                            .on_click(move |_, _, cx| {
                                reference_view.update(cx, |this, cx| {
                                    if this.inputs_disabled() {
                                        return;
                                    }
                                    this.showing_reference = !this.showing_reference;
                                    if this.showing_reference {
                                        this.help_level = "S".to_string();
                                        this.notice = Some(LearningNotice {
                                            text: "已将本次帮助标为完整示范。参考代码只读，你的实现保持在编辑区。".to_string(),
                                            error: false,
                                        });
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .when(self.showing_reference, |row| {
                        row.child(
                            Button::new("learning-reference-copy")
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .label("复制")
                                .disabled(self.inputs_disabled())
                                .on_click(move |_, _, cx| {
                                    copy_view.update(cx, |this, cx| {
                                        if this.inputs_disabled() {
                                            return;
                                        }
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            reference_code_for(this.service.chapter(), &this.implementation).to_string(),
                                        ));
                                        this.help_level = "S".to_string();
                                        cx.notify();
                                    });
                                }),
                        )
                    }),
            )
            .into_any_element();
        let body = if self.showing_reference {
            scrollable_learning_text(
                SharedString::from(format!(
                    "learning-reference-text-{}-{}",
                    self.service.chapter(),
                    self.implementation
                )),
                fenced_text(
                    reference_code_for(self.service.chapter(), &self.implementation),
                    CODE_LANGUAGE,
                ),
                window,
                cx,
            )
        } else {
            let input = if self.implementation == "langgraph" {
                &self.langgraph_input
            } else {
                &self.manual_input
            };
            Input::new(input)
                .h_full()
                .disabled(self.inputs_disabled())
                .into_any_element()
        };
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .child(header)
            .child(div().flex_1().min_h(px(0.)).p_2().child(body))
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_notes(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let menu_view = cx.entity();
        let selected_help = self.help_level.clone();
        let label = HELP_LEVELS
            .iter()
            .find(|(id, _)| *id == selected_help)
            .map_or("选择帮助级别", |(_, label)| *label);
        let help = Button::new("learning-help-level")
            .outline()
            .small()
            .label(label)
            .icon(IconName::ChevronDown)
            .disabled(self.inputs_disabled())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (id, label) in HELP_LEVELS {
                    let view = menu_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(selected_help == id)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if !this.inputs_disabled() {
                                        this.help_level = id.to_string();
                                        cx.notify();
                                    }
                                });
                            }),
                    );
                }
                menu
            });
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_4()
            .child(div().text_sm().font_semibold().child("运行前预测"))
            .child(
                div()
                    .h(px(116.))
                    .flex_none()
                    .child(Input::new(&self.prediction_input).h_full().disabled(self.inputs_disabled())),
            )
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("本章作答与复盘"))
                    .child(help),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(Input::new(&self.notes_input).h_full().disabled(self.inputs_disabled())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("记录首次判断、提示和修改理由。每次运行保存当时的代码、预测及帮助级别；评分仍待评阅。"),
            )
            .into_any_element()
    }
}
