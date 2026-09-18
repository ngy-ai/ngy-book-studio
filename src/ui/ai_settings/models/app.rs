//! The models settings window.

use super::*;

impl ModelsSettingsWindow {
    pub(crate) fn new(
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
    pub(crate) fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
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

    pub(crate) fn endpoint_id_for(&self, role: ModelRole) -> &str {
        match role {
            ModelRole::Chat => &self.chat_endpoint_id,
            ModelRole::Embedding => &self.embedding_endpoint_id,
            ModelRole::Vision => &self.vision_endpoint_id,
        }
    }

    pub(crate) fn assign_endpoint(&mut self, role: ModelRole, id: &str, cx: &mut Context<Self>) {
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

    pub(crate) fn model_input_for(&self, role: ModelRole) -> &Entity<InputState> {
        match role {
            ModelRole::Chat => &self.chat_model_input,
            ModelRole::Embedding => &self.embedding_model_input,
            ModelRole::Vision => &self.vision_model_input,
        }
    }

    pub(crate) fn reset_chat_generation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(crate) fn render_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
    pub(crate) fn render_role_panel(
        &self,
        role: ModelRole,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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
    pub(crate) fn render_generation_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
