//! Pending operations, their wait limit and the notice a window shows.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingOperation {
    Idle,
    Detecting,
    Saving,
}

impl PendingOperation {
    pub(crate) fn busy(self) -> bool {
        self != Self::Idle
    }
}

/// 界面等待一次设置保存的上限。
///
/// 保存自己没有超时：它要依次拿两把进程级门闩（`AppServices::ai_configuration` 与索引
/// 协调器的 `transitions`），被后台任务堵住时可以无限期等下去。界面如果陪着一起等，
/// 窗口就永远停在「正在安全保存设置…」——保存按钮被 `PendingOperation::Saving`
/// 禁用、关窗又被告知「保存未结束」，用户除了杀进程没有别的出路（现场 2026-09-17）。
///
/// 超时只放弃**等**，不放弃写入：任务已经在 `AppServices` 的 runtime 上跑，它会自己
/// 提交或回滚（见 `configure_providers` 的文档），所以提示必须写成「可能已提交」，
/// 不能写成失败。这个上限同时是退出软件时等待结果的上界。
pub(crate) const SAVE_WAIT_LIMIT: Duration = Duration::from_secs(60);

/// 一条显示在底部的提示（成功为绿色、失败为红色）。
#[derive(Clone, Debug)]
pub(crate) struct SettingsNotice {
    pub(crate) text: String,
    pub(crate) error: bool,
}
