//! Endpoint 设置窗口：连接、密钥与远程授权。
//!
//! 这里只管 Endpoint 本身；模型绑定与模型名在模型设置窗口里选。保存时以已保存的
//! `ProviderSettings` 为底，只覆盖 Endpoint 列表与各 Endpoint 的密钥。
//!
//! - [`editing`] holds the draft and selection state changes;
//! - [`render`] holds the panel and its cards;
//! - [`window`] holds the window plumbing traits;
//! - [`tests`] holds the GUI regressions.

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_kit::component::{Selectable as _, checkbox::Checkbox};
use ngy_book_studio::ai::{DEFAULT_AI_REQUEST_TIMEOUT_SECS, normalize_provider_base_url};
use ngy_book_studio::services::{
    ApiKeyUpdate, DEFAULT_ENDPOINT_ID, EndpointSettings, ProviderSettings,
};

mod editing;

mod render;

mod window;

#[cfg(test)]
mod tests;

pub(crate) use editing::*;

pub(crate) use render::*;

pub(crate) use window::*;

pub(crate) struct EndpointSettingsWindow {
    pub(crate) services: Arc<AppServices>,
    pub(crate) endpoints: Vec<EndpointDraft>,
    pub(crate) selected_endpoint_id: String,
    /// 已保存设置里各模型绑定的 Endpoint。删除时用它挡住仍被模型使用的连接；
    /// 每次删除前重新读一遍已保存设置，避免用旧快照放走正在被使用的 Endpoint。
    pub(crate) chat_endpoint_id: String,
    pub(crate) translation_endpoint_id: String,
    pub(crate) embedding_endpoint_id: String,
    pub(crate) vision_endpoint_id: String,
    pub(crate) state: SettingsWindowState,
}
