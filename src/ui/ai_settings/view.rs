//! The traits a settings window implements.

use super::*;

/// 一个设置窗口的最小契约：身份、状态、提交，以及提交成功后要立刻应用的本地偏好。
///
/// 只在 `ai_settings` 内部使用：窗口类型对外只通过各自的 `open_*` 函数暴露。
pub(crate) trait SettingsWindowView: Render + Sized + 'static {
    fn section() -> AiSettingsSection;

    /// 本窗口在 [`SettingsWindowTrackers`] 里的登记。
    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self>;

    fn state(&self) -> &SettingsWindowState;

    fn state_mut(&mut self) -> &mut SettingsWindowState;

    fn services(&self) -> &Arc<AppServices>;

    /// 校验草稿并提交。只覆盖本窗口负责的字段。
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>);

    /// 提交成功后立刻应用的本地偏好（皮肤、PDF 阅读、翻译显示方式）。
    fn apply_saved_preferences(&mut self, _cx: &mut App) {}
}

/// 需要在窗口里列出各 Endpoint 的设置窗口（Endpoint 与对话模型）。
pub(crate) trait EndpointBackedWindow: SettingsWindowView {
    fn endpoints(&self) -> &[EndpointDraft];

    fn endpoints_mut(&mut self) -> &mut Vec<EndpointDraft>;

    /// 某个角色当前填写的模型名。Endpoint 窗口没有模型输入，返回 `None`。
    fn role_model(&self, _role: ModelRole, _cx: &App) -> Option<String> {
        None
    }

    /// 采纳检测结果里的模型名。Endpoint 窗口没有模型输入，默认忽略。
    fn adopt_detected_model(
        &mut self,
        _role: ModelRole,
        _model: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}
