//! AI 设置的五个独立窗口。
//!
//! 设置按 Endpoint、模型设置、联网搜索、后台任务、系统配置分成五块，每一块一个原生
//! 窗口，各自持有自己的草稿、提示与保存流程，五个窗口可以同时打开。窗口之间**不共享
//! 草稿**：保存时以当前已保存的 `ProviderSettings` 为底，只覆盖本窗口负责的字段，
//! 因此任何一个窗口保存都不会把别的窗口里还没提交的输入写进数据库。
//!
//! 这一层只放五个窗口共用的东西：窗口身份、应用级单例登记、状态与提示、统一的外框
//! 渲染，以及统一的提交流程（`AppServices::configure_providers` 的两把门闩与等待上限）。
//! 每个窗口自己的字段、校验和面板渲染放在各自的子模块里。

/// `ui` 层的元素、取色入口与公共类型（与改动前的单文件设置窗口用的是同一批名字）。
///
/// 这里是 glob 再导出：子模块只要 `use super::*;` 就同时拿到本模块的定义和 `ui` 的这批
/// 名字，不必各自再写一遍 `use super::super::*;`。
pub(super) use super::*;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ngy_book_studio::services::{ApiKeyUpdate, AppServices, ModelRole, ProviderSettings};

mod background_jobs;
mod endpoint;
mod endpoint_draft;
mod models;
mod system;
#[cfg(test)]
mod tests_support;
mod web_search;

use background_jobs::BackgroundJobsSettingsWindow;
use endpoint::EndpointSettingsWindow;
use endpoint_draft::{
    EndpointDraft, detect_models, new_endpoint_draft, render_model_detection,
    set_delete_endpoint_key,
};
use models::ModelsSettingsWindow;
use system::SystemSettingsWindow;
use web_search::WebSearchSettingsWindow;

/// 五个设置窗口的身份。菜单项、窗口标题、应用级单例登记和调试选择器都以它为准。
mod commit;

mod confirm;

mod frame;

mod launch;

mod pending;

mod section;

mod state;

mod tracker;

mod view;

mod widgets;

pub(crate) use commit::*;

pub(crate) use confirm::*;

pub(crate) use frame::*;

pub(crate) use launch::*;

pub(crate) use pending::*;

pub(crate) use section::*;

pub(crate) use state::*;

pub(crate) use tracker::*;

pub(crate) use view::*;

pub(crate) use widgets::*;

#[cfg(test)]
mod tests;
