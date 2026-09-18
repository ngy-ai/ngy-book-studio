//! The overwrite confirmation.

use super::*;

impl SystemSettingsWindow {
    /// 目标目录里已经有一个图书库时，先在这里要一次明确的覆盖确认。
    pub(crate) fn render_confirm_overwrite(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let change = self.confirm_overwrite.clone()?;
        let theme = palette(cx);
        Some(
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .bg(rgb(theme.danger_soft))
                .child(
                    div()
                        .text_sm()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(theme.danger))
                        .child(format!(
                            "{} 已经是一个图书库。继续会先删掉它的内容，再把当前数据目录的内容\
                             整体搬过去 —— 这一步不可撤销。",
                            change.target.display()
                        )),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            Button::new("ai-data-directory-overwrite")
                                .label("覆盖并重启")
                                .primary()
                                .debug_selector(|| "ai-data-directory-overwrite".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let Some(change) = this.confirm_overwrite.clone() else {
                                        return;
                                    };
                                    this.commit_data_directory_change(change, true, cx);
                                })),
                        )
                        .child(
                            Button::new("ai-data-directory-overwrite-cancel")
                                .label("取消")
                                .outline()
                                .debug_selector(|| "ai-data-directory-overwrite-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_overwrite = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}
