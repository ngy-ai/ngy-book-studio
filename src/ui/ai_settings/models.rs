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

pub(super) struct ModelsSettingsWindow {
    services: Arc<AppServices>,
    endpoints: Vec<EndpointDraft>,
    chat_endpoint_id: String,
    embedding_endpoint_id: String,
    vision_endpoint_id: String,
    chat_model_input: Entity<InputState>,
    temperature_input: Entity<InputState>,
    top_p_input: Entity<InputState>,
    max_output_tokens_input: Entity<InputState>,
    presence_penalty_input: Entity<InputState>,
    frequency_penalty_input: Entity<InputState>,
    embedding_model_input: Entity<InputState>,
    embedding_dimensions_input: Entity<InputState>,
    vision_model_input: Entity<InputState>,
    state: SettingsWindowState,
}

impl ModelsSettingsWindow {
    pub(super) fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let endpoints = settings
            .endpoints()
            .into_iter()
            .map(|endpoint| new_endpoint_draft(endpoint, window, cx))
            .collect();
        let chat_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.chat_model.clone())
                .placeholder(DEFAULT_CHAT_MODEL)
        });
        let generation = &settings.chat_generation;
        let temperature_input = optional_parameter_input(generation.temperature, window, cx);
        let top_p_input = optional_parameter_input(generation.top_p, window, cx);
        let max_output_tokens_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(generation.max_output_tokens.to_string())
                .placeholder(DEFAULT_CHAT_OUTPUT_TOKENS.to_string())
        });
        let presence_penalty_input =
            optional_parameter_input(generation.presence_penalty, window, cx);
        let frequency_penalty_input =
            optional_parameter_input(generation.frequency_penalty, window, cx);
        let embedding_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.embedding_model.clone())
                .placeholder(DEFAULT_EMBEDDING_MODEL)
        });
        let embedding_dimensions_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.embedding_dimensions.to_string())
                .placeholder(DEFAULT_EMBEDDING_DIMENSIONS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let vision_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.vision_model.clone())
                .placeholder(DEFAULT_VISION_MODEL)
        });
        Self {
            services,
            endpoints,
            chat_endpoint_id: settings.endpoint_routing.chat_endpoint_id,
            embedding_endpoint_id: settings.endpoint_routing.embedding_endpoint_id,
            vision_endpoint_id: settings.endpoint_routing.vision_endpoint_id,
            chat_model_input,
            temperature_input,
            top_p_input,
            max_output_tokens_input,
            presence_penalty_input,
            frequency_penalty_input,
            embedding_model_input,
            embedding_dimensions_input,
            vision_model_input,
            state: SettingsWindowState::new(window),
        }
    }

    /// 以已保存设置为底，只覆盖本窗口负责的字段。Endpoint 列表由 Endpoint 窗口维护。
    fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        settings.chat_model = self.chat_model_input.read(cx).value().trim().to_string();
        settings.chat_generation = parse_chat_generation(
            self.temperature_input.read(cx).value().as_ref(),
            self.top_p_input.read(cx).value().as_ref(),
            self.max_output_tokens_input.read(cx).value().as_ref(),
            self.presence_penalty_input.read(cx).value().as_ref(),
            self.frequency_penalty_input.read(cx).value().as_ref(),
        )?;
        settings.embedding_model = self
            .embedding_model_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        settings.embedding_dimensions =
            parse_embedding_dimensions(self.embedding_dimensions_input.read(cx).value().as_ref())?;
        settings.vision_model = self.vision_model_input.read(cx).value().trim().to_string();
        settings.endpoint_routing.chat_endpoint_id = self.chat_endpoint_id.clone();
        settings.endpoint_routing.embedding_endpoint_id = self.embedding_endpoint_id.clone();
        settings.endpoint_routing.vision_endpoint_id = self.vision_endpoint_id.clone();
        settings.validate()?;
        Ok(settings)
    }

    fn endpoint_id_for(&self, role: ModelRole) -> &str {
        match role {
            ModelRole::Chat => &self.chat_endpoint_id,
            ModelRole::Embedding => &self.embedding_endpoint_id,
            ModelRole::Vision => &self.vision_endpoint_id,
        }
    }

    fn assign_endpoint(&mut self, role: ModelRole, id: &str, cx: &mut Context<Self>) {
        if self.state.busy() || !self.endpoints.iter().any(|endpoint| endpoint.id == id) {
            return;
        }
        match role {
            ModelRole::Chat => self.chat_endpoint_id = id.to_string(),
            ModelRole::Embedding => self.embedding_endpoint_id = id.to_string(),
            ModelRole::Vision => self.vision_endpoint_id = id.to_string(),
        }
        self.state.notice = None;
        cx.notify();
    }

    fn model_input_for(&self, role: ModelRole) -> &Entity<InputState> {
        match role {
            ModelRole::Chat => &self.chat_model_input,
            ModelRole::Embedding => &self.embedding_model_input,
            ModelRole::Vision => &self.vision_model_input,
        }
    }

    fn reset_chat_generation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let defaults = ChatGenerationSettings::default();
        for (input, value) in [
            (
                &self.temperature_input,
                optional_parameter_text(defaults.temperature),
            ),
            (&self.top_p_input, optional_parameter_text(defaults.top_p)),
            (
                &self.max_output_tokens_input,
                defaults.max_output_tokens.to_string(),
            ),
            (
                &self.presence_penalty_input,
                optional_parameter_text(defaults.presence_penalty),
            ),
            (
                &self.frequency_penalty_input,
                optional_parameter_text(defaults.frequency_penalty),
            ),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.state
            .report("对话生成参数已恢复默认，点击“保存设置”后生效。", false);
        cx.notify();
    }

    #[inline(never)]
    fn render_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        panel(
            theme,
            vec![
                card_note(
                    theme,
                    "Endpoint 名称与地址在「Endpoint」窗口里编辑；这里的选择在打开本窗口时读取，\
                     那边新增或改名后重新打开本窗口即可看到。",
                ),
                self.render_role_panel(ModelRole::Chat, cx),
                self.render_role_panel(ModelRole::Embedding, cx),
                self.render_role_panel(ModelRole::Vision, cx),
                self.render_generation_panel(cx),
            ],
        )
    }

    #[inline(never)]
    fn render_role_panel(&self, role: ModelRole, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let (label, description, selector) = match role {
            ModelRole::Chat => ("对话模型", "用于流式问答和只读工具调用。", "chat"),
            ModelRole::Embedding => (
                "Embedding 模型",
                "用于向量索引；不可用时全文检索仍可工作。",
                "embedding",
            ),
            ModelRole::Vision => ("视觉模型", "用于页面 OCR 与视觉说明任务。", "vision"),
        };
        let endpoint_id = self.endpoint_id_for(role).to_string();
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == endpoint_id);
        let busy = self.state.busy();
        let detection =
            endpoint.and_then(|endpoint| render_model_detection(self, endpoint, Some(role), cx));
        div()
            .id(SharedString::from(format!("ai-role-{selector}")))
            .v_flex()
            .gap_2()
            .child(card_title(theme, format!("{label} · Endpoint")))
            .child(div().h_flex().flex_wrap().gap_2().children(
                self.endpoints.iter().enumerate().map(|(index, endpoint)| {
                    let id = endpoint.id.clone();
                    Button::new(("ai-role-endpoint", index))
                        .label(endpoint.name_input.read(cx).value())
                        .selected(endpoint_id == endpoint.id)
                        .disabled(busy)
                        .debug_selector(move || format!("ai-{selector}-endpoint-{index}"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            window.blur();
                            this.assign_endpoint(role, &id, cx);
                        }))
                }),
            ))
            .when_some(endpoint, |this, endpoint| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.muted))
                        .child(endpoint.base_url_input.read(cx).value()),
                )
            })
            .child(render_input_field(
                theme,
                busy,
                label,
                description,
                self.model_input_for(role),
            ))
            .when(role == ModelRole::Embedding, |this| {
                this.child(render_input_field(
                    theme,
                    busy,
                    "向量维度（Dimensions）",
                    "正整数，默认 1024。修改后所有已索引的向量将失效并需要重新生成。",
                    &self.embedding_dimensions_input,
                ))
            })
            .child(
                Button::new("ai-role-detect-models")
                    .label("检测此 Endpoint 的模型")
                    .disabled(busy)
                    .debug_selector(move || format!("ai-detect-{selector}-models"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        detect_models(this, &endpoint_id, window, cx);
                    })),
            )
            .when_some(detection, |this, detection| this.child(detection))
            .into_any_element()
    }

    #[inline(never)]
    fn render_generation_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let busy = self.state.busy();
        div()
            .v_flex()
            .gap_4()
            .p_4()
            .rounded(px(10.))
            .bg(rgb(theme.paper))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .gap_3()
                    .child(card_title(theme, "对话生成参数"))
                    .child(
                        Button::new("ai-reset-chat-generation")
                            .label("恢复默认")
                            .disabled(busy)
                            .debug_selector(|| "ai-reset-chat-generation".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_chat_generation(window, cx);
                            })),
                    ),
            )
            .child(card_note(
                theme,
                "保存后用于新的对话请求。除最大输出外，其余参数可留空，使用服务端默认值；具体支持以所选模型为准。",
            ))
            .child(render_input_field(
                theme,
                busy,
                "温度（Temperature）",
                "0–2；默认 0.1。越低越集中，越高越随机。通常与 Top P 选择一项调整。",
                &self.temperature_input,
            ))
            .child(render_input_field(
                theme,
                busy,
                "核采样（Top P）",
                "0–1；控制候选词的累计概率范围，越小越集中。",
                &self.top_p_input,
            ))
            .child(render_input_field(
                theme,
                busy,
                "最大输出 token 数",
                "正整数，默认 4096；请按所选模型设置，墨页不设固定上限。上下文不足时可能进一步降低。",
                &self.max_output_tokens_input,
            ))
            .child(render_input_field(
                theme,
                busy,
                "出现惩罚（Presence Penalty）",
                "−2–2；正值降低已出现词的再次使用倾向，负值提高。",
                &self.presence_penalty_input,
            ))
            .child(render_input_field(
                theme,
                busy,
                "频率惩罚（Frequency Penalty）",
                "−2–2；正值按出现次数抑制重复，负值鼓励重复。",
                &self.frequency_penalty_input,
            ))
            .child(card_note(
                theme,
                "最大输出 token 数不改变模型的上下文窗口；Ollama 上下文长度需在服务端设置。",
            ))
            .into_any_element()
    }
}

