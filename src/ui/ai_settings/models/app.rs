//! The models settings window.

use super::*;

/// 一个模型角色在界面上的三件套：Tab/字段标签、说明、选择器短名。选择器短名同时
/// 用于 Tab 与角色面板（`ai-models-tab-chat`、`ai-role-chat`、`ai-detect-chat-models`）。
fn model_role_text(role: ModelRole) -> (&'static str, &'static str, &'static str) {
    match role {
        ModelRole::Chat => ("问答模型", "用于侧栏问答、引用与只读工具调用。", "chat"),
        ModelRole::Translation => (
            "翻译模型",
            "只用于整本图书的后台翻译；换问答模型不会让已翻完的图书重翻。",
            "translation",
        ),
        ModelRole::Embedding => (
            "Embedding 模型",
            "用于向量索引；不可用时全文检索仍可工作。",
            "embedding",
        ),
        ModelRole::Vision => ("视觉模型", "用于页面 OCR 与视觉说明任务。", "vision"),
    }
}

/// 一组生成参数输入框的构造、读取与写回。问答与翻译共用这一份实现，差别只在默认值
/// （与最大输出的占位值）上。
impl GenerationInputs {
    pub(crate) fn new(
        values: GenerationValues,
        default_output_tokens: u32,
        window: &mut Window,
        cx: &mut Context<ModelsSettingsWindow>,
    ) -> Self {
        Self {
            temperature: optional_parameter_input(values.temperature, window, cx),
            top_p: optional_parameter_input(values.top_p, window, cx),
            max_output_tokens: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(values.max_output_tokens.to_string())
                    .placeholder(default_output_tokens.to_string())
            }),
            presence_penalty: optional_parameter_input(values.presence_penalty, window, cx),
            frequency_penalty: optional_parameter_input(values.frequency_penalty, window, cx),
        }
    }

    /// 五个输入框的当前文本，顺序与 `GenerationValues::from_inputs` 一致。
    pub(crate) fn values(&self, cx: &App) -> [SharedString; 5] {
        [
            self.temperature.read(cx).value(),
            self.top_p.read(cx).value(),
            self.max_output_tokens.read(cx).value(),
            self.presence_penalty.read(cx).value(),
            self.frequency_penalty.read(cx).value(),
        ]
    }

    /// 把一组取值写回输入框（「恢复默认」用）。
    pub(crate) fn set_values(
        &self,
        values: &GenerationValues,
        window: &mut Window,
        cx: &mut Context<ModelsSettingsWindow>,
    ) {
        for (input, text) in [
            (
                &self.temperature,
                optional_parameter_text(values.temperature),
            ),
            (&self.top_p, optional_parameter_text(values.top_p)),
            (
                &self.max_output_tokens,
                values.max_output_tokens.to_string(),
            ),
            (
                &self.presence_penalty,
                optional_parameter_text(values.presence_penalty),
            ),
            (
                &self.frequency_penalty,
                optional_parameter_text(values.frequency_penalty),
            ),
        ] {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }
}

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
        let translation_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.translation_model.clone())
                .placeholder(DEFAULT_TRANSLATION_MODEL)
        });
        let chat_generation = GenerationInputs::new(
            GenerationValues::from(&settings.chat_generation),
            DEFAULT_CHAT_OUTPUT_TOKENS,
            window,
            cx,
        );
        let translation_generation = GenerationInputs::new(
            GenerationValues::from(&settings.translation_generation),
            DEFAULT_TRANSLATION_OUTPUT_TOKENS,
            window,
            cx,
        );
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
        let mut this = Self {
            services,
            endpoints,
            chat_endpoint_id: settings.endpoint_routing.chat_endpoint_id,
            translation_endpoint_id: settings.endpoint_routing.translation_endpoint_id,
            embedding_endpoint_id: settings.endpoint_routing.embedding_endpoint_id,
            vision_endpoint_id: settings.endpoint_routing.vision_endpoint_id,
            chat_model_input,
            translation_model_input,
            chat_generation,
            translation_generation,
            embedding_model_input,
            embedding_dimensions_input,
            vision_model_input,
            active_model_tab: MODEL_TABS[0],
            state: SettingsWindowState::new(window),
            activation_subscription: None,
        };
        // Endpoint 列表由「Endpoint」窗口维护。本窗口只在打开时读一次那份设置，所以窗口
        // 没关的时候那边保存的新增、改名或删除都要靠这里跟上，否则新 Endpoint 在模型设置
        // 里根本选不到（见 `refresh_endpoints`）。
        this.activation_subscription =
            Some(cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_endpoints(window, cx);
                }
            }));
        this
    }

    /// 按当前已保存的 Endpoint 列表刷新 Endpoint 选择项。
    ///
    /// 只刷新这份列表与它带来的连带结果：各模型名、生成参数与角色绑定都是本窗口的草稿，
    /// 不在刷新范围内。所以「Endpoint」窗口里未保存的输入不会漏进来，本窗口没提交的输入
    /// 也不会被覆盖。
    pub(crate) fn refresh_endpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let Ok(settings) = self.services.provider_settings() else {
            return;
        };
        if !refresh_endpoint_drafts(self, settings.endpoints(), window, cx) {
            return;
        }
        // 被删掉的 Endpoint 可能还是某个角色在本窗口里的选择（那边的删除只按已保存绑定
        // 拦截）。留着它会变成一个界面上选不中、直到保存才报错的绑定，所以退回默认 Endpoint。
        let stale: Vec<ModelRole> = MODEL_TABS
            .into_iter()
            .filter(|role| {
                let id = self.endpoint_id_for(*role);
                !self.endpoints.iter().any(|endpoint| endpoint.id == id)
            })
            .collect();
        if !stale.is_empty() {
            for role in stale {
                self.assign_endpoint(role, DEFAULT_ENDPOINT_ID, cx);
            }
            self.state.report(
                "有模型之前选择的 Endpoint 已不存在，已改回「默认 Endpoint」；点击“保存设置”后生效。",
                false,
            );
        }
        cx.notify();
    }

    /// 以已保存设置为底，只覆盖本窗口负责的字段。Endpoint 列表由 Endpoint 窗口维护。
    pub(crate) fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        settings.chat_model = self.chat_model_input.read(cx).value().trim().to_string();
        settings.translation_model = self
            .translation_model_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        settings.chat_generation =
            GenerationValues::from_inputs(&self.chat_generation, cx)?.chat()?;
        settings.translation_generation =
            GenerationValues::from_inputs(&self.translation_generation, cx)?.translation()?;
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
        settings.endpoint_routing.translation_endpoint_id = self.translation_endpoint_id.clone();
        settings.endpoint_routing.embedding_endpoint_id = self.embedding_endpoint_id.clone();
        settings.endpoint_routing.vision_endpoint_id = self.vision_endpoint_id.clone();
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn endpoint_id_for(&self, role: ModelRole) -> &str {
        match role {
            ModelRole::Chat => &self.chat_endpoint_id,
            ModelRole::Translation => &self.translation_endpoint_id,
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
            ModelRole::Translation => self.translation_endpoint_id = id.to_string(),
            ModelRole::Embedding => self.embedding_endpoint_id = id.to_string(),
            ModelRole::Vision => self.vision_endpoint_id = id.to_string(),
        }
        self.state.notice = None;
        cx.notify();
    }

    pub(crate) fn model_input_for(&self, role: ModelRole) -> &Entity<InputState> {
        match role {
            ModelRole::Chat => &self.chat_model_input,
            ModelRole::Translation => &self.translation_model_input,
            ModelRole::Embedding => &self.embedding_model_input,
            ModelRole::Vision => &self.vision_model_input,
        }
    }

    /// 切换当前显示的模型 Tab。四个 Tab 共用同一个实体，因此未提交的草稿不会丢。
    pub(crate) fn select_model_tab(&mut self, role: ModelRole, cx: &mut Context<Self>) {
        if self.active_model_tab == role {
            return;
        }
        self.active_model_tab = role;
        cx.notify();
    }

    /// 恢复某一组生成参数的默认值。两组的默认值不同：问答 0.1，翻译 0（确定性）。
    pub(crate) fn reset_generation(
        &mut self,
        role: ModelRole,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.busy() {
            return;
        }
        let (defaults, label) = match role {
            ModelRole::Translation => (
                GenerationValues::from(&TranslationGenerationSettings::default()),
                "翻译生成参数",
            ),
            _ => (
                GenerationValues::from(&ChatGenerationSettings::default()),
                "问答生成参数",
            ),
        };
        let inputs = match role {
            ModelRole::Translation => &self.translation_generation,
            _ => &self.chat_generation,
        };
        inputs.set_values(&defaults, window, cx);
        self.state
            .report(format!("{label}已恢复默认，点击“保存设置”后生效。"), false);
        cx.notify();
    }

    #[inline(never)]
    pub(crate) fn render_panel(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        // 一个 Tab 只显示一个模型的配置。问答与翻译各带一组生成参数：两份设置分开
        // 存储、分别随各自的请求发送，所以各自只在对应的 Tab 里出现。向量与视觉没有
        // 采样参数可调。
        let mut children = vec![
            card_note(
                theme,
                "Endpoint 名称与地址在「Endpoint」窗口里编辑；那边保存后，回到本窗口时会自动\
                 刷新这份列表（窗口不必关掉重开）。",
            ),
            self.render_model_tabs(cx),
            self.render_role_panel(self.active_model_tab, cx),
        ];
        if matches!(
            self.active_model_tab,
            ModelRole::Chat | ModelRole::Translation
        ) {
            children.push(self.render_generation_panel(self.active_model_tab, cx));
        }
        panel(theme, children)
    }

    /// 模型 Tab 条。
    ///
    /// 用分段按钮而不是 `gpui_kit::component::tab::TabBar`：`TabBar` 的子项不暴露
    /// `debug_selector`，而本仓库的 GUI 冒烟与 GPUI 测试都按
    /// `ai-models-tab-<role>` 定位控件（`docs/agent/code-map-ui.md`）。
    #[inline(never)]
    pub(crate) fn render_model_tabs(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        let busy = self.state.busy();
        let mut tabs = div().h_flex().flex_wrap().gap_1();
        for role in MODEL_TABS {
            let (label, _, role_selector) = model_role_text(role);
            let selector = AiSettingsSection::Models.selector(&format!("tab-{role_selector}"));
            let element_id = selector.clone();
            let selected = self.active_model_tab == role;
            tabs = tabs.child(
                Button::new(SharedString::from(element_id))
                    .small()
                    .ghost()
                    .label(label)
                    .disabled(busy)
                    .when(selected, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(theme.accent_soft).into())
                                .foreground(rgb(theme.accent_strong).into()),
                        )
                    })
                    .debug_selector(move || selector.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_model_tab(role, cx);
                    })),
            );
        }
        tabs.into_any_element()
    }

    #[inline(never)]
    pub(crate) fn render_role_panel(
        &self,
        role: ModelRole,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        let (label, description, selector) = model_role_text(role);
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
                            window.blur(cx);
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

    /// 生成参数面板。问答与翻译各一份：字段、取值范围与说明几乎相同，只有默认值、
    /// 温度那一句解释与「恢复默认」的选择器不同，所以面板本身只有一个实现。
    #[inline(never)]
    pub(crate) fn render_generation_panel(
        &self,
        role: ModelRole,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        let busy = self.state.busy();
        let (title, note, temperature_help, reset_selector, inputs) = match role {
            ModelRole::Translation => (
                "翻译生成参数",
                "保存后用于新的翻译请求（整本翻译、单块重试与「重新请求」共用这一份）。\
                 默认温度 0 是刻意的：一个文本块只有一个可接受的答案，采样只会增加方差。\
                 改动它不影响已翻好的译文，也不会让翻译任务重跑。除最大输出外，其余参数\
                 可留空，使用服务端默认值。",
                "0–2；默认 0（确定性）。一个文本块只有一个可接受的答案，调高只会引入方差。",
                "ai-reset-translation-generation",
                &self.translation_generation,
            ),
            _ => (
                "问答生成参数",
                "保存后用于新的问答请求。除最大输出外，其余参数可留空，使用服务端默认值；\
                 具体支持以所选模型为准。",
                "0–2；默认 0.1。越低越集中，越高越随机。通常与 Top P 选择一项调整。",
                "ai-reset-chat-generation",
                &self.chat_generation,
            ),
        };
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
                    .child(card_title(theme, title))
                    .child(
                        Button::new(reset_selector)
                            .label("恢复默认")
                            .disabled(busy)
                            .debug_selector(move || reset_selector.to_string())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reset_generation(role, window, cx);
                            })),
                    ),
            )
            .child(card_note(theme, note))
            .child(render_input_field(
                theme,
                busy,
                "温度（Temperature）",
                temperature_help,
                &inputs.temperature,
            ))
            .child(render_input_field(
                theme,
                busy,
                "核采样（Top P）",
                "0–1；控制候选词的累计概率范围，越小越集中。",
                &inputs.top_p,
            ))
            .child(render_input_field(
                theme,
                busy,
                "最大输出 token 数",
                "正整数，默认 4096；请按所选模型设置，墨页不设固定上限。上下文不足时可能进一步降低。",
                &inputs.max_output_tokens,
            ))
            .child(render_input_field(
                theme,
                busy,
                "出现惩罚（Presence Penalty）",
                "−2–2；正值降低已出现词的再次使用倾向，负值提高。",
                &inputs.presence_penalty,
            ))
            .child(render_input_field(
                theme,
                busy,
                "频率惩罚（Frequency Penalty）",
                "−2–2；正值按出现次数抑制重复，负值鼓励重复。",
                &inputs.frequency_penalty,
            ))
            .child(card_note(
                theme,
                "最大输出 token 数不改变模型的上下文窗口；Ollama 上下文长度需在服务端设置。",
            ))
            .into_any_element()
    }
}
