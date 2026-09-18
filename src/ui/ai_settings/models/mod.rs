//! 对话模型设置窗口：三个模型各自选择 Endpoint、模型名与对话生成参数。
//!
//! 保存时以已保存的 `ProviderSettings` 为底，只覆盖三个模型、它们的 Endpoint 绑定与
//! 生成参数。向量模型或维度变化会让已有向量索引失效，所以这里保留一次显式确认。

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_component::Selectable as _;
use ngy_book_studio::ai::{
    ChatGenerationSettings, DEFAULT_CHAT_OUTPUT_TOKENS, MAX_EMBEDDING_DIMENSIONS,
};
use ngy_book_studio::services::{
    ApiKeyUpdate, DEFAULT_CHAT_MODEL, DEFAULT_EMBEDDING_DIMENSIONS, DEFAULT_EMBEDDING_MODEL,
    DEFAULT_VISION_MODEL, ModelRole, ProviderSettings,
};

pub(crate) struct ModelsSettingsWindow {
    pub(crate) services: Arc<AppServices>,
    pub(crate) endpoints: Vec<EndpointDraft>,
    pub(crate) chat_endpoint_id: String,
    pub(crate) embedding_endpoint_id: String,
    pub(crate) vision_endpoint_id: String,
    pub(crate) chat_model_input: Entity<InputState>,
    pub(crate) temperature_input: Entity<InputState>,
    pub(crate) top_p_input: Entity<InputState>,
    pub(crate) max_output_tokens_input: Entity<InputState>,
    pub(crate) presence_penalty_input: Entity<InputState>,
    pub(crate) frequency_penalty_input: Entity<InputState>,
    pub(crate) embedding_model_input: Entity<InputState>,
    pub(crate) embedding_dimensions_input: Entity<InputState>,
    pub(crate) vision_model_input: Entity<InputState>,
    pub(crate) state: SettingsWindowState,
}

mod app;

mod parse;

mod traits;

pub(crate) use parse::*;

#[cfg(test)]
mod tests;
