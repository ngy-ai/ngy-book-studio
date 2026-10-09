//! The per-window settings state.

use super::*;

/// 一个设置窗口的运行状态。五个窗口各有一份。
pub(crate) struct SettingsWindowState {
    pub(crate) operation: PendingOperation,
    pub(crate) notice: Option<SettingsNotice>,
    /// 关窗时用它在当前帧之后移除本窗口。
    pub(crate) window_handle: gpui_kit::AnyWindowHandle,
    /// 正文的滚动位置；每个窗口独立，互不影响。
    pub(crate) scroll_handle: gpui_kit::ScrollHandle,
}

impl SettingsWindowState {
    pub(crate) fn new(window: &Window) -> Self {
        Self {
            operation: PendingOperation::Idle,
            notice: None,
            window_handle: gpui_kit::Window::window_handle(window),
            scroll_handle: gpui_kit::ScrollHandle::new(),
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.operation.busy()
    }

    pub(crate) fn notice(&self) -> Option<&SettingsNotice> {
        self.notice.as_ref()
    }

    pub(crate) fn report(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(SettingsNotice {
            text: text.into(),
            error,
        });
    }

    /// 开始一次提交：按钮进入「正在保存…」并给出提示。
    pub(crate) fn begin_save(&mut self) {
        self.operation = PendingOperation::Saving;
        self.report("正在安全保存设置…", false);
    }

    pub(crate) fn begin_detection(&mut self, text: impl Into<String>) {
        self.operation = PendingOperation::Detecting;
        self.report(text, false);
    }
}