impl SettingsWindowView for ModelsSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::Models
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().models
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
        let settings = match self.entered_settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.state
                    .report(format!("对话模型设置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        // When the embedding model or dimensions change, all existing vector
        // indices become incompatible. Warn the user before proceeding.
        let confirmation = match self.services.provider_settings() {
            Ok(current)
                if current.embedding_model != settings.embedding_model
                    || current.embedding_dimensions != settings.embedding_dimensions =>
            {
                let title = if current.embedding_model != settings.embedding_model
                    && current.embedding_dimensions != settings.embedding_dimensions
                {
                    "Embedding 模型与向量维度已修改"
                } else if current.embedding_model != settings.embedding_model {
                    "Embedding 模型已修改"
                } else {
                    "向量维度已修改"
                };
                let answer = window.prompt(
                    gpui::PromptLevel::Warning,
                    title,
                    Some(
                        "修改 Embedding 模型或向量维度会使所有已索引的向量失效，保存后将自动重新生成索引。是否继续？",
                    ),
                    &[
                        gpui::PromptButton::ok("继续保存"),
                        gpui::PromptButton::cancel("取消"),
                    ],
                    cx,
                );
                Some(SaveConfirmation::new(
                    async move { matches!(answer.await, Ok(0)) },
                    "已取消保存，Embedding 设置未修改。",
                ))
            }
            _ => None,
        };
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            BTreeMap::new(),
            ApiKeyUpdate::Keep,
            confirmation,
            window,
            cx,
        );
    }
}

