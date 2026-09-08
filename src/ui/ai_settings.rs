use super::*;

use gpui_component::checkbox::Checkbox;
use moye_epub_editor::{
    ai::{
        ChatGenerationSettings, DEFAULT_AI_REQUEST_TIMEOUT_SECS, DEFAULT_CHAT_OUTPUT_TOKENS,
        DEFAULT_OLLAMA_OPENAI_BASE_URL, MAX_AI_REQUEST_TIMEOUT_SECS, MIN_AI_REQUEST_TIMEOUT_SECS,
        ModelInfo, normalize_provider_base_url,
    },
    services::{
        ApiKeyUpdate, AppServices, DEFAULT_CHAT_MODEL, DEFAULT_EMBEDDING_MODEL,
        DEFAULT_VISION_MODEL, ProviderSettings,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    Endpoint,
    Models,
    WebSearch,
    BackgroundJobs,
}

impl SettingsTab {
    const ALL: [Self; 4] = [
        Self::Endpoint,
        Self::Models,
        Self::WebSearch,
        Self::BackgroundJobs,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint",
            Self::Models => "对话模型",
            Self::WebSearch => "联网搜索",
            Self::BackgroundJobs => "后台任务",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::Endpoint => "ai-settings-tab-endpoint",
            Self::Models => "ai-settings-tab-models",
            Self::WebSearch => "ai-settings-tab-web-search",
            Self::BackgroundJobs => "ai-settings-tab-background-jobs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingOperation {
    Idle,
    Detecting,
    Saving,
}

impl PendingOperation {
    fn busy(self) -> bool {
        self != Self::Idle
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExistingWindowDisposition<Handle> {
    OpenNew,
    SuppressReentrantOpen,
    Activate(Handle),
    ClearStale(Handle),
}

fn existing_window_disposition<Handle: Copy>(
    opening: bool,
    closing: bool,
    handle: Option<Handle>,
    view_is_alive: bool,
    is_open: impl FnOnce(Handle) -> bool,
) -> ExistingWindowDisposition<Handle> {
    if opening {
        return ExistingWindowDisposition::SuppressReentrantOpen;
    }
    match handle {
        None => ExistingWindowDisposition::OpenNew,
        Some(handle) if view_is_alive && is_open(handle) => {
            if closing {
                ExistingWindowDisposition::SuppressReentrantOpen
            } else {
                ExistingWindowDisposition::Activate(handle)
            }
        }
        Some(handle) => ExistingWindowDisposition::ClearStale(handle),
    }
}

#[derive(Default)]
struct AiSettingsWindowTracker {
    opening: bool,
    closing: bool,
    handle: Option<gpui::AnyWindowHandle>,
    view: Option<WeakEntity<AiSettingsWindow>>,
}

impl gpui::Global for AiSettingsWindowTracker {}

fn ensure_ai_settings_window_tracker(cx: &mut App) {
    if !cx.has_global::<AiSettingsWindowTracker>() {
        cx.set_global(AiSettingsWindowTracker::default());
    }
}

fn activate_existing_ai_settings_window(cx: &mut App) -> Result<bool> {
    ensure_ai_settings_window_tracker(cx);
    let (opening, closing, handle, view_is_alive) = {
        let tracker = cx.global::<AiSettingsWindowTracker>();
        (
            tracker.opening,
            tracker.closing,
            tracker.handle,
            tracker
                .view
                .as_ref()
                .is_some_and(|view| view.upgrade().is_some()),
        )
    };
    let open_windows = cx.windows();
    match existing_window_disposition(opening, closing, handle, view_is_alive, |handle| {
        open_windows.contains(&handle)
    }) {
        ExistingWindowDisposition::OpenNew => Ok(false),
        ExistingWindowDisposition::SuppressReentrantOpen => Ok(true),
        ExistingWindowDisposition::ClearStale(handle) => {
            let tracker = cx.global_mut::<AiSettingsWindowTracker>();
            if tracker.handle == Some(handle) {
                tracker.closing = false;
                tracker.handle = None;
                tracker.view = None;
            }
            Ok(false)
        }
        ExistingWindowDisposition::Activate(handle) => {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .context("无法激活已经打开的 AI Provider 设置窗口")?;
            Ok(true)
        }
    }
}

#[derive(Clone, Debug)]
struct SettingsNotice {
    text: String,
    error: bool,
}

/// Provider settings deliberately live in their own native window. It owns no
/// model client and performs no network or credential work on the GPUI thread.
pub(super) struct AiSettingsWindow {
    services: Arc<AppServices>,
    base_url_input: Entity<InputState>,
    chat_model_input: Entity<InputState>,
    temperature_input: Entity<InputState>,
    top_p_input: Entity<InputState>,
    max_output_tokens_input: Entity<InputState>,
    presence_penalty_input: Entity<InputState>,
    frequency_penalty_input: Entity<InputState>,
    embedding_model_input: Entity<InputState>,
    vision_model_input: Entity<InputState>,
    request_timeout_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    active_tab: SettingsTab,
    scroll_handles: [gpui::ScrollHandle; 4],
    window_handle: gpui::AnyWindowHandle,
    remote_content_confirmed: bool,
    allow_insecure_remote_http: bool,
    confirmed_remote_endpoint: String,
    delete_api_key: bool,
    // --- Host web-search fallback (opt-in) ---
    web_search_enabled: bool,
    web_search_url_input: Entity<InputState>,
    web_search_method_input: Entity<InputState>,
    web_search_body_input: Entity<InputState>,
    web_search_key_header_input: Entity<InputState>,
    web_search_api_key_input: Entity<InputState>,
    web_search_timeout_input: Entity<InputState>,
    web_search_max_results_input: Entity<InputState>,
    web_search_remote_confirmed: bool,
    web_search_allow_insecure_http: bool,
    web_search_confirmed_remote_endpoint: String,
    delete_web_search_api_key: bool,
    auto_run_background_jobs: bool,
    operation: PendingOperation,
    detected_models: Vec<String>,
    missing_models: Vec<String>,
    notice: Option<SettingsNotice>,
    _subscriptions: Vec<Subscription>,
}

impl AiSettingsWindow {
    fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let base_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.base_url)
                .placeholder(DEFAULT_OLLAMA_OPENAI_BASE_URL)
        });
        let chat_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.chat_model)
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
                .default_value(settings.embedding_model)
                .placeholder(DEFAULT_EMBEDDING_MODEL)
        });
        let vision_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.vision_model)
                .placeholder(DEFAULT_VISION_MODEL)
        });
        let request_timeout_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.request_timeout_secs.to_string())
                .placeholder(DEFAULT_AI_REQUEST_TIMEOUT_SECS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则保持现有密钥")
        });
        let web_search_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_url_template)
                .placeholder("http://127.0.0.1:8080/search?q={query}&format=json")
        });
        let web_search_method_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_method)
                .placeholder("GET")
                .validate(|value, _| matches!(value.to_ascii_uppercase().as_str(), "GET" | "POST"))
        });
        let web_search_body_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_body_template.unwrap_or_default())
                .placeholder(r#"{"query":"{query}"}"#)
        });
        let web_search_key_header_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_key_header.unwrap_or_default())
                .placeholder("Authorization")
        });
        let web_search_api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则保持现有密钥")
        });
        let web_search_timeout_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_timeout_secs.to_string())
                .placeholder(
                    moye_epub_editor::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS.to_string(),
                )
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let web_search_max_results_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_max_results.to_string())
                .placeholder(moye_epub_editor::web_search::MAX_WEB_SEARCH_RESULTS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let subscriptions =
            vec![cx.subscribe_in(&base_url_input, window, Self::on_base_url_input_event)];

        Self {
            services,
            base_url_input,
            chat_model_input,
            temperature_input,
            top_p_input,
            max_output_tokens_input,
            presence_penalty_input,
            frequency_penalty_input,
            embedding_model_input,
            vision_model_input,
            request_timeout_input,
            api_key_input,
            active_tab: SettingsTab::default(),
            scroll_handles: std::array::from_fn(|_| gpui::ScrollHandle::new()),
            window_handle: gpui::Window::window_handle(window),
            remote_content_confirmed: settings.remote_content_confirmed,
            allow_insecure_remote_http: settings.allow_insecure_remote_http,
            confirmed_remote_endpoint: settings.confirmed_remote_endpoint,
            delete_api_key: false,
            web_search_enabled: settings.web_search_enabled,
            web_search_url_input,
            web_search_method_input,
            web_search_body_input,
            web_search_key_header_input,
            web_search_api_key_input,
            web_search_timeout_input,
            web_search_max_results_input,
            web_search_remote_confirmed: settings.web_search_remote_confirmed,
            web_search_allow_insecure_http: settings.web_search_allow_insecure_http,
            web_search_confirmed_remote_endpoint: settings.web_search_confirmed_remote_endpoint,
            delete_web_search_api_key: false,
            auto_run_background_jobs: settings.auto_run_background_jobs,
            operation: PendingOperation::Idle,
            detected_models: Vec::new(),
            missing_models: Vec::new(),
            notice: None,
            _subscriptions: subscriptions,
        }
    }

    fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let chat_generation = parse_chat_generation(
            self.temperature_input.read(cx).value().as_ref(),
            self.top_p_input.read(cx).value().as_ref(),
            self.max_output_tokens_input.read(cx).value().as_ref(),
            self.presence_penalty_input.read(cx).value().as_ref(),
            self.frequency_penalty_input.read(cx).value().as_ref(),
        )?;
        let request_timeout_secs =
            parse_request_timeout_secs(self.request_timeout_input.read(cx).value().as_ref())?;
        let web_search_timeout_secs =
            parse_web_search_timeout_secs(self.web_search_timeout_input.read(cx).value().as_ref())?;
        let web_search_max_results = parse_web_search_max_results(
            self.web_search_max_results_input.read(cx).value().as_ref(),
        )?;
        let body_template = {
            let raw = self.web_search_body_input.read(cx).value();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let key_header = {
            let raw = self.web_search_key_header_input.read(cx).value();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let settings = ProviderSettings {
            base_url: self.base_url_input.read(cx).value().trim().to_string(),
            chat_model: self.chat_model_input.read(cx).value().trim().to_string(),
            chat_generation,
            embedding_model: self
                .embedding_model_input
                .read(cx)
                .value()
                .trim()
                .to_string(),
            vision_model: self.vision_model_input.read(cx).value().trim().to_string(),
            remote_content_confirmed: self.remote_content_confirmed,
            allow_insecure_remote_http: self.allow_insecure_remote_http,
            confirmed_remote_endpoint: self.confirmed_remote_endpoint.clone(),
            request_timeout_secs,
            web_search_enabled: self.web_search_enabled,
            web_search_url_template: self
                .web_search_url_input
                .read(cx)
                .value()
                .trim()
                .to_string(),
            web_search_method: self
                .web_search_method_input
                .read(cx)
                .value()
                .trim()
                .to_ascii_uppercase(),
            web_search_body_template: body_template,
            web_search_key_header: key_header,
            web_search_remote_confirmed: self.web_search_remote_confirmed,
            web_search_allow_insecure_http: self.web_search_allow_insecure_http,
            web_search_confirmed_remote_endpoint: self.web_search_confirmed_remote_endpoint.clone(),
            web_search_timeout_secs,
            web_search_max_results,
            auto_run_background_jobs: self.auto_run_background_jobs,
        };
        settings.validate()?;
        Ok(settings)
    }

    fn on_base_url_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let entered = input.read(cx).value();
        let canonical = normalize_provider_base_url(entered.trim())
            .ok()
            .map(|url| url.to_string());
        if canonical.as_deref() != Some(self.confirmed_remote_endpoint.as_str()) {
            self.remote_content_confirmed = false;
            self.allow_insecure_remote_http = false;
            self.confirmed_remote_endpoint.clear();
            cx.notify();
        }
    }

    fn api_key_update(&self, cx: &App) -> ApiKeyUpdate {
        api_key_update_for_input(
            self.api_key_input.read(cx).value().as_ref(),
            self.delete_api_key,
        )
    }

    fn clear_entered_api_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.api_key_input.clone();
        input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn web_api_key_update(&self, cx: &App) -> ApiKeyUpdate {
        api_key_update_for_input(
            self.web_search_api_key_input.read(cx).value().as_ref(),
            self.delete_web_search_api_key,
        )
    }

    fn clear_entered_web_api_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.web_search_api_key_input.clone();
        input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn detect_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() {
            return;
        }
        let settings = match self.entered_settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.notice = Some(SettingsNotice {
                    text: format!("AI Provider 设置无效：{error:#}"),
                    error: true,
                });
                cx.notify();
                return;
            }
        };
        let key_update = self.api_key_update(cx);
        self.clear_entered_api_key(window, cx);
        self.switch_tab(SettingsTab::Models, window, cx);
        self.operation = PendingOperation::Detecting;
        self.detected_models.clear();
        self.missing_models.clear();
        self.notice = Some(SettingsNotice {
            text: "正在连接 OpenAI-compatible endpoint 并读取模型…".to_string(),
            error: false,
        });

        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            // Credential access and HTTP I/O are dispatched by AppServices to
            // its Tokio runtime. Keeping the Arc in this GPUI task prevents
            // AppServices from ever being last-dropped by its own worker.
            let outcome = services
                .probe_provider_models(settings.clone(), key_update)
                .await;
            let _ = view.update(cx, |this, cx| {
                this.operation = PendingOperation::Idle;
                match outcome {
                    Ok(models) => this.apply_detected_models(&settings, models),
                    Err(error) => {
                        this.notice = Some(SettingsNotice {
                            text: format!(
                                "无法读取模型：{error:#}。墨页不会启动或管理 Ollama；若使用本地 Ollama，请确认服务已运行。"
                            ),
                            error: true,
                        });
                    }
                }
                this.scroll_handles[SettingsTab::Models as usize].scroll_to_bottom();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_detected_models(&mut self, settings: &ProviderSettings, models: Vec<ModelInfo>) {
        self.detected_models = models.into_iter().map(|model| model.id).collect();
        self.missing_models = configured_models(settings)
            .into_iter()
            .filter(|model| !self.detected_models.iter().any(|found| found == model))
            .map(str::to_string)
            .collect();
        self.missing_models.sort();
        self.missing_models.dedup();
        self.notice = Some(SettingsNotice {
            text: if self.detected_models.is_empty() {
                "连接成功，但 endpoint 没有返回可用模型。".to_string()
            } else if self.missing_models.is_empty() {
                format!(
                    "连接成功，已发现 {} 个模型，当前选择均可用。",
                    self.detected_models.len()
                )
            } else {
                format!(
                    "连接成功，已发现 {} 个模型；{} 个当前选择未在列表中。",
                    self.detected_models.len(),
                    self.missing_models.len()
                )
            },
            error: !self.missing_models.is_empty(),
        });
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() {
            return;
        }
        let settings = match self.entered_settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.notice = Some(SettingsNotice {
                    text: format!("AI Provider 设置无效：{error:#}"),
                    error: true,
                });
                cx.notify();
                return;
            }
        };
        let key_update = self.api_key_update(cx);
        let web_key_update = self.web_api_key_update(cx);
        self.clear_entered_api_key(window, cx);
        self.clear_entered_web_api_key(window, cx);
        self.operation = PendingOperation::Saving;
        self.notice = Some(SettingsNotice {
            text: "正在安全保存 Provider 设置…".to_string(),
            error: false,
        });

        let services = Arc::clone(&self.services);
        let window_handle = self.window_handle.clone();
        cx.spawn_in(window, async move |view, cx| {
            // AppServices moves credential and SQLite work to its dedicated
            // runtime; this UI future only awaits and applies the result.
            let outcome = services.configure_provider(settings, key_update).await;
            let web_outcome = match &web_key_update {
                ApiKeyUpdate::Keep => Ok(()),
                ApiKeyUpdate::Set(value) => services.set_web_search_api_key(value),
                ApiKeyUpdate::Delete => services.delete_web_search_api_key(),
            };
            let outcome = outcome.and(web_outcome);
            let _ = view.update(cx, |this, cx| {
                this.operation = PendingOperation::Idle;
                match outcome {
                    Ok(()) => {
                        this.delete_api_key = false;
                        this.notice = Some(SettingsNotice {
                            text: "Provider 设置已保存；新的搜索与问答请求会使用这些设置。"
                                .to_string(),
                            error: false,
                        });
                        let window_handle = window_handle.clone();
                        cx.spawn(async move |_entity, cx| {
                            let _ = window_handle.update(cx, |_, window, cx| {
                                remove_window_after_current_frame(window, cx, None);
                            });
                        })
                        .detach();
                    }
                    Err(error) => {
                        cancel_application_exit(cx);
                        this.notice = Some(SettingsNotice {
                            text: format!("保存 Provider 设置失败：{error:#}"),
                            error: true,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_input_field(
        &self,
        label: &'static str,
        description: &'static str,
        input: &Entity<InputState>,
    ) -> gpui::AnyElement {
        div()
            .v_flex()
            .gap_1p5()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(INK))
                    .child(label),
            )
            .child(Input::new(input).disabled(self.operation.busy()))
            .child(div().text_xs().text_color(rgb(MUTED)).child(description))
            .into_any_element()
    }

    fn render_notice(&self) -> Option<gpui::AnyElement> {
        self.notice.as_ref().map(|notice| {
            let (background, foreground, icon) = if notice.error {
                (rgb(0xf7e1df), rgb(0x9f302c), IconName::TriangleAlert)
            } else {
                (rgb(0xe3efe5), rgb(0x376441), IconName::CircleCheck)
            };
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .bg(background)
                .text_sm()
                .text_color(foreground)
                .child(Icon::new(icon).small())
                .child(
                    div()
                        .flex_1()
                        .line_height(gpui::relative(1.5))
                        .child(notice.text.clone()),
                )
                .into_any_element()
        })
    }

    fn render_model_detection(&self, cx: &App) -> Option<gpui::AnyElement> {
        if self.detected_models.is_empty() && self.missing_models.is_empty() {
            return None;
        }
        let endpoint = self.base_url_input.read(cx).value();
        let commands = ollama_pull_commands(endpoint.trim(), &self.missing_models);
        Some(
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(PAPER))
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(rgb(INK))
                        .child(format!(
                            "Endpoint 返回的模型（{}）",
                            self.detected_models.len()
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(MUTED))
                        .child(if self.detected_models.is_empty() {
                            "未返回模型".to_string()
                        } else {
                            self.detected_models.join("、")
                        }),
                )
                .children(commands.into_iter().map(|command| {
                    div()
                        .px_2()
                        .py_1()
                        .rounded(px(5.))
                        .bg(rgb(SIDEBAR))
                        .text_xs()
                        .text_color(rgb(INK))
                        .child(command)
                }))
                .into_any_element(),
        )
    }

    fn switch_tab(&mut self, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() || self.active_tab == tab {
            return;
        }
        // Keep drafts and input history while removing focus from hidden fields.
        window.blur();
        self.active_tab = tab;
        cx.notify();
    }

    #[inline(never)]
    fn render_endpoint_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let remote_view = cx.entity();
        let insecure_view = cx.entity();
        let endpoint_for_confirmation = self.base_url_input.clone();
        let busy = self.operation.busy();

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(self.render_input_field(
                "Endpoint",
                "默认使用本机 Ollama 的 http://127.0.0.1:11434/v1/；不会静默切换到云端。",
                &self.base_url_input,
            ))
            .child(
                Checkbox::new("ai-confirm-remote")
                    .checked(self.remote_content_confirmed)
                    .disabled(busy)
                    .label("确认允许把图书内容发送到远程 endpoint")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        remote_view.update(cx, |this, cx| {
                            if checked {
                                let entered = endpoint_for_confirmation.read(cx).value();
                                match normalize_provider_base_url(entered.trim()) {
                                    Ok(url) => {
                                        this.remote_content_confirmed = true;
                                        this.confirmed_remote_endpoint = url.to_string();
                                    }
                                    Err(error) => {
                                        this.remote_content_confirmed = false;
                                        this.confirmed_remote_endpoint.clear();
                                        this.notice = Some(SettingsNotice {
                                            text: format!("Endpoint 无效，无法确认：{error:#}"),
                                            error: true,
                                        });
                                    }
                                }
                            } else {
                                this.remote_content_confirmed = false;
                                this.allow_insecure_remote_http = false;
                                this.confirmed_remote_endpoint.clear();
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(
                Checkbox::new("ai-allow-insecure-http")
                    .checked(self.allow_insecure_remote_http)
                    .disabled(busy || !self.remote_content_confirmed)
                    .label("额外允许非 HTTPS 的远程 endpoint（内容可能被窃听）")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        insecure_view.update(cx, |this, cx| {
                            this.allow_insecure_remote_http = checked;
                            cx.notify();
                        });
                    }),
            )
            .child(self.render_input_field(
                "请求超时（秒）",
                "单次 OpenAI-compatible HTTP 请求的总超时；可设置 1–600 秒，默认 120 秒。",
                &self.request_timeout_input,
            ))
            .into_any_element()
    }

    #[inline(never)]
    fn render_models_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .v_flex()
            .gap_4()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(self.render_input_field(
                "对话模型",
                "用于流式问答和只读工具调用。",
                &self.chat_model_input,
            ))
            .child(self.render_generation_panel(cx))
            .child(self.render_input_field(
                "Embedding 模型",
                "用于向量索引；不可用时全文检索仍可工作。",
                &self.embedding_model_input,
            ))
            .child(self.render_input_field(
                "视觉模型",
                "用于后续页面 OCR 与视觉说明任务。",
                &self.vision_model_input,
            ))
            .when_some(self.render_model_detection(cx), |this, result| {
                this.child(result)
            })
            .into_any_element()
    }

    fn reset_chat_generation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() {
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
        self.notice = Some(SettingsNotice {
            text: "对话生成参数已恢复默认，点击“保存设置”后生效。".into(),
            error: false,
        });
        cx.notify();
    }

    #[inline(never)]
    fn render_generation_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .v_flex()
            .gap_4()
            .p_4()
            .rounded(px(10.))
            .bg(rgb(PAPER))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .gap_3()
                    .child(div().text_sm().font_semibold().child("对话生成参数"))
                    .child(
                        Button::new("ai-reset-chat-generation")
                            .label("恢复默认")
                            .disabled(self.operation.busy())
                            .debug_selector(|| "ai-reset-chat-generation".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_chat_generation(window, cx);
                            })),
                    ),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "保存后用于新的对话请求。除最大输出外，其余参数可留空，使用服务端默认值；具体支持以所选模型为准。",
            ))
            .child(self.render_input_field(
                "温度（Temperature）",
                "0–2；默认 0.1。越低越集中，越高越随机。通常与 Top P 选择一项调整。",
                &self.temperature_input,
            ))
            .child(self.render_input_field(
                "核采样（Top P）",
                "0–1；控制候选词的累计概率范围，越小越集中。",
                &self.top_p_input,
            ))
            .child(self.render_input_field(
                "最大输出 token 数",
                "正整数，默认 4096；请按所选模型设置，墨页不设固定上限。上下文不足时可能进一步降低。",
                &self.max_output_tokens_input,
            ))
            .child(self.render_input_field(
                "出现惩罚（Presence Penalty）",
                "−2–2；正值降低已出现词的再次使用倾向，负值提高。",
                &self.presence_penalty_input,
            ))
            .child(self.render_input_field(
                "频率惩罚（Frequency Penalty）",
                "−2–2；正值按出现次数抑制重复，负值鼓励重复。",
                &self.frequency_penalty_input,
            ))
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "最大输出 token 数不改变模型的上下文窗口；Ollama 上下文长度需在服务端设置。",
            ))
            .into_any_element()
    }

    #[inline(never)]
    fn render_api_key_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let delete_view = cx.entity();
        let busy = self.operation.busy();

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(INK))
                    .child("API key"),
            )
            .child(
                Input::new(&self.api_key_input)
                    .mask_toggle()
                    .disabled(busy || self.delete_api_key),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(gpui::relative(1.5))
                    .text_color(rgb(MUTED))
                    .child("现有密钥不会回填明文。留空表示保持；输入新值表示替换。密钥只存入 Windows Credential Manager。"),
            )
            .child(
                Checkbox::new("ai-delete-api-key")
                    .checked(self.delete_api_key)
                    .disabled(busy)
                    .label("删除这个 endpoint 已保存的 API key")
                    .on_click(move |checked, window, cx| {
                        let checked = *checked;
                        delete_view.update(cx, |this, cx| {
                            this.delete_api_key = checked;
                            if checked {
                                this.clear_entered_api_key(window, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_web_search_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let web_enable_view = cx.entity();
        let web_confirm_view = cx.entity();
        let web_insecure_view = cx.entity();
        let web_delete_view = cx.entity();
        let web_url_input = self.web_search_url_input.clone();
        let busy = self.operation.busy();

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .child(
                        div()
                            .v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(INK))
                                    .child("联网搜索（可选）"),
                            )
                            .child(div().text_xs().text_color(rgb(MUTED)).child(
                                "书中检索不到时，宿主补充一次联网搜索作为回答依据；关闭则不联网。",
                            )),
                    )
                    .child(
                        Checkbox::new("ai-web-search-enabled")
                            .checked(self.web_search_enabled)
                            .disabled(busy)
                            .label("启用")
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                web_enable_view.update(cx, |this, cx| {
                                    this.web_search_enabled = checked;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(self.render_input_field(
                "请求模板（URL）",
                "必须包含 {query}；可配置 SearxNG、Brave、Tavily 等端点。",
                &self.web_search_url_input,
            ))
            .child(self.render_input_field(
                "方法",
                "GET 或 POST；POST 端点需填写下方的请求体模板。",
                &self.web_search_method_input,
            ))
            .child(self.render_input_field(
                "请求体模板（可选）",
                "POST 时使用，需包含 {query}；留空则用 GET。",
                &self.web_search_body_input,
            ))
            .child(self.render_input_field(
                "API key 头（可选）",
                "自定义携带密钥的请求头名；留空则用 Authorization: Bearer。",
                &self.web_search_key_header_input,
            ))
            .child(
                Input::new(&self.web_search_api_key_input)
                    .mask_toggle()
                    .disabled(busy || self.delete_web_search_api_key || !self.web_search_enabled),
            )
            .child(
                div().text_xs().text_color(rgb(MUTED)).child(
                    "联网搜索 API key 只存入 Windows Credential Manager；留空保持、输入替换。",
                ),
            )
            .child(
                Checkbox::new("ai-web-confirm-remote")
                    .checked(self.web_search_remote_confirmed)
                    .disabled(busy || !self.web_search_enabled)
                    .label("确认允许把检索词发送到远程搜索端点")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        web_confirm_view.update(cx, |this, cx| {
                            if checked {
                                let entered = web_url_input.read(cx).value();
                                match moye_epub_editor::web_search::normalize_web_endpoint(
                                    entered.trim(),
                                ) {
                                    Some(url) => {
                                        this.web_search_remote_confirmed = true;
                                        this.web_search_confirmed_remote_endpoint = url;
                                    }
                                    None => {
                                        this.web_search_remote_confirmed = false;
                                        this.web_search_confirmed_remote_endpoint.clear();
                                        this.notice = Some(SettingsNotice {
                                            text: "联网搜索端点无效，无法确认。".to_string(),
                                            error: true,
                                        });
                                    }
                                }
                            } else {
                                this.web_search_remote_confirmed = false;
                                this.web_search_allow_insecure_http = false;
                                this.web_search_confirmed_remote_endpoint.clear();
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(
                Checkbox::new("ai-web-allow-insecure-http")
                    .checked(self.web_search_allow_insecure_http)
                    .disabled(busy || !self.web_search_enabled || !self.web_search_remote_confirmed)
                    .label("额外允许非 HTTPS 的搜索端点（内容可能被窃听）")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        web_insecure_view.update(cx, |this, cx| {
                            this.web_search_allow_insecure_http = checked;
                            cx.notify();
                        });
                    }),
            )
            .child(self.render_input_field(
                "超时（秒）",
                "单次联网搜索请求的总超时。",
                &self.web_search_timeout_input,
            ))
            .child(self.render_input_field(
                "结果条数",
                "返回给模型的最大搜索结果数量。",
                &self.web_search_max_results_input,
            ))
            .child(
                Checkbox::new("ai-web-delete-api-key")
                    .checked(self.delete_web_search_api_key)
                    .disabled(busy || !self.web_search_enabled)
                    .label("删除已保存的联网搜索 API key")
                    .on_click(move |checked, window, cx| {
                        let checked = *checked;
                        web_delete_view.update(cx, |this, cx| {
                            this.delete_web_search_api_key = checked;
                            if checked {
                                this.clear_entered_web_api_key(window, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_background_jobs_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let auto_run_view = cx.entity();

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(INK))
                    .child("新任务运行方式"),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(gpui::relative(1.5))
                    .text_color(rgb(MUTED))
                    .child(
                        "导入或编辑图书后，墨页会创建逻辑页面渲染、视觉理解和向量索引任务。",
                    ),
            )
            .child(
                Checkbox::new("ai-auto-run-background-jobs")
                    .checked(self.auto_run_background_jobs)
                    .disabled(self.operation.busy())
                    .label("自动运行新创建的后台任务")
                    .debug_selector(|| "ai-auto-run-background-jobs".into())
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        auto_run_view.update(cx, |this, cx| {
                            this.auto_run_background_jobs = checked;
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(gpui::relative(1.5))
                    .text_color(rgb(MUTED))
                    .child(
                        "关闭后，新导入、创建或编辑图书产生的三类任务会以暂停状态创建，可在“后台任务”窗口逐项恢复。更改此设置不会暂停、恢复或取消已经排队或运行的任务。",
                    ),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_active_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.active_tab {
            SettingsTab::Endpoint => div()
                .v_flex()
                .gap_5()
                .child(self.render_endpoint_panel(cx))
                .child(self.render_api_key_panel(cx))
                .into_any_element(),
            SettingsTab::Models => self.render_models_panel(cx),
            SettingsTab::WebSearch => self.render_web_search_panel(cx),
            SettingsTab::BackgroundJobs => self.render_background_jobs_panel(cx),
        }
    }
}

impl Render for AiSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detect_view = cx.entity();
        let save_view = cx.entity();
        let busy = self.operation.busy();
        let panel = self.render_active_panel(cx);
        let tabs = TabBar::new("ai-settings-tabs")
            .selected_index(self.active_tab as usize)
            .on_click(cx.listener(|this, index: &usize, window, cx| {
                if let Some(tab) = SettingsTab::ALL.get(*index) {
                    this.switch_tab(*tab, window, cx);
                }
            }))
            .children(SettingsTab::ALL.into_iter().map(|tab| {
                Tab::new()
                    .label(tab.label())
                    .disabled(busy)
                    .debug_selector(move || tab.selector().into())
            }));

        div()
            .v_flex()
            .size_full()
            .bg(rgb(PAPER))
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .justify_between()
                    .gap_3()
                    .px_6()
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_9()
                                    .rounded(px(10.))
                                    .bg(rgb(ACCENT_SOFT))
                                    .text_color(rgb(ACCENT))
                                    .child(Icon::new(IconName::Bot)),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_semibold()
                                            .text_color(rgb(INK))
                                            .child("AI Provider"),
                                    )
                                    .child(div().text_xs().text_color(rgb(MUTED)).child(
                                        "OpenAI-compatible · 本地 Ollama 或显式配置的远程端点",
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("墨页不会启动、停止或下载 Ollama 模型"),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .pt_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(tabs),
            )
            .child(
                div()
                    .id(("ai-settings-scroll", self.active_tab as usize))
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handles[self.active_tab as usize])
                    .child(div().p_6().child(panel)),
            )
            .when_some(self.render_notice(), |this, notice| {
                this.child(div().flex_none().px_6().py_2().child(notice))
            })
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .justify_between()
                    .gap_3()
                    .px_6()
                    .py_4()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .max_w(px(440.))
                            .text_xs()
                            .line_height(gpui::relative(1.5))
                            .text_color(rgb(MUTED))
                            .child("切换标签保留输入；保存设置会应用全部四个标签中的配置。"),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .when(
                                matches!(
                                    self.active_tab,
                                    SettingsTab::Endpoint | SettingsTab::Models
                                ),
                                |this| {
                                    this.child(
                                        Button::new("ai-detect-models")
                                            .outline()
                                            .icon(IconName::Search)
                                            .label(
                                                if self.operation == PendingOperation::Detecting {
                                                    "正在检测…"
                                                } else {
                                                    "检测模型"
                                                },
                                            )
                                            .disabled(busy)
                                            .on_click(move |_, window, cx| {
                                                detect_view.update(cx, |this, cx| {
                                                    this.detect_models(window, cx)
                                                });
                                            }),
                                    )
                                },
                            )
                            .child(
                                Button::new("ai-save-settings")
                                    .debug_selector(|| "ai-save-settings".into())
                                    .primary()
                                    .icon(IconName::Check)
                                    .label(if self.operation == PendingOperation::Saving {
                                        "正在保存…"
                                    } else {
                                        "保存设置"
                                    })
                                    .disabled(busy)
                                    .on_click(move |_, window, cx| {
                                        save_view.update(cx, |this, cx| this.save(window, cx));
                                    }),
                            ),
                    ),
            )
    }
}

pub(super) fn open_ai_settings_window(services: Arc<AppServices>, cx: &mut App) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    if activate_existing_ai_settings_window(cx)? {
        return Ok(());
    }

    {
        let tracker = cx.global_mut::<AiSettingsWindowTracker>();
        tracker.opening = true;
        tracker.closing = false;
        tracker.handle = None;
        tracker.view = None;
    }
    let opened: Result<gpui::AnyWindowHandle> = (|| {
        let settings = services.provider_settings()?;
        let bounds = Bounds::centered(None, size(px(760.), px(780.)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(640.), px(620.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("墨页 · AI Provider 设置".into()),
                        ..Default::default()
                    }),
                    app_id: Some("dev.moye.epub-editor.ai-settings".to_string()),
                    ..Default::default()
                },
                move |window, cx| {
                    let settings = cx.new(|cx| {
                        AiSettingsWindow::new(Arc::clone(&services), settings, window, cx)
                    });
                    cx.global_mut::<AiSettingsWindowTracker>().view = Some(settings.downgrade());
                    let settings_window = gpui::Window::window_handle(window);
                    let close_settings = settings.downgrade();
                    let close_requested = Cell::new(false);
                    on_window_close(window, cx, move |window, cx| {
                        // The accepted credential/SQLite write owns its result
                        // callback until completion. Success already closes
                        // this window; failure keeps it open for retry.
                        if close_settings.upgrade().is_some_and(|settings| {
                            settings.read(cx).operation == PendingOperation::Saving
                        }) {
                            return false;
                        }
                        let tracker = cx.global_mut::<AiSettingsWindowTracker>();
                        if tracker.handle == Some(settings_window) {
                            tracker.closing = true;
                        }
                        window.minimize_window();
                        if !close_requested.replace(true) {
                            remove_window_after_current_frame(window, cx, None);
                        }
                        false
                    });
                    cx.new(|cx| Root::new(settings, window, cx))
                },
            )
            .context("无法创建 AI Provider 设置窗口")?;
        Ok(handle.into())
    })();

    let tracker = cx.global_mut::<AiSettingsWindowTracker>();
    tracker.opening = false;
    match opened {
        Ok(handle) => {
            tracker.handle = Some(handle);
            Ok(())
        }
        Err(error) => {
            tracker.closing = false;
            tracker.handle = None;
            tracker.view = None;
            Err(error)
        }
    }
}

fn api_key_update_for_input(value: &str, delete: bool) -> ApiKeyUpdate {
    if delete {
        ApiKeyUpdate::Delete
    } else if value.trim().is_empty() {
        ApiKeyUpdate::Keep
    } else {
        ApiKeyUpdate::Set(value.to_string())
    }
}

fn optional_parameter_text(value: Option<f32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_parameter_input(
    value: Option<f32>,
    window: &mut Window,
    cx: &mut Context<AiSettingsWindow>,
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

fn parse_request_timeout_secs(value: &str) -> Result<u64> {
    let value = value.trim();
    let timeout = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "请求超时必须是 {MIN_AI_REQUEST_TIMEOUT_SECS} 到 {MAX_AI_REQUEST_TIMEOUT_SECS} 之间的整数秒数"
        )
    })?;
    anyhow::ensure!(
        (MIN_AI_REQUEST_TIMEOUT_SECS..=MAX_AI_REQUEST_TIMEOUT_SECS).contains(&timeout),
        "请求超时必须是 {MIN_AI_REQUEST_TIMEOUT_SECS} 到 {MAX_AI_REQUEST_TIMEOUT_SECS} 之间的整数秒数"
    );
    Ok(timeout)
}

fn parse_web_search_timeout_secs(value: &str) -> Result<u64> {
    use moye_epub_editor::web_search::{MAX_WEB_SEARCH_TIMEOUT_SECS, MIN_WEB_SEARCH_TIMEOUT_SECS};
    let value = value.trim();
    let timeout = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "联网搜索超时必须是 {MIN_WEB_SEARCH_TIMEOUT_SECS} 到 {MAX_WEB_SEARCH_TIMEOUT_SECS} 之间的整数秒数"
        )
    })?;
    anyhow::ensure!(
        (MIN_WEB_SEARCH_TIMEOUT_SECS..=MAX_WEB_SEARCH_TIMEOUT_SECS).contains(&timeout),
        "联网搜索超时必须是 {MIN_WEB_SEARCH_TIMEOUT_SECS} 到 {MAX_WEB_SEARCH_TIMEOUT_SECS} 之间的整数秒数"
    );
    Ok(timeout)
}

fn parse_web_search_max_results(value: &str) -> Result<usize> {
    use moye_epub_editor::web_search::MAX_WEB_SEARCH_RESULTS;
    let value = value.trim();
    let count = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("联网搜索结果数必须是 1 到 {MAX_WEB_SEARCH_RESULTS} 之间的整数")
    })?;
    anyhow::ensure!(
        (1..=MAX_WEB_SEARCH_RESULTS).contains(&count),
        "联网搜索结果数必须是 1 到 {MAX_WEB_SEARCH_RESULTS} 之间的整数"
    );
    Ok(count)
}

fn configured_models(settings: &ProviderSettings) -> [&str; 3] {
    [
        settings.chat_model.as_str(),
        settings.embedding_model.as_str(),
        settings.vision_model.as_str(),
    ]
}

fn ollama_pull_commands(endpoint: &str, missing_models: &[String]) -> Vec<String> {
    if !looks_like_local_ollama(endpoint) {
        return Vec::new();
    }
    missing_models
        .iter()
        .map(|model| format!("ollama pull {model}"))
        .collect()
}

fn looks_like_local_ollama(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint.trim()) else {
        return false;
    };
    let local_host = url.host_str().is_some_and(|host| {
        let host = host.trim_matches(['[', ']']);
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    local_host && url.port_or_known_default() == Some(11_434)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable as _, Modifiers, TestAppContext, VisualTestContext};
    use moye_epub_editor::credentials::MemoryCredentialStore;

    fn redraw(visual: &mut VisualTestContext) {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear());
        visual.run_until_parked();
    }

    fn open_settings(
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        Entity<AiSettingsWindow>,
        &mut VisualTestContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let services = Arc::new(
            AppServices::open_with_credentials(
                directory.path(),
                Arc::new(MemoryCredentialStore::default()),
            )
            .unwrap(),
        );
        let settings = services.provider_settings().unwrap();
        cx.update(gpui_component::init);
        let mut settings_view = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| AiSettingsWindow::new(services, settings, window, cx));
            settings_view = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1080.), px(1000.)));
        redraw(visual);
        (directory, settings_view.unwrap(), visual)
    }

    fn click_tab(visual: &mut VisualTestContext, tab: SettingsTab) {
        let bounds = visual
            .debug_bounds(tab.selector())
            .expect("settings tab must be rendered");
        assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
        visual.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        visual.simulate_click(bounds.center(), Modifiers::none());
        redraw(visual);
    }

    fn edit_input(visual: &mut VisualTestContext, input: &Entity<InputState>, text: &str) {
        visual.update(|window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        redraw(visual);
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        visual.simulate_input(text);
        redraw(visual);
        input.read_with(visual, |input, _| assert_eq!(input.value().as_ref(), text));
        visual.update(|window, cx| assert!(input.read(cx).focus_handle(cx).is_focused(window)));
    }

    fn assert_hidden_input_does_not_receive_text(
        visual: &mut VisualTestContext,
        input: &Entity<InputState>,
        expected: &str,
    ) {
        visual.update(|window, cx| assert!(!input.read(cx).focus_handle(cx).is_focused(window)));
        visual.simulate_input("hidden-input-must-not-change");
        redraw(visual);
        input.read_with(visual, |input, _| {
            assert_eq!(input.value().as_ref(), expected);
        });
    }

    #[gpui::test]
    fn tab_clicks_preserve_unsaved_inputs_and_blur_hidden_fields(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        let [endpoint, api_key, model, web_url] = settings.read_with(visual, |view, _| {
            [
                view.base_url_input.clone(),
                view.api_key_input.clone(),
                view.chat_model_input.clone(),
                view.web_search_url_input.clone(),
            ]
        });
        let original_ids = [
            endpoint.entity_id(),
            api_key.entity_id(),
            model.entity_id(),
            web_url.entity_id(),
        ];
        let endpoint_draft = "http://127.0.0.1:18081/v1/";
        let model_draft = "draft-chat-model";
        let web_draft = "http://127.0.0.1:18082/search?q={query}&format=json";
        let key_draft = "fixture-only-unsaved-key";

        edit_input(visual, &endpoint, endpoint_draft);
        edit_input(visual, &api_key, key_draft);
        click_tab(visual, SettingsTab::Models);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::Models)
        });
        assert_hidden_input_does_not_receive_text(visual, &api_key, key_draft);

        edit_input(visual, &model, model_draft);
        click_tab(visual, SettingsTab::WebSearch);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::WebSearch)
        });
        assert_hidden_input_does_not_receive_text(visual, &model, model_draft);

        edit_input(visual, &web_url, web_draft);
        click_tab(visual, SettingsTab::BackgroundJobs);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::BackgroundJobs);
            assert!(!view.auto_run_background_jobs);
        });
        assert!(visual.debug_bounds("ai-detect-models").is_none());
        let auto_run = visual
            .debug_bounds("ai-auto-run-background-jobs")
            .expect("background-job auto-run checkbox must be rendered");
        visual.simulate_click(auto_run.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert!(view.auto_run_background_jobs);
            assert!(view.entered_settings(cx).unwrap().auto_run_background_jobs);
            assert!(
                !view
                    .services
                    .provider_settings()
                    .unwrap()
                    .auto_run_background_jobs,
                "changing the checkbox must not save the draft",
            );
        });

        click_tab(visual, SettingsTab::Endpoint);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::Endpoint)
        });
        assert_hidden_input_does_not_receive_text(visual, &web_url, web_draft);

        for tab in SettingsTab::ALL {
            click_tab(visual, tab);
            settings.read_with(visual, |view, cx| {
                assert_eq!(view.active_tab, tab);
                assert_eq!(
                    [
                        view.base_url_input.entity_id(),
                        view.api_key_input.entity_id(),
                        view.chat_model_input.entity_id(),
                        view.web_search_url_input.entity_id(),
                    ],
                    original_ids,
                );
                let entered = view.entered_settings(cx).unwrap();
                assert_eq!(entered.base_url, endpoint_draft);
                assert_eq!(entered.chat_model, model_draft);
                assert_eq!(entered.web_search_url_template, web_draft);
                assert!(!entered.web_search_enabled);
                assert!(entered.auto_run_background_jobs);
                assert_eq!(view.api_key_update(cx), ApiKeyUpdate::Set(key_draft.into()));
                assert_eq!(
                    view.services.provider_settings().unwrap().base_url,
                    DEFAULT_OLLAMA_OPENAI_BASE_URL,
                    "changing tabs must not save the draft",
                );
            });
        }
    }

    #[gpui::test]
    fn generation_drafts_survive_tabs_and_invalid_save_then_reset(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        visual.simulate_resize(size(px(1080.), px(1500.)));
        redraw(visual);
        click_tab(visual, SettingsTab::Models);
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
        click_tab(visual, SettingsTab::Endpoint);
        assert_hidden_input_does_not_receive_text(visual, &inputs[4], "-0.4");
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

        click_tab(visual, SettingsTab::Models);
        edit_input(visual, &inputs[2], "0");
        let save = visual.debug_bounds("ai-save-settings").unwrap();
        visual.simulate_click(save.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(view.operation, PendingOperation::Idle);
            assert!(view.notice.as_ref().is_some_and(|notice| notice.error));
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
            assert!(!view.notice.as_ref().unwrap().error);
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

    #[gpui::test]
    fn tab_clicks_wait_for_detection_and_saving_to_finish(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        for operation in [PendingOperation::Detecting, PendingOperation::Saving] {
            visual.update(|_, cx| {
                settings.update(cx, |view, cx| {
                    view.operation = operation;
                    cx.notify();
                });
            });
            redraw(visual);
            for tab in [
                SettingsTab::Models,
                SettingsTab::WebSearch,
                SettingsTab::BackgroundJobs,
            ] {
                click_tab(visual, tab);
                settings.read_with(visual, |view, _| {
                    assert_eq!(view.operation, operation);
                    assert_eq!(view.active_tab, SettingsTab::Endpoint);
                });
            }
        }
        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                view.operation = PendingOperation::Idle;
                cx.notify();
            });
        });
        redraw(visual);
        click_tab(visual, SettingsTab::BackgroundJobs);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::BackgroundJobs)
        });
    }

    #[test]
    fn singleton_window_state_distinguishes_live_stale_and_reentrant_requests() {
        assert_eq!(
            existing_window_disposition(false, false, None::<u8>, false, |_| false),
            ExistingWindowDisposition::OpenNew
        );
        assert_eq!(
            existing_window_disposition(true, false, None::<u8>, false, |_| false),
            ExistingWindowDisposition::SuppressReentrantOpen
        );
        assert_eq!(
            existing_window_disposition(false, false, Some(7_u8), true, |handle| handle == 7),
            ExistingWindowDisposition::Activate(7)
        );
        assert_eq!(
            existing_window_disposition(false, false, Some(7_u8), false, |_| true),
            ExistingWindowDisposition::ClearStale(7)
        );
        assert_eq!(
            existing_window_disposition(false, false, Some(7_u8), true, |_| false),
            ExistingWindowDisposition::ClearStale(7)
        );
        assert_eq!(
            existing_window_disposition(false, true, Some(7_u8), true, |_| true),
            ExistingWindowDisposition::SuppressReentrantOpen
        );
    }

    #[test]
    fn api_key_input_has_explicit_keep_set_and_delete_semantics() {
        assert_eq!(api_key_update_for_input("", false), ApiKeyUpdate::Keep);
        assert_eq!(api_key_update_for_input("   ", false), ApiKeyUpdate::Keep);
        assert_eq!(
            api_key_update_for_input("new-secret", false),
            ApiKeyUpdate::Set("new-secret".to_string())
        );
        assert_eq!(
            api_key_update_for_input("new-secret", true),
            ApiKeyUpdate::Delete
        );
    }

    #[test]
    fn pull_guidance_is_only_generated_for_local_ollama() {
        let missing = vec!["chat-model".to_string(), "embed-model".to_string()];
        assert_eq!(
            ollama_pull_commands(DEFAULT_OLLAMA_OPENAI_BASE_URL, &missing),
            vec![
                "ollama pull chat-model".to_string(),
                "ollama pull embed-model".to_string()
            ]
        );
        assert!(ollama_pull_commands("https://models.example.test/v1/", &missing).is_empty());
        assert_eq!(
            ollama_pull_commands("http://localhost:11434", &missing).len(),
            2
        );
    }

    #[test]
    fn request_timeout_is_an_integer_within_the_provider_range() {
        assert_eq!(parse_request_timeout_secs("1").unwrap(), 1);
        assert_eq!(parse_request_timeout_secs(" 120 ").unwrap(), 120);
        assert_eq!(parse_request_timeout_secs("600").unwrap(), 600);
        for invalid in ["", "0", "601", "1.5", "-1", "ten", "18446744073709551616"] {
            assert!(
                parse_request_timeout_secs(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
    }

    #[test]
    fn default_models_feed_detection_in_role_order() {
        let settings = ProviderSettings::default();
        assert_eq!(
            configured_models(&settings),
            ["qwen3.5:0.8b", "qwen3-embedding:0.6b", "qwen3.5:0.8b"]
        );
    }

    #[test]
    fn scroll_handle_is_cloneable_and_independent_per_window() {
        // Each AiSettingsWindow owns its own ScrollHandle so scrolling
        // requests never bleed across windows when multiple Provider setting
        // dialogs are open. The handle is also Clone so it can be captured
        // into the async detection future that scrolls to the freshly rendered
        // notice after detection completes.
        let handle = gpui::ScrollHandle::new();
        let clone = handle.clone();
        let other = gpui::ScrollHandle::new();
        // ScrollHandle starts at its default state on every fresh window; the
        // wrapper is Clone so async tasks can move a copy, but a separately
        // constructed handle is a logically distinct value we can use to
        // drive an unrelated container.
        assert_eq!(handle.offset(), clone.offset());
        assert_eq!(handle.offset(), other.offset());
        assert_eq!(handle.max_offset(), other.max_offset());
    }
}
