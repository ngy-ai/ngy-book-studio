//! The center/evidence split and the rendered learning window.

use super::*;

impl LearningWindow {
    #[inline(never)]
    pub(super) fn render_center(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        if let StudyPane::Document(document) = self.study_pane {
            return div()
                .v_flex()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .h_full()
                .bg(rgb(palette(cx).surface))
                .child(self.render_course_document(document, window, cx))
                .child(self.render_environment(cx))
                .into_any_element();
        }
        let pane = match self.study_pane {
            StudyPane::Document(_) => unreachable!("course documents are rendered above"),
            StudyPane::Lesson => self.render_lesson(window, cx),
            StudyPane::Code => self.render_code(window, cx),
            StudyPane::Notes => self.render_notes(cx),
        };
        div()
            .v_flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .h_full()
            .bg(rgb(palette(cx).surface))
            .child(self.render_study_tabs(cx))
            .child(pane)
            .child(self.render_run_controls(cx))
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn render_evidence(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let pane = match self.evidence_pane {
            EvidencePane::Result => self.render_result(window, cx),
            EvidencePane::Trace => self.render_trace(window, cx),
            EvidencePane::Attempt => self.render_attempt(window, cx),
            EvidencePane::History => self.render_history(cx),
        };
        div()
            .v_flex()
            .w(px(350.))
            .flex_none()
            .h_full()
            .min_h(px(0.))
            .bg(rgb(palette(cx).paper))
            .border_l_1()
            .border_color(rgb(palette(cx).border))
            .child(self.render_evidence_tabs(cx))
            .child(div().v_flex().flex_1().min_h(px(0.)).child(pane))
            .child(self.render_environment(cx))
            .into_any_element()
    }
}

impl Render for LearningWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let navigation = self.render_navigation(cx);
        let center = self.render_center(window, cx);
        let evidence = (!matches!(self.study_pane, StudyPane::Document(_)))
            .then(|| self.render_evidence(window, cx));
        let status = if self.closing {
            "关闭前等待保存与取消完成"
        } else if self.reload_required {
            "工作区版本尚未确认，请重新检测"
        } else if self.running {
            "实验运行中"
        } else if self.operation.is_some() {
            "正在处理学习记录"
        } else if self.dirty(cx) {
            "有未保存的修改"
        } else if self.saved_workspace.is_some() {
            "学习记录已保存"
        } else {
            "学习工作区未载入"
        };
        div()
            .v_flex()
            .size_full()
            .min_h(px(0.))
            .text_color(rgb(palette(cx).ink))
            .bg(rgb(palette(cx).paper))
            .child(header)
            .when_some(self.render_notice(palette(cx)), |root, notice| {
                root.child(notice)
            })
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(navigation)
                    .child(center)
                    .when_some(evidence, |row, evidence| row.child(evidence)),
            )
            .child(render_status_bar(
                IconName::BookOpen,
                status.to_string(),
                palette(cx).muted,
                match self.study_pane {
                    StudyPane::Document(document) => document.title().to_string(),
                    _ => format!(
                        "第{}章 · 第 {} / {} 步 · {}",
                        self.service.chapter(),
                        self.selected_lesson + 1,
                        self.lessons.len().max(1),
                        implementation_label(self.service.chapter(), &self.implementation)
                    ),
                },
                "本章学习证据待评阅".to_string(),
                palette(cx),
            ))
    }
}
