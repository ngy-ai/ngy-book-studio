//! The header, the notice banner and the course document button.

use super::*;

impl LearningWindow {
    pub(super) fn open_course_document(
        &mut self,
        document: CourseDocument,
        cx: &mut Context<Self>,
    ) {
        // Keep execution controls, especially cancellation, reachable while a
        // run is active. Document pages intentionally hide those controls.
        if self.busy() {
            return;
        }
        if document == CourseDocument::Answers {
            if self.inputs_disabled() {
                return;
            }
            self.help_level = "S".to_string();
            self.notice = Some(LearningNotice {
                text: "已查看参考解答，本次帮助标为完整示范。请保留查看前的预测与作答。".into(),
                error: false,
            });
        }
        self.study_pane = StudyPane::Document(document);
        cx.notify();
    }

    #[inline(never)]
    pub(super) fn render_document_button(
        &self,
        document: CourseDocument,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity();
        Button::new(SharedString::from(format!(
            "learning-document-{}",
            document.path()
        )))
        .debug_selector(move || format!("learning-document-{}", document.path()))
        .ghost()
        .w_full()
        .justify_start()
        .label(document.title())
        .when(self.study_pane == StudyPane::Document(document), |button| {
            button
                .bg(rgb(palette(cx).accent_soft))
                .text_color(rgb(palette(cx).accent_strong))
        })
        .disabled(self.busy() || (document == CourseDocument::Answers && self.inputs_disabled()))
        .on_click(move |_, _, cx| {
            view.update(cx, |this, cx| this.open_course_document(document, cx));
        })
        .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let save_view = cx.entity();
        let export_view = cx.entity();
        let restore_view = cx.entity();
        let refresh_view = cx.entity();
        let round_view = cx.entity();
        let disabled = self.inputs_disabled();
        let actions = div()
            .id("learning-header-actions")
            .h_flex()
            .min_w(px(0.))
            .gap_2()
            .overflow_x_scroll()
            .child(
                Button::new("learning-save")
                    .outline()
                    .label(format!("保存第{}章记录", self.service.chapter()))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        save_view.update(cx, |this, cx| this.save(window, cx));
                    }),
            )
            .child(
                Button::new("learning-export")
                    .ghost()
                    .icon(IconName::ArrowUp)
                    .label(format!("导出第{}章", self.service.chapter()))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        export_view.update(cx, |this, cx| this.export(window, cx));
                    }),
            )
            .child(
                Button::new("learning-restore")
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .label(format!("恢复第{}章", self.service.chapter()))
                    .tooltip("先保存当前章节，再恢复该章备份；其它章节不受影响")
                    .disabled(
                        self.busy() || (self.reload_required && self.saved_workspace.is_some()),
                    )
                    .on_click(move |_, window, cx| {
                        restore_view.update(cx, |this, cx| this.restore(window, cx));
                    }),
            )
            .child(
                Button::new("learning-new-round")
                    .ghost()
                    .label("新一轮")
                    .tooltip("保存当前代码与笔记，将本轮历史存入档案后开始新一轮")
                    .disabled(disabled || self.history.is_empty())
                    .on_click(move |_, window, cx| {
                        round_view.update(cx, |this, cx| this.new_round(window, cx));
                    }),
            )
            .child(
                Button::new("learning-refresh")
                    .ghost()
                    .icon(IconName::Redo2)
                    .label("重新检测")
                    .tooltip("保存当前编辑并重新检查运行环境")
                    .disabled(self.busy())
                    .on_click(move |_, window, cx| {
                        refresh_view.update(cx, |this, cx| this.load(window, cx));
                    }),
            )
            .into_any_element();
        div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .py_3()
            .bg(rgb(palette(cx).surface))
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .min_w(px(0.))
                    .child(Icon::new(IconName::BookOpen).text_color(rgb(palette(cx).accent)))
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .child(div().text_lg().font_semibold().child("学习 · AI Agent"))
                            .child(div().text_xs().text_color(rgb(palette(cx).muted)).child(
                                match self.study_pane {
                                    StudyPane::Document(document) => document.title(),
                                    _ => chapter_title(self.service.chapter()),
                                },
                            )),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_notice(&self, theme: Palette) -> Option<gpui::AnyElement> {
        let notice = self.notice.as_ref()?;
        Some(
            div()
                .h_flex()
                .flex_none()
                .items_start()
                .gap_2()
                .px_5()
                .py_2()
                .bg(rgb(if notice.error {
                    theme.danger_soft
                } else {
                    theme.accent_soft
                }))
                .text_color(rgb(if notice.error {
                    theme.danger
                } else {
                    theme.accent_strong
                }))
                .text_sm()
                .child(
                    Icon::new(if notice.error {
                        IconName::TriangleAlert
                    } else {
                        IconName::Info
                    })
                    .small(),
                )
                .child(notice.text.clone())
                .into_any_element(),
        )
    }
}
