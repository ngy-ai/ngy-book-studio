//! 模型设置窗口：问答、翻译、向量与视觉四个模型各自选择 Endpoint 与模型名，
//! 问答与翻译各自带一组生成参数，四个配置用 Tab 切换。
//!
//! 保存时以已保存的 `ProviderSettings` 为底，只覆盖四个模型、它们的 Endpoint 绑定与
//! 两组生成参数。向量模型或维度变化会让已有向量索引失效，所以这里保留一次显式确认；
//! 翻译模型变化不需要确认 —— 已翻好的行会被重盖身份，只有没翻的块交给新模型。翻译的
//! 生成参数连执行身份都不进，改动它不影响任何已存数据，因此更不需要确认。

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_component::Selectable as _;
use ngy_book_studio::ai::{
    ChatGenerationSettings, DEFAULT_CHAT_OUTPUT_TOKENS, DEFAULT_TRANSLATION_OUTPUT_TOKENS,
    MAX_EMBEDDING_DIMENSIONS, TranslationGenerationSettings,
};
use ngy_book_studio::services::{
    ApiKeyUpdate, DEFAULT_CHAT_MODEL, DEFAULT_EMBEDDING_DIMENSIONS, DEFAULT_EMBEDDING_MODEL,
    DEFAULT_TRANSLATION_MODEL, DEFAULT_VISION_MODEL, ModelRole, ProviderSettings,
};

/// 本窗口 Tab 的顺序，`ModelRole` 同时充当 Tab 的键。问答模型排在最前，其次是与它
/// 成对出现的翻译模型。
pub(crate) const MODEL_TABS: [ModelRole; 4] = [
    ModelRole::Chat,
    ModelRole::Translation,
    ModelRole::Embedding,
    ModelRole::Vision,
];

/// 一组生成参数输入框。问答与翻译各持一份：字段相同，默认值与说明不同（翻译默认温度
/// 0），所以在窗口里出现两次而不是共用一个。
pub(crate) struct GenerationInputs {
    pub(crate) temperature: Entity<InputState>,
    pub(crate) top_p: Entity<InputState>,
    pub(crate) max_output_tokens: Entity<InputState>,
    pub(crate) presence_penalty: Entity<InputState>,
    pub(crate) frequency_penalty: Entity<InputState>,
}

pub(crate) struct ModelsSettingsWindow {
    pub(crate) services: Arc<AppServices>,
    pub(crate) endpoints: Vec<EndpointDraft>,
    pub(crate) chat_endpoint_id: String,
    pub(crate) translation_endpoint_id: String,
    pub(crate) embedding_endpoint_id: String,
    pub(crate) vision_endpoint_id: String,
    pub(crate) chat_model_input: Entity<InputState>,
    pub(crate) translation_model_input: Entity<InputState>,
    pub(crate) chat_generation: GenerationInputs,
    pub(crate) translation_generation: GenerationInputs,
    pub(crate) embedding_model_input: Entity<InputState>,
    pub(crate) embedding_dimensions_input: Entity<InputState>,
    pub(crate) vision_model_input: Entity<InputState>,
    /// 当前显示的模型 Tab。四个 Tab 的输入都活在同一个实体里，切 Tab 只换显示。
    pub(crate) active_model_tab: ModelRole,
    pub(crate) state: SettingsWindowState,
}

mod app;

mod parse;

mod traits;

pub(crate) use parse::*;

#[cfg(test)]
mod tests;
