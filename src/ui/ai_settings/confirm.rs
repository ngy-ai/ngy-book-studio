//! The save confirmation a window asks for.

use super::*;

/// 一次提交前需要用户确认的对话框。`cancelled` 是用户选择取消时的提示文案。
pub(crate) struct SaveConfirmation {
    pub(crate) answer: Pin<Box<dyn Future<Output = bool> + 'static>>,
    pub(crate) cancelled: &'static str,
}

impl SaveConfirmation {
    pub(crate) fn new(
        answer: impl Future<Output = bool> + 'static,
        cancelled: &'static str,
    ) -> Self {
        Self {
            answer: Box::pin(answer),
            cancelled,
        }
    }
}
