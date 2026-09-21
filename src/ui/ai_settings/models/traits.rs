//! The window traits for the models settings window.

use super::*;

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
                self.state.report(format!("模型设置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        // When the embedding model or dimensions change, all existing vector
        // indices become incompatible. Warn the user before proceeding.
        // 翻译模型/翻译 Endpoint 变化不需要确认：`reconfigure_translation_jobs` 会给已翻好
        // 的行重盖新身份、只把没翻的块交给新模型，没有要丢弃的数据。
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