impl EndpointBackedWindow for ModelsSettingsWindow {
    fn endpoints(&self) -> &[EndpointDraft] {
        &self.endpoints
    }

    fn endpoints_mut(&mut self) -> &mut Vec<EndpointDraft> {
        &mut self.endpoints
    }

    fn role_model(&self, role: ModelRole, cx: &App) -> Option<String> {
        Some(
            self.model_input_for(role)
                .read(cx)
                .value()
                .trim()
                .to_string(),
        )
    }

    fn adopt_detected_model(
        &mut self,
        role: ModelRole,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.model_input_for(role).clone();
        input.update(cx, |input, cx| {
            input.set_value(model.to_string(), window, cx)
        });
        cx.notify();
    }
}

impl Render for ModelsSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        settings_frame(self, body, None, cx)
    }
}

fn optional_parameter_text(value: Option<f32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_parameter_input<V>(
    value: Option<f32>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(optional_parameter_text(value))
            .placeholder("留空使用服务端默认值")
    })
}

fn parse_chat_generation(
    temperature: &str,
    top_p: &str,
    max_output_tokens: &str,
    presence_penalty: &str,
    frequency_penalty: &str,
) -> Result<ChatGenerationSettings> {
    fn optional_number(label: &str, value: &str) -> Result<Option<f32>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        value
            .parse::<f32>()
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{label}必须是数字，或留空使用服务端默认值"))
    }
    let settings = ChatGenerationSettings {
        temperature: optional_number("温度（Temperature）", temperature)?,
        top_p: optional_number("核采样（Top P）", top_p)?,
        max_output_tokens: max_output_tokens
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("最大输出 token 数必须是有效的正整数"))?,
        presence_penalty: optional_number("出现惩罚（Presence Penalty）", presence_penalty)?,
        frequency_penalty: optional_number("频率惩罚（Frequency Penalty）", frequency_penalty)?,
    };
    settings.validate()?;
    Ok(settings)
}

