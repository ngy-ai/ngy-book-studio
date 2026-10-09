//! The navigation, study tabs and course document panes.

use super::*;

impl LearningWindow {
    #[inline(never)]
    pub(super) fn render_navigation(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let mut steps = div().v_flex().gap_2();
        for (index, lesson) in self.lessons.iter().enumerate() {
            let view = cx.entity();
            let selected =
                self.selected_lesson == index && !matches!(self.study_pane, StudyPane::Document(_));
            steps = steps.child(
                Button::new(SharedString::from(format!("learning-step-{}", lesson.id)))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .label(lesson.title.clone())
                    .when(selected, |button| {
                        button
                            .bg(rgb(palette(cx).accent_soft))
                            .text_color(rgb(palette(cx).accent_strong))
                    })
                    .disabled(self.inputs_disabled())
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.inputs_disabled() {
                                return;
                            }
                            this.selected_lesson = index;
                            this.study_pane = StudyPane::Lesson;
                            this.showing_reference = false;
                            cx.notify();
                        });
                    }),
            );
        }
        div()
            .v_flex()
            .w(px(244.))
            .flex_none()
            .h_full()
            .min_h(px(0.))
            .bg(rgb(palette(cx).sidebar))
            .border_r_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .px_4()
                    .py_4()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(palette(cx).muted))
                    .child("课程导航"),
            )
            .child(
                div()
                    .id("learning-steps-scroll")
                    .v_flex()
                    .px_2()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(self.render_document_button(CourseDocument::Tutorial, cx))
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child("完整教程 · 十章"),
                    )
                    .children(
                        CourseDocument::CHAPTERS
                            .into_iter()
                            .map(|chapter| self.render_document_button(chapter, cx)),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child(if self.service.chapter() == 1 {
                                "第1章 · 六步练习".to_string()
                            } else {
                                format!("第{}章 · 练习任务", self.service.chapter())
                            }),
                    )
                    .child(steps)
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child("第一章 · 实验资料"),
                    )
                    .children(
                        CourseDocument::MATERIALS
                            .into_iter()
                            .map(|document| self.render_document_button(document, cx)),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap_2()
                    .p_4()
                    .border_t_1()
                    .border_color(rgb(palette(cx).border))
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("手搓 → 框架重建 → 同题比较")
                    .child("先写预测，再看运行。保留第一次判断和使用过的提示。")
                    .child("十章均可进入代码实验；切换实验章节时自动保存当前编辑。各章记录独立。"),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_study_tabs(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let mut tabs = div().h_flex().gap_1().px_3().py_2();
        for (id, label, pane) in [
            ("learning-tab-lesson", "讲义", StudyPane::Lesson),
            ("learning-tab-code", "代码实验", StudyPane::Code),
            ("learning-tab-notes", "预测与作答", StudyPane::Notes),
        ] {
            let view = cx.entity();
            let selected = self.study_pane == pane;
            tabs = tabs.child(
                Button::new(id)
                    .ghost()
                    .label(label)
                    .when(selected, |button| {
                        button
                            .bg(rgb(palette(cx).accent_soft))
                            .text_color(rgb(palette(cx).accent_strong))
                    })
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.study_pane = pane;
                            cx.notify();
                        });
                    }),
            );
        }
        let view = cx.entity();
        let chapter = self.service.chapter();
        tabs.child(
            Button::new("learning-current-chapter-text")
                .debug_selector(|| "learning-current-chapter-text".into())
                .ghost()
                .label("本章正文")
                .disabled(self.busy())
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.open_course_document(
                            CourseDocument::CHAPTERS[usize::from(chapter - 1)],
                            cx,
                        );
                    });
                }),
        )
        .border_b_1()
        .border_color(rgb(palette(cx).border))
        .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_chapter_navigation(
        &self,
        document: CourseDocument,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mut navigation = div().h_flex().gap_2();
        if let Some(index) = document.chapter_index() {
            for (id, label, destination) in [
                (
                    "learning-previous-chapter",
                    "上一章",
                    index
                        .checked_sub(1)
                        .map(|previous| CourseDocument::CHAPTERS[previous]),
                ),
                (
                    "learning-next-chapter",
                    "下一章",
                    CourseDocument::CHAPTERS.get(index + 1).copied(),
                ),
            ] {
                if let Some(destination) = destination {
                    let view = cx.entity();
                    navigation = navigation.child(
                        Button::new(id)
                            .debug_selector(move || id.into())
                            .ghost()
                            .label(label)
                            .tooltip(destination.title())
                            .disabled(self.busy())
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.open_course_document(destination, cx)
                                });
                            }),
                    );
                }
            }
        }
        navigation.into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_course_document(
        &self,
        document: CourseDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let guide_view = cx.entity();
        let continue_view = cx.entity();
        let chapter = document
            .chapter_index()
            .map_or(self.service.chapter(), |index| index as u8 + 1);
        let resume = chapter == self.service.chapter()
            && (self.selected_lesson > 0
                || self.dirty(cx)
                || self
                    .saved_workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.revision > 0 || workspace.lesson_index > 0));
        let actions = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .when(document != CourseDocument::Tutorial, |row| {
                row.child(
                    Button::new("learning-back-to-guide")
                        .debug_selector(|| "learning-back-to-guide".into())
                        .ghost()
                        .label("教程总览")
                        .disabled(self.busy())
                        .on_click(move |_, _, cx| {
                            guide_view.update(cx, |this, cx| {
                                this.open_course_document(CourseDocument::Tutorial, cx)
                            });
                        }),
                )
            })
            .when(
                document == CourseDocument::Tutorial || document.chapter_index().is_some(),
                |row| {
                    row.child(
                        Button::new("learning-start-or-continue")
                            .debug_selector(move || {
                                if chapter > 1 {
                                    format!("learning-enter-chapter-{chapter}")
                                } else if resume {
                                    "learning-continue-first-chapter".into()
                                } else {
                                    "learning-start-first-chapter".into()
                                }
                            })
                            .primary()
                            .label(if document == CourseDocument::Tutorial && chapter > 1 {
                                format!("继续第{chapter}章实验")
                            } else if chapter > 1 {
                                "本章代码实验".to_string()
                            } else if resume {
                                "继续第一章".to_string()
                            } else {
                                "开始第一章".to_string()
                            })
                            .disabled(self.chapter_entry_disabled())
                            .on_click(move |_, window, cx| {
                                continue_view.update(cx, |this, cx| {
                                    this.enter_chapter(chapter, window, cx);
                                });
                            }),
                    )
                },
            )
            .child(self.render_chapter_navigation(document, cx))
            .into_any_element();
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .child(actions)
            .child(div().px_4().pt_2().text_xs().text_color(rgb(palette(cx).muted)).child(
                if document.chapter_index().is_some_and(|index| index > 0) {
                    "读完本章后进入“本章代码实验”：先预测，再编写手搓与库版本，查看宿主检查和运行轨迹。"
                } else {
                    "各章与第一章六步讲义可从左侧打开；外部链接均为可选延伸阅读。"
                },
            ))
            .child(div().flex_1().min_h(px(0.)).min_w(px(0.)).p_4().child(
                scrollable_learning_text(
                    SharedString::from(format!("learning-document-text-{}", document.path())),
                    course_display_markdown(document.markdown()),
                    window,
                    cx,
                ),
            ))
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_lesson_navigation(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let previous_view = cx.entity();
        let next_view = cx.entity();
        div()
            .h_flex()
            .flex_none()
            .justify_between()
            .items_center()
            .border_t_1()
            .border_color(rgb(palette(cx).border))
            .pt_2()
            .child(
                Button::new("learning-previous-lesson")
                    .debug_selector(|| "learning-previous-lesson".into())
                    .ghost()
                    .label("上一节")
                    .disabled(self.inputs_disabled() || self.selected_lesson == 0)
                    .on_click(move |_, _, cx| {
                        previous_view.update(cx, |this, cx| {
                            if this.inputs_disabled() || this.selected_lesson == 0 {
                                return;
                            }
                            this.selected_lesson -= 1;
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child(format!(
                        "第{}章 · 第 {} / {} 节",
                        self.service.chapter(),
                        self.selected_lesson + 1,
                        self.lessons.len(),
                    )),
            )
            .child(
                Button::new("learning-next-lesson")
                    .debug_selector(|| "learning-next-lesson".into())
                    .ghost()
                    .label(if self.selected_lesson + 1 < self.lessons.len() {
                        "下一节".to_string()
                    } else if self.service.chapter() == 10 {
                        "教程总览".to_string()
                    } else {
                        format!("进入第{}章", self.service.chapter() + 1)
                    })
                    .disabled(self.inputs_disabled())
                    .on_click(move |_, _, cx| {
                        next_view.update(cx, |this, cx| {
                            if this.inputs_disabled() {
                                return;
                            }
                            if this.selected_lesson + 1 < this.lessons.len() {
                                this.selected_lesson += 1;
                            } else {
                                let destination = CourseDocument::CHAPTERS
                                    .get(usize::from(this.service.chapter()))
                                    .copied()
                                    .unwrap_or(CourseDocument::Tutorial);
                                this.open_course_document(destination, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }
}
