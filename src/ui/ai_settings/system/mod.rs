//! 系统配置窗口：界面外观、阅读偏好、翻译默认与存储位置。
//!
//! 这里的偏好都**不是** Provider 配置，但和它们存在同一份 `ProviderSettings` 快照里，
//! 所以保存时同样以已保存设置为底、只覆盖本窗口的字段。界面外观、PDF 紧凑阅读与译文
//! 显示方式在提交成功后立刻应用到已经打开的窗口。

use super::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_component::checkbox::Checkbox;
use ngy_book_studio::services::{
    ApiKeyUpdate, ProviderSettings, TRANSLATION_LANGUAGES, TranslationDisplayMode,
    translation_language_label,
};
use ngy_book_studio::{logging, startup};

/// 一次待提交的数据目录更改：新位置、引导配置文件、当前（旧）数据目录。
#[derive(Clone)]
pub(crate) struct DataDirChange {
    pub(crate) target: PathBuf,
    pub(crate) config_path: PathBuf,
    pub(crate) from: PathBuf,
}

pub(crate) struct SystemSettingsWindow {
    pub(crate) services: Arc<AppServices>,
    /// Global PDF reading preference: no gap between pages when enabled.
    pub(crate) pdf_compact_reading: bool,
    /// 界面皮肤。保存后才生效，与上面的阅读偏好同一套语义。
    pub(crate) appearance: Appearance,
    /// Target language for reading-time book translation. `None` disables it.
    pub(crate) default_language: Option<String>,
    /// How reading-time translations are displayed relative to the original text.
    pub(crate) translation_display_mode: TranslationDisplayMode,
    /// Whether AI and web-search requests go through the system proxy.
    pub(crate) use_proxy: bool,
    pub(crate) state: SettingsWindowState,
    /// 目录选择对话框正开着（或更改正在提交）。原生对话框在自己的消息循环里跑，
    /// 用它挡住重复点击。
    pub(crate) choosing_data_directory: bool,
    /// 选中的新目录里已经有一个图书库：等用户确认是否覆盖。
    pub(crate) confirm_overwrite: Option<DataDirChange>,
}

mod app;

mod confirm;

mod panel;

mod view;

pub(crate) use view::*;

#[cfg(test)]
mod tests;