fn parse_embedding_dimensions(value: &str) -> Result<usize> {
    let value = value.trim();
    let dimensions = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("embedding 维度必须是 1 到 {MAX_EMBEDDING_DIMENSIONS} 之间的正整数")
    })?;
    anyhow::ensure!(
        (1..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions),
        "embedding 维度必须是 1 到 {MAX_EMBEDDING_DIMENSIONS} 之间的正整数"
    );
    Ok(dimensions)
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use ngy_book_studio::services::DEFAULT_ENDPOINT_ID;

    fn open_window(
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        Entity<ModelsSettingsWindow>,
        &mut VisualTestContext,
    ) {
        open_settings_view::<ModelsSettingsWindow>(cx, ModelsSettingsWindow::new)
    }

    #[gpui::test]
    fn role_bindings_change_without_saving_and_block_missing_endpoints(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        visual.simulate_resize(size(px(1080.), px(1600.)));
        redraw(visual);
        let choice = visual.debug_bounds("ai-embedding-endpoint-0").unwrap();
        visual.simulate_click(choice.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            let entered = view.entered_settings(cx).unwrap();
            assert_eq!(
                entered.endpoint_routing.embedding_endpoint_id,
                DEFAULT_ENDPOINT_ID
            );
            assert_eq!(
                entered.endpoint_routing.chat_endpoint_id,
                DEFAULT_ENDPOINT_ID
            );
            assert_eq!(
                view.services
                    .provider_settings()
                    .unwrap()
                    .endpoint_routing
                    .embedding_endpoint_id,
                DEFAULT_ENDPOINT_ID,
            );
        });
        // 不存在的 Endpoint 不会被采纳。
        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                view.assign_endpoint(ModelRole::Vision, "missing-endpoint", cx);
                assert_eq!(view.vision_endpoint_id, DEFAULT_ENDPOINT_ID);
            });
        });
    }

    #[gpui::test]
    fn model_and_generation_drafts_survive_invalid_save_then_reset(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        visual.simulate_resize(size(px(1080.), px(2200.)));
        redraw(visual);
        let inputs = settings.read_with(visual, |view, _| {
            [
                view.temperature_input.clone(),
                view.top_p_input.clone(),
                view.max_output_tokens_input.clone(),
                view.presence_penalty_input.clone(),
                view.frequency_penalty_input.clone(),
            ]
        });
        for (input, value) in inputs.iter().zip(["0.7", "0.8", "131072", "0.3", "-0.4"]) {
            edit_input(visual, input, value);
        }
        settings.read_with(visual, |view, cx| {
            let parameters = view.entered_settings(cx).unwrap().chat_generation;
            assert_eq!(parameters.temperature, Some(0.7));
            assert_eq!(parameters.top_p, Some(0.8));
            assert_eq!(parameters.max_output_tokens, 131_072);
            assert_eq!(parameters.presence_penalty, Some(0.3));
            assert_eq!(parameters.frequency_penalty, Some(-0.4));
            assert_eq!(
                view.services.provider_settings().unwrap().chat_generation,
                ChatGenerationSettings::default()
            );
        });

        edit_input(visual, &inputs[2], "0");
        click(visual, "ai-models-save-settings");
        settings.read_with(visual, |view, cx| {
            assert_eq!(view.state.operation, PendingOperation::Idle);
            assert!(view.state.notice().is_some_and(|notice| notice.error));
            assert_eq!(view.max_output_tokens_input.read(cx).value().as_ref(), "0");
            assert_eq!(
                view.services.provider_settings().unwrap().chat_generation,
                ChatGenerationSettings::default()
            );
        });

        let model = settings.read_with(visual, |view, _| view.chat_model_input.clone());
        edit_input(visual, &model, "preserved-model-draft");
        let reset = visual.debug_bounds("ai-reset-chat-generation").unwrap();
        visual.simulate_click(reset.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            let entered = view.entered_settings(cx).unwrap();
            assert_eq!(entered.chat_generation, ChatGenerationSettings::default());
            assert_eq!(entered.chat_model, "preserved-model-draft");
            assert_eq!(
                view.services.provider_settings().unwrap().chat_model,
                DEFAULT_CHAT_MODEL
            );
            assert!(!view.state.notice().unwrap().error);
        });
    }

    #[gpui::test]
    fn detected_models_fill_the_role_input(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        visual.simulate_resize(size(px(1080.), px(2200.)));
        redraw(visual);
        settings.update(visual, |view, cx| {
            view.endpoints[0].detected_models = Some(vec!["qwen3.5:0.8b".into()]);
            cx.notify();
        });
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(
                view.role_model(ModelRole::Chat, cx).as_deref(),
                Some(DEFAULT_CHAT_MODEL)
            );
        });
        visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.adopt_detected_model(ModelRole::Chat, "qwen3.5:0.8b", window, cx);
            });
        });
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(
                view.role_model(ModelRole::Chat, cx).as_deref(),
                Some("qwen3.5:0.8b")
            );
            assert_eq!(
                view.chat_model_input.read(cx).value().as_ref(),
                "qwen3.5:0.8b"
            );
        });
    }

    #[test]
    fn generation_text_parsing_supports_blank_options_and_rejects_invalid_drafts() {
        let empty_options = parse_chat_generation(" ", "", " 1 ", "", " ").unwrap();
        assert_eq!(empty_options.temperature, None);
        assert_eq!(empty_options.top_p, None);
        assert_eq!(empty_options.presence_penalty, None);
        assert_eq!(empty_options.frequency_penalty, None);
        assert_eq!(empty_options.max_output_tokens, 1);
        for value in ["4097", "65536", "131072", "4294967295"] {
            assert_eq!(
                parse_chat_generation("", "", value, "", "")
                    .unwrap()
                    .max_output_tokens,
                value.parse::<u32>().unwrap(),
            );
        }
        for invalid in ["", "0", "1.5", "-1", "NaN", "4294967296"] {
            assert!(parse_chat_generation("", "", invalid, "", "").is_err());
        }
        for invalid in ["NaN", "inf", "-inf", "0,7", "abc", "2.1", "-0.1"] {
            assert!(parse_chat_generation(invalid, "", "4096", "", "").is_err());
        }
        assert!(parse_chat_generation("", "1.1", "4096", "", "").is_err());
        assert!(parse_chat_generation("", "", "4096", "-2.1", "").is_err());
        assert!(parse_chat_generation("", "", "4096", "", "2.1").is_err());
    }
}
