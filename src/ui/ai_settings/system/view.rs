//! Window traits and the storage path row helper.

use super::*;

impl SettingsWindowView for SystemSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::System
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().system
    }

    fn state(&self) -> &SettingsWindowState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SettingsWindowState {
        &mut self.state
    }

    fn services(&self) -> &Arc<AppServices> {
        &self.services
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let settings = match self.entered_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.state.report(format!("系统配置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            std::collections::BTreeMap::new(),
            ApiKeyUpdate::Keep,
            None,
            window,
            cx,
        );
    }

    fn apply_saved_preferences(&mut self, cx: &mut App) {
        // 提交成功后立刻把这三项推给已经打开的窗口；写入本身才是唯一事实来源。
        apply_pdf_compact_reading(self.pdf_compact_reading, cx);
        apply_translation_display_mode(cx);
        set_appearance(self.appearance, cx);
    }
}

impl Render for SystemSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        settings_frame(self, body, None, cx)
    }
}

/// 存储位置的只读路径行：标签在上、路径在下，长路径换行显示。
pub(crate) fn storage_path_row(
    label: &'static str,
    path: &Path,
    theme: Palette,
) -> gpui_kit::AnyElement {
    div()
        .v_flex()
        .gap_0p5()
        .child(div().text_xs().text_color(rgb(theme.muted)).child(label))
        .child(
            div()
                .text_sm()
                .line_height(gpui_kit::relative(1.4))
                .text_color(rgb(theme.ink))
                .child(path.display().to_string()),
        )
        .into_any_element()
}
