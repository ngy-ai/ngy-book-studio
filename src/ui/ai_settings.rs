use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui_component::{Selectable as _, checkbox::Checkbox};
use ngy_book_studio::{
    ai::{
        ChatGenerationSettings, DEFAULT_AI_REQUEST_TIMEOUT_SECS, DEFAULT_CHAT_OUTPUT_TOKENS,
        DEFAULT_OLLAMA_OPENAI_BASE_URL, MAX_AI_REQUEST_TIMEOUT_SECS, MAX_EMBEDDING_DIMENSIONS,
        MIN_AI_REQUEST_TIMEOUT_SECS, ModelInfo, normalize_provider_base_url,
    },
    services::{
        ApiKeyUpdate, AppServices, DEFAULT_BACKGROUND_JOB_CONCURRENCY,
        DEFAULT_BACKGROUND_JOB_INTERVAL_MS, DEFAULT_CHAT_MODEL, DEFAULT_EMBEDDING_DIMENSIONS,
        DEFAULT_EMBEDDING_MODEL, DEFAULT_VISION_MODEL, EndpointRoutingSettings, EndpointSettings,
        MAX_BACKGROUND_JOB_CONCURRENCY, MAX_BACKGROUND_JOB_INTERVAL_MS,
        MIN_BACKGROUND_JOB_CONCURRENCY, ModelRole, ProviderSettings, TRANSLATION_LANGUAGES,
        TranslationDisplayMode, translation_language_label,
    },
};
use ngy_book_studio::{logging, startup};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    Endpoint,
    Models,
    WebSearch,
    BackgroundJobs,
    /// Application-level preferences that are not provider configuration. The
    /// variant must stay last: `active_tab as usize` indexes both the tab bar
    /// and the per-tab scroll handles.
    System,
}

impl SettingsTab {
    const ALL: [Self; 5] = [
        Self::Endpoint,
        Self::Models,
        Self::WebSearch,
        Self::BackgroundJobs,
        Self::System,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint",
            Self::Models => "对话模型",
            Self::WebSearch => "联网搜索",
            Self::BackgroundJobs => "后台任务",
            Self::System => "系统配置",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::Endpoint => "ai-settings-tab-endpoint",
            Self::Models => "ai-settings-tab-models",
            Self::WebSearch => "ai-settings-tab-web-search",
            Self::BackgroundJobs => "ai-settings-tab-background-jobs",
            Self::System => "ai-settings-tab-system",
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

/// 一次待提交的数据目录更改：新位置、引导配置文件、当前（旧）数据目录。
#[derive(Clone)]
struct DataDirChange {
    target: PathBuf,
    config_path: PathBuf,
    from: PathBuf,
}

struct EndpointDraft {
    id: String,
    name_input: Entity<InputState>,
    base_url_input: Entity<InputState>,
    request_timeout_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    remote_content_confirmed: bool,
    allow_insecure_remote_http: bool,
    confirmed_remote_endpoint: String,
    delete_api_key: bool,
    observed_url: String,
    detected_models: Option<Vec<String>>,
    discovery_generation: u64,
    api_key_subscription: Subscription,
    _subscriptions: Vec<Subscription>,
}

impl EndpointDraft {
    fn invalidate_models(&mut self) {
        self.detected_models = None;
        self.discovery_generation = self.discovery_generation.wrapping_add(1);
    }

    fn entered(&self, cx: &App) -> Result<EndpointSettings> {
        Ok(EndpointSettings {
            id: self.id.clone(),
            name: self.name_input.read(cx).value().trim().to_string(),
            base_url: self.base_url_input.read(cx).value().trim().to_string(),
            request_timeout_secs: parse_request_timeout_secs(
                self.request_timeout_input.read(cx).value().as_ref(),
            )?,
            remote_content_confirmed: self.remote_content_confirmed,
            allow_insecure_remote_http: self.allow_insecure_remote_http,
            confirmed_remote_endpoint: self.confirmed_remote_endpoint.clone(),
        })
    }

    fn api_key_update(&self, cx: &App) -> ApiKeyUpdate {
        api_key_update_for_input(
            self.api_key_input.read(cx).value().as_ref(),
            self.delete_api_key,
        )
    }
}

/// Provider settings deliberately live in their own native window. It owns no
/// model client and performs no network or credential work on the GPUI thread.
pub(super) struct AiSettingsWindow {
    services: Arc<AppServices>,
    endpoints: Vec<EndpointDraft>,
    selected_endpoint_id: String,
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
    active_tab: SettingsTab,
    scroll_handles: [gpui::ScrollHandle; 5],
    window_handle: gpui::AnyWindowHandle,
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
    /// How many background model jobs may run at the same time.
    background_job_concurrency_input: Entity<InputState>,
    /// Milliseconds one worker pauses after finishing a job.
    background_job_interval_input: Entity<InputState>,
    /// Global PDF reading preference: no gap between pages when enabled.
    pdf_compact_reading: bool,
    /// Target language for reading-time book translation. `None` disables it.
    default_language: Option<String>,
    /// How reading-time translations are displayed relative to the original text.
    translation_display_mode: TranslationDisplayMode,
    /// Whether AI and web-search requests go through the system proxy.
    use_proxy: bool,
    operation: PendingOperation,
    /// 目录选择对话框正开着（或更改正在提交）。原生对话框在自己的消息循环里跑，
    /// 用它挡住重复点击。
    choosing_data_directory: bool,
    /// 选中的新目录里已经有一个图书库：等用户确认是否覆盖。
    confirm_overwrite: Option<DataDirChange>,
    notice: Option<SettingsNotice>,
}

impl AiSettingsWindow {
    fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let endpoints = settings
            .endpoints()
            .into_iter()
            .map(|endpoint| Self::new_endpoint_draft(endpoint, window, cx))
            .collect();
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
        let embedding_dimensions_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.embedding_dimensions.to_string())
                .placeholder(DEFAULT_EMBEDDING_DIMENSIONS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let vision_model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.vision_model)
                .placeholder(DEFAULT_VISION_MODEL)
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
                    ngy_book_studio::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS.to_string(),
                )
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let web_search_max_results_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_max_results.to_string())
                .placeholder(ngy_book_studio::web_search::MAX_WEB_SEARCH_RESULTS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let background_job_concurrency_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.background_job_concurrency.to_string())
                .placeholder(DEFAULT_BACKGROUND_JOB_CONCURRENCY.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let background_job_interval_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.background_job_interval_ms.to_string())
                .placeholder(DEFAULT_BACKGROUND_JOB_INTERVAL_MS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        Self {
            services,
            endpoints,
            selected_endpoint_id: "default".into(),
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
            active_tab: SettingsTab::default(),
            scroll_handles: std::array::from_fn(|_| gpui::ScrollHandle::new()),
            window_handle: gpui::Window::window_handle(window),
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
            background_job_concurrency_input,
            background_job_interval_input,
            pdf_compact_reading: settings.pdf_compact_reading,
            default_language: settings.default_language.clone(),
            translation_display_mode: settings.translation_display_mode,
            use_proxy: settings.use_proxy,
            operation: PendingOperation::Idle,
            choosing_data_directory: false,
            confirm_overwrite: None,
            notice: None,
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
        let mut endpoints = self
            .endpoints
            .iter()
            .map(|endpoint| endpoint.entered(cx))
            .collect::<Result<Vec<_>>>()?;
        let default_index = endpoints
            .iter()
            .position(|endpoint| endpoint.id == "default")
            .context("缺少默认 Endpoint")?;
        let default_endpoint = endpoints.remove(default_index);
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
            base_url: default_endpoint.base_url,
            endpoint_routing: EndpointRoutingSettings {
                default_endpoint_name: default_endpoint.name,
                additional_endpoints: endpoints,
                chat_endpoint_id: self.chat_endpoint_id.clone(),
                embedding_endpoint_id: self.embedding_endpoint_id.clone(),
                vision_endpoint_id: self.vision_endpoint_id.clone(),
            },
            chat_model: self.chat_model_input.read(cx).value().trim().to_string(),
            chat_generation,
            embedding_model: self
                .embedding_model_input
                .read(cx)
                .value()
                .trim()
                .to_string(),
            embedding_dimensions: parse_embedding_dimensions(
                self.embedding_dimensions_input.read(cx).value().as_ref(),
            )?,
            vision_model: self.vision_model_input.read(cx).value().trim().to_string(),
            remote_content_confirmed: default_endpoint.remote_content_confirmed,
            allow_insecure_remote_http: default_endpoint.allow_insecure_remote_http,
            confirmed_remote_endpoint: default_endpoint.confirmed_remote_endpoint,
            request_timeout_secs: default_endpoint.request_timeout_secs,
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
            background_job_concurrency: parse_background_job_concurrency(
                self.background_job_concurrency_input
                    .read(cx)
                    .value()
                    .as_ref(),
            )?,
            background_job_interval_ms: parse_background_job_interval_ms(
                self.background_job_interval_input.read(cx).value().as_ref(),
            )?,
            pdf_compact_reading: self.pdf_compact_reading,
            default_language: self.default_language.clone(),
            translation_display_mode: self.translation_display_mode,
            use_proxy: self.use_proxy,
        };
        settings.validate()?;
        Ok(settings)
    }

    fn set_default_language(&mut self, language: Option<String>, cx: &mut Context<Self>) {
        self.default_language = language;
        cx.notify();
    }

    fn set_translation_display_mode(
        &mut self,
        mode: TranslationDisplayMode,
        cx: &mut Context<Self>,
    ) {
        self.translation_display_mode = mode;
        cx.notify();
    }

    fn new_endpoint_draft(
        endpoint: EndpointSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EndpointDraft {
        let observed_url = endpoint_draft_identity(&endpoint.base_url);
        let name_input = cx.new(|cx| InputState::new(window, cx).default_value(endpoint.name));
        let base_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(endpoint.base_url)
                .placeholder(DEFAULT_OLLAMA_OPENAI_BASE_URL)
        });
        let request_timeout_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(endpoint.request_timeout_secs.to_string())
                .placeholder(DEFAULT_AI_REQUEST_TIMEOUT_SECS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let endpoint_id = endpoint.id.clone();
        let url_subscription = cx.subscribe_in(
            &base_url_input,
            window,
            move |this, input, event, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let canonical = endpoint_draft_identity(input.read(cx).value().as_ref());
                if let Some(endpoint) = this.endpoints.iter_mut().find(|e| e.id == endpoint_id)
                    && canonical != endpoint.observed_url
                {
                    endpoint.observed_url = canonical;
                    endpoint.remote_content_confirmed = false;
                    endpoint.allow_insecure_remote_http = false;
                    endpoint.confirmed_remote_endpoint.clear();
                    endpoint.delete_api_key = false;
                    // A different server must never inherit a key through input undo history.
                    Self::reset_endpoint_key(endpoint, window, cx);
                    endpoint.invalidate_models();
                    this.notice = None;
                    cx.notify();
                }
            },
        );
        let name_subscription = cx.subscribe(&name_input, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let api_key_input = Self::new_api_key_input(window, cx);
        let api_key_subscription = Self::subscribe_endpoint_key(&api_key_input, &endpoint.id, cx);
        EndpointDraft {
            id: endpoint.id,
            name_input,
            base_url_input,
            request_timeout_input,
            api_key_input,
            remote_content_confirmed: endpoint.remote_content_confirmed,
            allow_insecure_remote_http: endpoint.allow_insecure_remote_http,
            confirmed_remote_endpoint: endpoint.confirmed_remote_endpoint,
            delete_api_key: false,
            observed_url,
            detected_models: None,
            discovery_generation: 0,
            api_key_subscription,
            _subscriptions: vec![url_subscription, name_subscription],
        }
    }

    fn new_api_key_input(window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则保持当前 Endpoint 的现有密钥")
        })
    }

    fn subscribe_endpoint_key(
        input: &Entity<InputState>,
        endpoint_id: &str,
        cx: &mut Context<Self>,
    ) -> Subscription {
        let endpoint_id = endpoint_id.to_string();
        cx.subscribe(input, move |this, _, event, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(endpoint) = this
                    .endpoints
                    .iter_mut()
                    .find(|endpoint| endpoint.id == endpoint_id)
            {
                endpoint.invalidate_models();
                this.notice = None;
                cx.notify();
            }
        })
    }

    fn reset_endpoint_key(
        endpoint: &mut EndpointDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        endpoint.api_key_input = Self::new_api_key_input(window, cx);
        endpoint.api_key_subscription =
            Self::subscribe_endpoint_key(&endpoint.api_key_input, &endpoint.id, cx);
    }

    fn set_delete_endpoint_key(
        &mut self,
        endpoint_id: &str,
        delete: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.id == endpoint_id)
        {
            endpoint.delete_api_key = delete;
            endpoint.invalidate_models();
            if delete {
                Self::reset_endpoint_key(endpoint, window, cx);
            }
            self.notice = None;
            cx.notify();
        }
    }

    fn selected_endpoint(&self) -> &EndpointDraft {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.id == self.selected_endpoint_id)
            .expect("the selected endpoint remains in the draft registry")
    }

    fn select_endpoint(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() || !self.endpoints.iter().any(|endpoint| endpoint.id == id) {
            return;
        }
        window.blur();
        self.selected_endpoint_id = id.to_string();
        self.notice = None;
        cx.notify();
    }

    fn add_endpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() {
            return;
        }
        let mut bytes = [0_u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            self.notice = Some(SettingsNotice {
                text: "无法生成 Endpoint 标识，请重试。".into(),
                error: true,
            });
            cx.notify();
            return;
        }
        let id = format!(
            "endpoint-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let endpoint = EndpointSettings {
            id: id.clone(),
            name: format!("Endpoint {}", self.endpoints.len() + 1),
            base_url: String::new(),
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: String::new(),
            request_timeout_secs: DEFAULT_AI_REQUEST_TIMEOUT_SECS,
        };
        self.endpoints
            .push(Self::new_endpoint_draft(endpoint, window, cx));
        self.select_endpoint(&id, window, cx);
    }

    fn endpoint_is_bound(&self, id: &str) -> bool {
        [
            &self.chat_endpoint_id,
            &self.embedding_endpoint_id,
            &self.vision_endpoint_id,
        ]
        .into_iter()
        .any(|bound| bound == id)
    }

    fn delete_selected_endpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() || self.selected_endpoint_id == "default" {
            return;
        }
        if self.endpoint_is_bound(&self.selected_endpoint_id) {
            self.notice = Some(SettingsNotice {
                text: "这个 Endpoint 仍被模型使用，请先在“对话模型”中重新选择对应 Endpoint。"
                    .into(),
                error: true,
            });
            cx.notify();
            return;
        }
        self.endpoints
            .retain(|endpoint| endpoint.id != self.selected_endpoint_id);
        self.select_endpoint("default", window, cx);
    }

    fn endpoint_id_for(&self, role: ModelRole) -> &str {
        match role {
            ModelRole::Chat => &self.chat_endpoint_id,
            ModelRole::Embedding => &self.embedding_endpoint_id,
            ModelRole::Vision => &self.vision_endpoint_id,
        }
    }

    fn assign_endpoint(&mut self, role: ModelRole, id: &str, cx: &mut Context<Self>) {
        if self.operation.busy() || !self.endpoints.iter().any(|endpoint| endpoint.id == id) {
            return;
        }
        match role {
            ModelRole::Chat => self.chat_endpoint_id = id.to_string(),
            ModelRole::Embedding => self.embedding_endpoint_id = id.to_string(),
            ModelRole::Vision => self.vision_endpoint_id = id.to_string(),
        }
        self.notice = None;
        cx.notify();
    }

    fn model_input_for(&self, role: ModelRole) -> &Entity<InputState> {
        match role {
            ModelRole::Chat => &self.chat_model_input,
            ModelRole::Embedding => &self.embedding_model_input,
            ModelRole::Vision => &self.vision_model_input,
        }
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

    fn detect_models(&mut self, endpoint_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.busy() {
            return;
        }
        let Some(endpoint_index) = self
            .endpoints
            .iter()
            .position(|endpoint| endpoint.id == endpoint_id)
        else {
            return;
        };
        self.endpoints[endpoint_index].invalidate_models();
        let generation = self.endpoints[endpoint_index].discovery_generation;
        let draft = &self.endpoints[endpoint_index];
        let endpoint = match draft.entered(cx).and_then(|endpoint| {
            endpoint.validate()?;
            Ok(endpoint)
        }) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.notice = Some(SettingsNotice {
                    text: format!("AI Provider 设置无效：{error:#}"),
                    error: true,
                });
                cx.notify();
                return;
            }
        };
        let key_update = draft.api_key_update(cx);
        self.operation = PendingOperation::Detecting;
        self.notice = Some(SettingsNotice {
            text: format!("正在读取 {} 的模型…", endpoint.name),
            error: false,
        });

        let services = Arc::clone(&self.services);
        cx.spawn_in(window, async move |view, cx| {
            // Credential access and HTTP I/O are dispatched by AppServices to
            // its Tokio runtime. Keeping the Arc in this GPUI task prevents
            // AppServices from ever being last-dropped by its own worker.
            let outcome = services
                .probe_endpoint_models(endpoint.clone(), key_update)
                .await;
            let _ = view.update(cx, |this, cx| {
                this.operation = PendingOperation::Idle;
                if !this.detection_matches(&endpoint, generation, cx) {
                    this.notice = None;
                    cx.notify();
                    return;
                }
                match outcome {
                    Ok(models) => this.apply_detected_models(&endpoint, models),
                    Err(error) => {
                        this.notice = Some(SettingsNotice {
                            text: format!(
                                "无法读取 {} 的模型：{error:#}。若使用本地 Ollama，请确认服务已运行。", endpoint.name
                            ),
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

    fn detection_matches(&self, endpoint: &EndpointSettings, generation: u64, cx: &App) -> bool {
        self.endpoints.iter().any(|draft| {
            draft.id == endpoint.id
                && draft.discovery_generation == generation
                && canonical_endpoint(draft.base_url_input.read(cx).value().as_ref())
                    == canonical_endpoint(&endpoint.base_url)
        })
    }

    fn apply_detected_models(&mut self, endpoint: &EndpointSettings, models: Vec<ModelInfo>) {
        let detected_models: Vec<_> = models.into_iter().map(|model| model.id).collect();
        let count = detected_models.len();
        if let Some(draft) = self
            .endpoints
            .iter_mut()
            .find(|draft| draft.id == endpoint.id)
        {
            draft.detected_models = Some(detected_models);
        }
        self.notice = Some(SettingsNotice {
            text: format!(
                "{} 连接成功，返回 {count} 个模型；可在各模型区域查看或选择。",
                endpoint.name
            ),
            error: false,
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
        // When the embedding model or dimensions change, all existing vector
        // indices become incompatible. Warn the user before proceeding.
        let embedding_confirmation = match self.services.provider_settings() {
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
                Some(answer)
            }
            _ => None,
        };
        let key_updates: BTreeMap<_, _> = self
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.id.clone(), endpoint.api_key_update(cx)))
            .collect();
        let web_key_update = self.web_api_key_update(cx);
        self.operation = PendingOperation::Saving;
        self.notice = Some(SettingsNotice {
            text: "正在安全保存 Provider 设置…".to_string(),
            error: false,
        });

        // The reading preference applies to open readers as soon as the save
        // commits, so it is captured before the settings move into the future.
        let pdf_compact_reading = settings.pdf_compact_reading;
        let services = Arc::clone(&self.services);
        let window_handle = self.window_handle;
        cx.spawn_in(window, async move |view, cx| {
            if let Some(confirmation) = embedding_confirmation {
                let confirmed = matches!(confirmation.await, Ok(0));
                if !confirmed {
                    let _ = view.update(cx, |this, cx| {
                        this.operation = PendingOperation::Idle;
                        this.notice = Some(SettingsNotice {
                            text: "已取消保存，Embedding 设置未修改。".to_string(),
                            error: false,
                        });
                        cx.notify();
                    });
                    return;
                }
            }
            // AppServices moves credential and SQLite work to its dedicated
            // runtime; this UI future only awaits and applies the result.
            let outcome = services.configure_providers(settings, key_updates).await;
            let outcome = outcome.and_then(|()| match &web_key_update {
                ApiKeyUpdate::Keep => Ok(()),
                ApiKeyUpdate::Set(value) => services.set_web_search_api_key(value),
                ApiKeyUpdate::Delete => services.delete_web_search_api_key(),
            });
            let _ = view.update(cx, |this, cx| {
                this.operation = PendingOperation::Idle;
                match outcome {
                    Ok(()) => {
                        this.notice = Some(SettingsNotice {
                            text: "Provider 设置已保存；新的搜索与问答请求会使用这些设置。"
                                .to_string(),
                            error: false,
                        });
                        // Saved settings now reach documents that are already
                        // open; the write above is the source of truth.
                        apply_pdf_compact_reading(pdf_compact_reading, cx);
                        apply_translation_display_mode(cx);
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

    fn render_model_detection(
        &self,
        endpoint: &EndpointDraft,
        role: Option<ModelRole>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let models = endpoint.detected_models.as_ref()?;
        let missing_models = role
            .into_iter()
            .map(|role| {
                self.model_input_for(role)
                    .read(cx)
                    .value()
                    .trim()
                    .to_string()
            })
            .filter(|model| !models.contains(model))
            .collect::<Vec<_>>();
        let commands = ollama_pull_commands(
            endpoint.base_url_input.read(cx).value().as_ref(),
            &missing_models,
        );
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
                            "{} 返回的模型（{}）",
                            endpoint.name_input.read(cx).value(),
                            models.len()
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(MUTED))
                        .child(if models.is_empty() {
                            "未返回模型".to_string()
                        } else {
                            "模型列表不标注能力，请选择支持当前用途的模型。".to_string()
                        }),
                )
                .when(!missing_models.is_empty(), |this| {
                    this.child(div().text_xs().text_color(rgb(0x9f302c)).child(format!(
                        "当前模型未在此 Endpoint 的列表中：{}",
                        missing_models.join("、")
                    )))
                })
                .child(
                    div()
                        .h_flex()
                        .flex_wrap()
                        .gap_2()
                        .children(models.iter().enumerate().map(|(index, model)| {
                            let model = model.clone();
                            let selected_role = role;
                            Button::new(("ai-detected-model", index))
                                .small()
                                .label(model.clone())
                                .disabled(self.operation.busy() || role.is_none())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(role) = selected_role {
                                        this.model_input_for(role).update(cx, |input, cx| {
                                            input.set_value(model.clone(), window, cx);
                                        });
                                        cx.notify();
                                    }
                                }))
                        })),
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
        let endpoint = self.selected_endpoint();
        let remote_view = cx.entity();
        let insecure_view = cx.entity();
        let endpoint_for_confirmation = endpoint.base_url_input.clone();
        let remote_endpoint_id = endpoint.id.clone();
        let insecure_endpoint_id = endpoint.id.clone();
        let busy = self.operation.busy();

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(self.render_endpoint_list(cx))
            .child(self.render_input_field(
                "Endpoint 名称",
                "用于区分连接；各模型按此名称选择 Endpoint。",
                &endpoint.name_input,
            ))
            .child(self.render_input_field(
                "Endpoint",
                "默认使用本机 Ollama 的 http://127.0.0.1:11434/v1/；不会静默切换到云端。",
                &endpoint.base_url_input,
            ))
            .child(
                Checkbox::new("ai-confirm-remote")
                    .checked(endpoint.remote_content_confirmed)
                    .disabled(busy)
                    .label("确认允许把图书内容发送到远程 endpoint")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        remote_view.update(cx, |this, cx| {
                            let Some(endpoint) = this
                                .endpoints
                                .iter_mut()
                                .find(|endpoint| endpoint.id == remote_endpoint_id)
                            else {
                                return;
                            };
                            if checked {
                                let entered = endpoint_for_confirmation.read(cx).value();
                                match normalize_provider_base_url(entered.trim()) {
                                    Ok(url) => {
                                        endpoint.remote_content_confirmed = true;
                                        endpoint.confirmed_remote_endpoint = url.to_string();
                                    }
                                    Err(error) => {
                                        endpoint.remote_content_confirmed = false;
                                        endpoint.confirmed_remote_endpoint.clear();
                                        this.notice = Some(SettingsNotice {
                                            text: format!("Endpoint 无效，无法确认：{error:#}"),
                                            error: true,
                                        });
                                    }
                                }
                            } else {
                                endpoint.remote_content_confirmed = false;
                                endpoint.allow_insecure_remote_http = false;
                                endpoint.confirmed_remote_endpoint.clear();
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(
                Checkbox::new("ai-allow-insecure-http")
                    .checked(endpoint.allow_insecure_remote_http)
                    .disabled(busy || !endpoint.remote_content_confirmed)
                    .label("额外允许非 HTTPS 的远程 endpoint（内容可能被窃听）")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        insecure_view.update(cx, |this, cx| {
                            if let Some(endpoint) = this
                                .endpoints
                                .iter_mut()
                                .find(|endpoint| endpoint.id == insecure_endpoint_id)
                            {
                                endpoint.allow_insecure_remote_http = checked;
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(self.render_input_field(
                "请求超时（秒）",
                "可设置 1–600 秒，默认 120 秒。非流式请求按整段调用计时；流式回答按“多久没有新数据”计时，只要模型持续输出就不会被中途中断。",
                &endpoint.request_timeout_input,
            ))
            .when_some(
                self.render_model_detection(endpoint, None, cx),
                |this, detection| this.child(detection),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_endpoint_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div().v_flex().gap_2()
            .child(div().h_flex().flex_wrap().gap_2().children(
                self.endpoints.iter().enumerate().map(|(index, endpoint)| {
                    let id = endpoint.id.clone();
                    Button::new(("ai-endpoint-choice", index))
                        .label(endpoint.name_input.read(cx).value())
                        .selected(self.selected_endpoint_id == endpoint.id)
                        .disabled(self.operation.busy())
                        .debug_selector(move || format!("ai-endpoint-choice-{index}"))
                        .on_click(cx.listener(move |this, _, window, cx| this.select_endpoint(&id, window, cx)))
                }),
            ))
            .child(div().h_flex().gap_2()
                .child(Button::new("ai-add-endpoint").label("新增 Endpoint")
                    .disabled(self.operation.busy())
                    .debug_selector(|| "ai-add-endpoint".into())
                    .on_click(cx.listener(|this, _, window, cx| this.add_endpoint(window, cx))))
                .child(Button::new("ai-delete-endpoint").label("删除 Endpoint")
                    .disabled(self.operation.busy() || self.selected_endpoint_id == "default")
                    .debug_selector(|| "ai-delete-endpoint".into())
                    .on_click(cx.listener(|this, _, window, cx| this.delete_selected_endpoint(window, cx)))))
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "每个 Endpoint 独立保存连接、密钥和远程授权。模型使用中的 Endpoint 需先重新分配才能删除。"))
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
            .child(self.render_role_panel(ModelRole::Chat, cx))
            .child(self.render_role_panel(ModelRole::Embedding, cx))
            .child(self.render_role_panel(ModelRole::Vision, cx))
            .child(self.render_generation_panel(cx))
            .into_any_element()
    }

    #[inline(never)]
    fn render_role_panel(&self, role: ModelRole, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        div()
            .id(SharedString::from(format!("ai-role-{selector}")))
            .v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .child(format!("{label} · Endpoint")),
            )
            .child(div().h_flex().flex_wrap().gap_2().children(
                self.endpoints.iter().enumerate().map(|(index, endpoint)| {
                    let id = endpoint.id.clone();
                    Button::new(("ai-role-endpoint", index))
                        .label(endpoint.name_input.read(cx).value())
                        .selected(endpoint_id == endpoint.id)
                        .disabled(self.operation.busy())
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
                        .text_color(rgb(MUTED))
                        .child(endpoint.base_url_input.read(cx).value()),
                )
            })
            .child(self.render_input_field(label, description, self.model_input_for(role)))
            .when(role == ModelRole::Embedding, |this| {
                this.child(self.render_input_field(
                    "向量维度（Dimensions）",
                    "正整数，默认 1024。修改后所有已索引的向量将失效并需要重新生成。",
                    &self.embedding_dimensions_input,
                ))
            })
            .child(
                Button::new("ai-role-detect-models")
                    .label("检测此 Endpoint 的模型")
                    .disabled(self.operation.busy())
                    .debug_selector(move || format!("ai-detect-{selector}-models"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.detect_models(&endpoint_id, window, cx);
                    })),
            )
            .when_some(
                endpoint.and_then(|endpoint| self.render_model_detection(endpoint, Some(role), cx)),
                |this, detection| this.child(detection),
            )
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
        let endpoint = self.selected_endpoint();
        let endpoint_id = endpoint.id.clone();
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
                Input::new(&endpoint.api_key_input)
                    .mask_toggle()
                    .disabled(busy || endpoint.delete_api_key),
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
                    .checked(endpoint.delete_api_key)
                    .disabled(busy)
                    .label("删除这个 endpoint 已保存的 API key")
                    .on_click(move |checked, window, cx| {
                        let checked = *checked;
                        delete_view.update(cx, |this, cx| {
                            this.set_delete_endpoint_key(&endpoint_id, checked, window, cx);
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
                                match ngy_book_studio::web_search::normalize_web_endpoint(
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
            .gap_5()
            .child(
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
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "ai-background-job-scheduling".into())
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
                            .child("后台任务调度"),
                    )
                    .child(self.render_input_field(
                        "任务并发",
                        "同时运行的模型任务数量，1–1024，默认 1。并发越高占用的内存与网络越多；配置较低的机器建议保持 1。",
                        &self.background_job_concurrency_input,
                    ))
                    .child(self.render_input_field(
                        "任务间隔（毫秒）",
                        "一个任务完成后，休眠多久再开始下一个任务，0–60000 毫秒，默认 10。机器配置较差时增大该值可降低持续满载的风险。",
                        &self.background_job_interval_input,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .line_height(gpui::relative(1.5))
                            .text_color(rgb(MUTED))
                            .child(
                                "保存后对后续任务立即生效，不需要重启；正在运行的任务不会被打断。",
                            ),
                    ),
            )
            .into_any_element()
    }

    /// 更改数据目录：选新位置、校验、记录，然后重启墨页完成搬迁。
    ///
    /// 运行中的图书库、对象存储和日志都绑在旧目录上，就地搬走会让当前进程立刻失效，
    /// 所以这里只记录选择：真正的搬迁由重启后的新进程在打开图书库**之前**完成
    /// （`startup::complete_pending_move`）。重启不是可选项 —— 不重启，本次会话会继续
    /// 往旧目录里写新导入的书。
    fn change_data_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choosing_data_directory {
            return;
        }
        let config_path = match startup::bootstrap_path() {
            Ok(path) => path,
            Err(error) => {
                self.notice = Some(SettingsNotice {
                    text: format!("无法定位目录设置文件：{error:#}"),
                    error: true,
                });
                cx.notify();
                return;
            }
        };
        let from = self.services.data_dir().to_path_buf();
        // 原生对话框在本次实体更新返回之后再显示：Windows 文件对话框跑自己的消息
        // 循环，在 GPUI 持有 App 借用时弹出会让随后每一帧都 BorrowMutError。
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title("选择数据目录")
            .set_location(&startup::shell_dialog_directory(&from))
            .open_single_dir();
        self.choosing_data_directory = true;
        self.confirm_overwrite = None;
        cx.spawn_in(window, async move |view, cx| {
            let picked = dialog.show();
            let _ = view.update(cx, |this, cx| {
                this.choosing_data_directory = false;
                match picked {
                    Ok(Some(target)) => this.prepare_data_directory_change(
                        DataDirChange {
                            target,
                            config_path,
                            from,
                        },
                        cx,
                    ),
                    // 取消：保留原来的提示，不假装改过。
                    Ok(None) => cx.notify(),
                    Err(error) => {
                        this.notice = Some(SettingsNotice {
                            text: format!("无法打开目录选择器：{error}"),
                            error: true,
                        });
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// 校验选中的新目录：空目录直接提交，已经是一个图书库时先要一次覆盖确认。
    fn prepare_data_directory_change(&mut self, change: DataDirChange, cx: &mut Context<Self>) {
        let prepared = cx.background_executor().spawn({
            let target = change.target.clone();
            // 建目录、探针写删都在后台：用户可能选到网络盘。
            async move {
                startup::ensure_data_dir_usable(&target)?;
                Ok::<_, anyhow::Error>(startup::holds_a_library(&target))
            }
        });
        cx.spawn(async move |view, cx| {
            let outcome = prepared.await;
            let _ = view.update(cx, |this, cx| match outcome {
                // 目标里已经有一个图书库：覆盖不可逆，先问清楚。
                Ok(true) => {
                    this.confirm_overwrite = Some(change);
                    this.notice = None;
                    cx.notify();
                }
                Ok(false) => this.commit_data_directory_change(change, false, cx),
                Err(error) => {
                    this.notice = Some(SettingsNotice {
                        text: format!("无法使用该目录：{error:#}"),
                        error: true,
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 写下引导配置并重启墨页；搬迁交给重启后的新进程。
    fn commit_data_directory_change(
        &mut self,
        change: DataDirChange,
        overwrite: bool,
        cx: &mut Context<Self>,
    ) {
        let applied = cx.background_executor().spawn({
            let (config_path, target, from) = (
                change.config_path.clone(),
                change.target.clone(),
                change.from.clone(),
            );
            async move { startup::apply_data_dir_change(&config_path, &target, &from, overwrite) }
        });
        self.choosing_data_directory = true;
        self.confirm_overwrite = None;
        self.notice = Some(SettingsNotice {
            text: format!(
                "正在把数据目录切换到 {}，随后墨页会重新启动并把原目录的内容搬过去…",
                change.target.display()
            ),
            error: false,
        });
        cx.notify();

        cx.spawn(async move |view, cx| {
            let outcome = applied.await;
            let _ = view.update(cx, |this, cx| {
                this.choosing_data_directory = false;
                match outcome {
                    // 记录已经落盘：重启后新进程会把旧目录的内容搬过来。
                    Ok(_) => this.restart_application(&change.target, cx),
                    Err(error) => {
                        this.notice = Some(SettingsNotice {
                            text: format!("无法使用该目录：{error:#}"),
                            error: true,
                        });
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 重启墨页。搬迁和切换数据目录都要重新打开图书库，只能靠重启。
    fn restart_application(&mut self, data_dir: &Path, cx: &mut Context<Self>) {
        let spawned = std::env::current_exe()
            .map_err(|error| error.to_string())
            .and_then(|executable| {
                std::process::Command::new(&executable)
                    .spawn()
                    .map(|_| ())
                    .map_err(|error| format!("{}：{error}", executable.display()))
            });
        match spawned {
            Ok(()) => cx.quit(),
            Err(error) => {
                self.notice = Some(SettingsNotice {
                    text: format!(
                        "已把数据目录记为 {}。请手动关闭墨页再重新打开：新目录会在下次启动时\
                         启用，原目录的内容也在那时搬过去。\n\n自动重启失败：{error}",
                        data_dir.display()
                    ),
                    error: true,
                });
                cx.notify();
            }
        }
    }

    /// 用系统默认程序打开目录，出问题只提示不打断。
    fn open_directory(&mut self, path: PathBuf, label: &str, cx: &mut Context<Self>) {
        #[cfg(target_os = "windows")]
        {
            if let Err(error) = open::that_detached(&path) {
                self.notice = Some(SettingsNotice {
                    text: format!("无法打开{label}：{error}"),
                    error: true,
                });
                cx.notify();
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (path, label);
            self.notice = Some(SettingsNotice {
                text: format!("当前平台不支持在文件管理器中打开{label}。"),
                error: true,
            });
            cx.notify();
        }
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
            SettingsTab::System => self.render_system_panel(cx),
        }
    }

    #[inline(never)]
    fn render_system_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let compact_view = cx.entity();
        let language_view = cx.entity();
        let display_mode_view = cx.entity();
        let use_proxy_view = cx.entity();
        // 图书库自己记的是标准化路径（Windows 上是 `\\?\C:\…`），菜单、资源管理器和
        // 对话框都更认常规写法，所以显示与打开都用转换后的路径。
        let data_dir = startup::shell_dialog_directory(self.services.data_dir());
        let log_dir = startup::shell_dialog_directory(&logging::log_directory(&data_dir));
        let selected_language = self.default_language.clone();
        let current_label = selected_language
            .as_deref()
            .and_then(translation_language_label)
            .unwrap_or("不翻译（仅原文）");
        let language_button = Button::new("ai-default-language")
            .debug_selector(|| "ai-default-language".into())
            .outline()
            .label(current_label)
            .disabled(self.operation.busy())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.item(
                    PopupMenuItem::new("不翻译（仅原文）")
                        .checked(selected_language.is_none())
                        .on_click({
                            let view = language_view.clone();
                            move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_default_language(None, cx);
                                });
                            }
                        }),
                );
                for (tag, label) in TRANSLATION_LANGUAGES {
                    let selected = selected_language.as_deref() == Some(tag);
                    let view = language_view.clone();
                    let tag = tag.to_string();
                    menu = menu.item(PopupMenuItem::new(label).checked(selected).on_click(
                        move |_, _, cx| {
                            let tag = tag.clone();
                            view.update(cx, |this, cx| {
                                this.set_default_language(Some(tag), cx);
                            });
                        },
                    ));
                }
                menu
            });

        let selected_mode = self.translation_display_mode;
        let display_mode_button = Button::new("ai-translation-display-mode")
            .debug_selector(|| "ai-translation-display-mode".into())
            .outline()
            .label(match selected_mode {
                TranslationDisplayMode::Bilingual => "双语",
                TranslationDisplayMode::OriginalOnly => "原文",
                TranslationDisplayMode::TranslationOnly => "译文",
            })
            .disabled(self.operation.busy())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (mode, label) in [
                    (TranslationDisplayMode::TranslationOnly, "译文"),
                    (TranslationDisplayMode::Bilingual, "双语"),
                    (TranslationDisplayMode::OriginalOnly, "原文"),
                ] {
                    let view = display_mode_view.clone();
                    let selected = selected_mode == mode;
                    menu = menu.item(PopupMenuItem::new(label).checked(selected).on_click(
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.set_translation_display_mode(mode, cx);
                            });
                        },
                    ));
                }
                menu
            });

        div()
            .v_flex()
            .gap_5()
            .child(
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
                            .child("PDF 阅读"),
                    )
                    .child(
                        Checkbox::new("ai-pdf-compact-reading")
                            .checked(self.pdf_compact_reading)
                            .disabled(self.operation.busy())
                            .label("PDF 紧凑阅读")
                            .debug_selector(|| "ai-pdf-compact-reading".into())
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                compact_view.update(cx, |this, cx| {
                                    this.pdf_compact_reading = checked;
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
                                "开启后 PDF 阅读窗口的页面上下贴合，不再保留页间留白；关闭时恢复默认间距。\
                                 页面投影、左右留白与阅读位置不变，标注与笔记仍按页面文字位置保存。",
                            ),
                    ),
            )
            .child(
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
                            .child("网络"),
                    )
                    .child(
                        Checkbox::new("ai-use-system-proxy")
                            .checked(self.use_proxy)
                            .disabled(self.operation.busy())
                            .label("使用系统代理")
                            .debug_selector(|| "ai-use-system-proxy".into())
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                use_proxy_view.update(cx, |this, cx| {
                                    this.use_proxy = checked;
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
                                "开启后 AI 问答、联网搜索与后台翻译请求会走系统代理（环境变量 \
                                 HTTP_PROXY / HTTPS_PROXY，NO_PROXY 里的地址仍然直连）。关闭后一律\
                                 直连，本机模型端点（例如 127.0.0.1 上的 Ollama）不会被代理接管。\
                                 保存后新的请求生效，已在进行的请求不受影响。",
                            ),
                    ),
            )
            .child(
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
                            .child("图书翻译"),
                    )
                    .child(language_button)
                    .child(display_mode_button)
                    .child(
                        div()
                            .text_xs()
                            .line_height(gpui::relative(1.5))
                            .text_color(rgb(MUTED))
                            .child(
                                "选择语言后，导入或重新翻译的 EPUB、MOBI、AZW、AZW3、Word 图书会按文本块\
                                翻译为目标语言。“译文”只显示译文、隐藏原文，点击段落可在译文与双语之间\
                                切换，笔记仍锚定原文；“双语”则译文在上、原文在下同时显示；“原文”只显示\
                                原文（后台翻译照常进行）。这里是全局默认，阅读窗口可以按图书切换，\
                                图书自己的选择优先于它。选择“不翻译”则保持原文。",
                            ),
                    ),
            )
            .child(
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
                            .child("存储位置"),
                    )
                    .child(storage_path_row("数据目录", &data_dir))
                    .child(storage_path_row("日志目录", &log_dir))
                    .children(self.render_confirm_overwrite(cx))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("ai-data-directory-change")
                                    .label("更改数据目录…")
                                    .outline()
                                    .disabled(self.operation.busy() || self.choosing_data_directory)
                                    .debug_selector(|| "ai-data-directory-change".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.change_data_directory(window, cx);
                                    })),
                            )
                            .child({
                                let directory = data_dir.clone();
                                Button::new("ai-open-data-directory")
                                    .label("打开数据目录")
                                    .outline()
                                    .debug_selector(|| "ai-open-data-directory".into())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_directory(directory.clone(), "数据目录", cx);
                                    }))
                            })
                            .child({
                                let directory = log_dir.clone();
                                Button::new("ai-open-log-directory")
                                    .label("打开日志目录")
                                    .outline()
                                    .debug_selector(|| "ai-open-log-directory".into())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_directory(directory.clone(), "日志目录", cx);
                                    }))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .line_height(gpui::relative(1.5))
                            .text_color(rgb(MUTED))
                            .child(
                                "图书库、笔记、索引、对象存储和日志都放在数据目录里；首次启动时由你选择，\
                                 记录保存在用户配置目录下的 bootstrap.json。更换位置时整个数据目录会被搬到\
                                 新位置（旧目录随之清空），墨页会自动重启完成搬迁 —— 搬迁在打开图书库之前\
                                 进行，所以本次会话不会再有图书写进旧目录。备份或迁移时直接复制整个数据\
                                 目录即可。日志按 UTC 日期分文件，保留最近 7 天。",
                            ),
                    ),
            )
            .into_any_element()
    }

    /// 目标目录里已经有一个图书库时，先在这里要一次明确的覆盖确认。
    fn render_confirm_overwrite(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let change = self.confirm_overwrite.clone()?;
        Some(
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .bg(rgb(0xf7e1df))
                .child(
                    div()
                        .text_sm()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(0x9f302c))
                        .child(format!(
                            "{} 已经是一个图书库。继续会先删掉它的内容，再把当前数据目录的内容\
                             整体搬过去 —— 这一步不可撤销。",
                            change.target.display()
                        )),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            Button::new("ai-data-directory-overwrite")
                                .label("覆盖并重启")
                                .primary()
                                .debug_selector(|| "ai-data-directory-overwrite".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let Some(change) = this.confirm_overwrite.clone() else {
                                        return;
                                    };
                                    this.commit_data_directory_change(change, true, cx);
                                })),
                        )
                        .child(
                            Button::new("ai-data-directory-overwrite-cancel")
                                .label("取消")
                                .outline()
                                .debug_selector(|| "ai-data-directory-overwrite-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_overwrite = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// 存储位置的只读路径行：标签在上、路径在下，长路径换行显示。
fn storage_path_row(label: &'static str, path: &Path) -> impl IntoElement {
    div()
        .v_flex()
        .gap_0p5()
        .child(div().text_xs().text_color(rgb(MUTED)).child(label))
        .child(
            div()
                .text_sm()
                .line_height(gpui::relative(1.4))
                .text_color(rgb(INK))
                .child(path.display().to_string()),
        )
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
                            .child("切换标签保留输入；保存设置会应用全部标签中的配置。"),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .when(self.active_tab == SettingsTab::Endpoint, |this| {
                                this.child(
                                    Button::new("ai-detect-models")
                                        .debug_selector(|| "ai-detect-models".into())
                                        .outline()
                                        .icon(IconName::Search)
                                        .label(if self.operation == PendingOperation::Detecting {
                                            "正在检测…"
                                        } else {
                                            "检测模型"
                                        })
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            detect_view.update(cx, |this, cx| {
                                                let endpoint_id = this.selected_endpoint_id.clone();
                                                this.detect_models(&endpoint_id, window, cx)
                                            });
                                        }),
                                )
                            })
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
                    app_id: Some("dev.ngy.book-studio.ai-settings".to_string()),
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
    use ngy_book_studio::web_search::{MAX_WEB_SEARCH_TIMEOUT_SECS, MIN_WEB_SEARCH_TIMEOUT_SECS};
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
    use ngy_book_studio::web_search::MAX_WEB_SEARCH_RESULTS;
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

fn parse_background_job_concurrency(value: &str) -> Result<usize> {
    let value = value.trim();
    let concurrency = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!(
            "后台任务并发必须是 {MIN_BACKGROUND_JOB_CONCURRENCY} 到 {MAX_BACKGROUND_JOB_CONCURRENCY} 之间的整数"
        )
    })?;
    anyhow::ensure!(
        (MIN_BACKGROUND_JOB_CONCURRENCY..=MAX_BACKGROUND_JOB_CONCURRENCY).contains(&concurrency),
        "后台任务并发必须是 {MIN_BACKGROUND_JOB_CONCURRENCY} 到 {MAX_BACKGROUND_JOB_CONCURRENCY} 之间的整数"
    );
    Ok(concurrency)
}

fn parse_background_job_interval_ms(value: &str) -> Result<u64> {
    let value = value.trim();
    let interval_ms = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("后台任务间隔必须是 0 到 {MAX_BACKGROUND_JOB_INTERVAL_MS} 之间的整数毫秒")
    })?;
    anyhow::ensure!(
        interval_ms <= MAX_BACKGROUND_JOB_INTERVAL_MS,
        "后台任务间隔必须是 0 到 {MAX_BACKGROUND_JOB_INTERVAL_MS} 之间的整数毫秒"
    );
    Ok(interval_ms)
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

fn canonical_endpoint(endpoint: &str) -> Option<String> {
    normalize_provider_base_url(endpoint.trim())
        .ok()
        .map(|url| url.to_string())
}

fn endpoint_draft_identity(endpoint: &str) -> String {
    canonical_endpoint(endpoint).unwrap_or_else(|| endpoint.trim().to_string())
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
    use ngy_book_studio::credentials::MemoryCredentialStore;

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
        visual.simulate_resize(size(px(1080.), px(1400.)));
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
        let [endpoint, _api_key, model, web_url] = settings.read_with(visual, |view, _| {
            [
                view.selected_endpoint().base_url_input.clone(),
                view.selected_endpoint().api_key_input.clone(),
                view.chat_model_input.clone(),
                view.web_search_url_input.clone(),
            ]
        });
        let endpoint_draft = "http://127.0.0.1:18081/v1/";
        let model_draft = "draft-chat-model";
        let web_draft = "http://127.0.0.1:18082/search?q={query}&format=json";
        let key_draft = "fixture-only-unsaved-key";

        edit_input(visual, &endpoint, endpoint_draft);
        let api_key = settings.read_with(visual, |view, _| {
            view.selected_endpoint().api_key_input.clone()
        });
        let original_ids = [
            endpoint.entity_id(),
            api_key.entity_id(),
            model.entity_id(),
            web_url.entity_id(),
        ];
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

        click_tab(visual, SettingsTab::System);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::System);
            assert!(!view.pdf_compact_reading);
        });
        let compact = visual
            .debug_bounds("ai-pdf-compact-reading")
            .expect("compact reading checkbox must be rendered");
        visual.simulate_click(compact.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert!(view.pdf_compact_reading);
            assert!(view.entered_settings(cx).unwrap().pdf_compact_reading);
            assert!(
                !view
                    .services
                    .provider_settings()
                    .unwrap()
                    .pdf_compact_reading,
                "changing the checkbox must not save the reading preference",
            );
        });

        let proxy = visual
            .debug_bounds("ai-use-system-proxy")
            .expect("system proxy checkbox must be rendered");
        settings.read_with(visual, |view, _| {
            assert!(
                view.use_proxy,
                "the system proxy stays on until the user opts out",
            );
        });
        visual.simulate_click(proxy.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert!(!view.use_proxy);
            assert!(!view.entered_settings(cx).unwrap().use_proxy);
            assert!(
                view.services.provider_settings().unwrap().use_proxy,
                "changing the checkbox must not save the network preference",
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
                        view.selected_endpoint().base_url_input.entity_id(),
                        view.selected_endpoint().api_key_input.entity_id(),
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
                assert!(entered.pdf_compact_reading);
                assert_eq!(
                    view.selected_endpoint().api_key_update(cx),
                    ApiKeyUpdate::Set(key_draft.into())
                );
                assert_eq!(
                    view.services.provider_settings().unwrap().base_url,
                    DEFAULT_OLLAMA_OPENAI_BASE_URL,
                    "changing tabs must not save the draft",
                );
            });
        }
    }

    #[gpui::test]
    fn system_tab_language_dropdown_updates_translation_draft(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        click_tab(visual, SettingsTab::System);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::System);
            assert_eq!(
                view.default_language.as_deref(),
                Some("zh-Hans"),
                "the system tab starts on Simplified Chinese",
            );
        });
        let button = visual
            .debug_bounds("ai-default-language")
            .expect("default-language dropdown must be rendered");
        assert!(button.size.width > px(0.) && button.size.height > px(0.));

        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                view.set_default_language(Some("ja".to_string()), cx);
            });
        });
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(view.default_language.as_deref(), Some("ja"));
            assert_eq!(
                view.entered_settings(cx)
                    .unwrap()
                    .default_language
                    .as_deref(),
                Some("ja")
            );
            assert_eq!(
                view.services
                    .provider_settings()
                    .unwrap()
                    .default_language
                    .as_deref(),
                Some("zh-Hans"),
                "changing the language must not save the draft",
            );
        });
    }

    #[gpui::test]
    fn endpoint_switches_preserve_drafts_and_role_bindings_block_deletion(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        visual.simulate_resize(size(px(1080.), px(1400.)));
        redraw(visual);
        let default_key = settings.read_with(visual, |view, _| {
            view.selected_endpoint().api_key_input.clone()
        });
        edit_input(visual, &default_key, "fixture-default-key");
        let add = visual.debug_bounds("ai-add-endpoint").unwrap();
        visual.simulate_click(add.center(), Modifiers::none());
        redraw(visual);
        assert_hidden_input_does_not_receive_text(visual, &default_key, "fixture-default-key");
        let (extra_id, extra_url) = settings.read_with(visual, |view, cx| {
            let endpoint = view.selected_endpoint();
            assert_ne!(endpoint.id, "default");
            assert!(!endpoint.remote_content_confirmed);
            assert!(!endpoint.allow_insecure_remote_http);
            assert_eq!(endpoint.api_key_update(cx), ApiKeyUpdate::Keep);
            (endpoint.id.clone(), endpoint.base_url_input.clone())
        });
        edit_input(visual, &extra_url, "http://127.0.0.1:18082/v1/");
        let extra_key = settings.read_with(visual, |view, _| {
            view.selected_endpoint().api_key_input.clone()
        });
        edit_input(visual, &extra_key, "fixture-extra-key");
        click_tab(visual, SettingsTab::Models);
        let embedding_choice = visual.debug_bounds("ai-embedding-endpoint-1").unwrap();
        visual.simulate_click(embedding_choice.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            let entered = view.entered_settings(cx).unwrap();
            assert_eq!(entered.endpoint_routing.chat_endpoint_id, "default");
            assert_eq!(entered.endpoint_routing.embedding_endpoint_id, extra_id);
            assert_eq!(entered.endpoint_routing.vision_endpoint_id, "default");
            assert_eq!(
                entered.endpoint_for(ModelRole::Embedding).unwrap().base_url,
                "http://127.0.0.1:18082/v1/"
            );
            assert_eq!(
                view.services.provider_settings().unwrap().endpoints().len(),
                1
            );
        });
        click_tab(visual, SettingsTab::Endpoint);
        let delete = visual.debug_bounds("ai-delete-endpoint").unwrap();
        visual.simulate_click(delete.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(view.endpoints.len(), 2);
            assert!(view.notice.as_ref().unwrap().error);
            assert_eq!(
                view.selected_endpoint().api_key_update(cx),
                ApiKeyUpdate::Set("fixture-extra-key".into())
            );
        });
        let default_choice = visual.debug_bounds("ai-endpoint-choice-0").unwrap();
        visual.simulate_click(default_choice.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(
                view.selected_endpoint().api_key_input.entity_id(),
                default_key.entity_id()
            );
            assert_eq!(
                view.selected_endpoint().api_key_update(cx),
                ApiKeyUpdate::Set("fixture-default-key".into())
            );
            assert_eq!(
                view.endpoints[1].api_key_input.entity_id(),
                extra_key.entity_id()
            );
        });
        visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.assign_endpoint(ModelRole::Embedding, "default", cx);
                view.select_endpoint(&extra_id, window, cx);
                view.delete_selected_endpoint(window, cx);
                assert_eq!(view.endpoints.len(), 1);
                assert_eq!(view.selected_endpoint_id, "default");
            });
        });
    }

    #[gpui::test]
    fn endpoint_url_change_resets_only_its_authorization_key_and_detection(
        cx: &mut TestAppContext,
    ) {
        let (_directory, settings, visual) = open_settings(cx);
        let (url_input, original_key, original_endpoint) =
            settings.read_with(visual, |view, cx| {
                let endpoint = view.selected_endpoint();
                (
                    endpoint.base_url_input.clone(),
                    endpoint.api_key_input.clone(),
                    endpoint.entered(cx).unwrap(),
                )
            });
        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                let endpoint = &mut view.endpoints[0];
                endpoint.remote_content_confirmed = true;
                endpoint.allow_insecure_remote_http = true;
                endpoint.confirmed_remote_endpoint =
                    canonical_endpoint(&original_endpoint.base_url).unwrap();
                endpoint.delete_api_key = true;
                endpoint.detected_models = Some(vec!["old-server-model".into()]);
                cx.notify();
            });
        });
        edit_input(visual, &url_input, "http://127.0.0.1:18083/v1/");
        settings.read_with(visual, |view, cx| {
            let endpoint = view.selected_endpoint();
            assert!(!endpoint.remote_content_confirmed);
            assert!(!endpoint.allow_insecure_remote_http);
            assert!(endpoint.confirmed_remote_endpoint.is_empty());
            assert!(!endpoint.delete_api_key);
            assert!(endpoint.detected_models.is_none());
            assert_ne!(endpoint.api_key_input.entity_id(), original_key.entity_id());
            assert_eq!(endpoint.api_key_update(cx), ApiKeyUpdate::Keep);
            assert!(!view.detection_matches(&original_endpoint, 0, cx));
        });
        visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.max_output_tokens_input
                    .update(cx, |input, cx| input.set_value("0", window, cx));
                assert!(view.entered_settings(cx).is_err());
                // Detecting the selected connection does not parse unrelated model parameters.
                view.selected_endpoint()
                    .entered(cx)
                    .unwrap()
                    .validate()
                    .unwrap();
                let endpoint = view.selected_endpoint().entered(cx).unwrap();
                assert!(view.detection_matches(
                    &endpoint,
                    view.selected_endpoint().discovery_generation,
                    cx
                ));
                view.apply_detected_models(&endpoint, vec![]);
                assert_eq!(view.selected_endpoint().detected_models, Some(vec![]));
            });
        });
    }

    #[gpui::test]
    fn changing_endpoint_keys_or_retrying_detection_invalidates_only_its_models(
        cx: &mut TestAppContext,
    ) {
        let (_directory, settings, visual) = open_settings(cx);
        visual.simulate_resize(size(px(1080.), px(1600.)));
        let (key_input, endpoint, generation) = visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                let endpoint = view.selected_endpoint().entered(cx).unwrap();
                let other = EndpointSettings {
                    id: "second-endpoint".into(),
                    name: "另一个服务".into(),
                    base_url: "http://127.0.0.1:18084/v1/".into(),
                    ..endpoint.clone()
                };
                view.endpoints
                    .push(AiSettingsWindow::new_endpoint_draft(other, window, cx));
                view.endpoints[0].detected_models = Some(vec!["account-a-model".into()]);
                view.endpoints[1].detected_models = Some(vec!["other-server-model".into()]);
                cx.notify();
                (
                    view.endpoints[0].api_key_input.clone(),
                    endpoint,
                    view.endpoints[0].discovery_generation,
                )
            })
        });
        redraw(visual);
        edit_input(visual, &key_input, "fixture-account-b-key");
        settings.read_with(visual, |view, cx| {
            assert!(view.endpoints[0].detected_models.is_none());
            assert_eq!(
                view.endpoints[0].api_key_input.entity_id(),
                key_input.entity_id()
            );
            assert!(!view.detection_matches(&endpoint, generation, cx));
            assert_eq!(
                view.endpoints[1].detected_models,
                Some(vec!["other-server-model".into()])
            );
        });
        let replacement_key = visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.endpoints[0].detected_models = Some(vec!["account-b-model".into()]);
                view.set_delete_endpoint_key("default", true, window, cx);
                assert!(view.endpoints[0].detected_models.is_none());
                view.endpoints[0].detected_models = Some(vec!["anonymous-model".into()]);
                view.set_delete_endpoint_key("default", false, window, cx);
                assert!(view.endpoints[0].detected_models.is_none());
                view.endpoints[0].detected_models = Some(vec!["cached-after-reset".into()]);
                view.endpoints[0].api_key_input.clone()
            })
        });
        redraw(visual);
        edit_input(visual, &replacement_key, "fixture-account-c-key");
        settings.read_with(visual, |view, _| {
            assert!(view.endpoints[0].detected_models.is_none())
        });
        visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.endpoints[0].detected_models = Some(vec!["cached-before-failure".into()]);
                view.endpoints[0]
                    .request_timeout_input
                    .update(cx, |input, cx| input.set_value("0", window, cx));
                view.detect_models("default", window, cx);
                assert_eq!(view.operation, PendingOperation::Idle);
                assert!(view.notice.as_ref().unwrap().error);
                assert!(view.endpoints[0].detected_models.is_none());
                assert_eq!(
                    view.endpoints[1].detected_models,
                    Some(vec!["other-server-model".into()])
                );
            });
        });
    }

    #[gpui::test]
    fn generation_drafts_survive_tabs_and_invalid_save_then_reset(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        visual.simulate_resize(size(px(1080.), px(2200.)));
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

    #[test]
    fn background_job_scheduling_parsing_enforces_supported_ranges() {
        assert_eq!(
            parse_background_job_concurrency(" 1 ").unwrap(),
            MIN_BACKGROUND_JOB_CONCURRENCY
        );
        assert_eq!(
            parse_background_job_concurrency(&MAX_BACKGROUND_JOB_CONCURRENCY.to_string()).unwrap(),
            MAX_BACKGROUND_JOB_CONCURRENCY
        );
        let over_max = (MAX_BACKGROUND_JOB_CONCURRENCY + 1).to_string();
        for invalid in ["", "0", "-1", "1.5", "abc", over_max.as_str()] {
            assert!(
                parse_background_job_concurrency(invalid).is_err(),
                "invalid concurrency unexpectedly accepted: {invalid}"
            );
        }

        assert_eq!(parse_background_job_interval_ms("0").unwrap(), 0);
        assert_eq!(parse_background_job_interval_ms(" 10 ").unwrap(), 10);
        assert_eq!(
            parse_background_job_interval_ms(&MAX_BACKGROUND_JOB_INTERVAL_MS.to_string()).unwrap(),
            MAX_BACKGROUND_JOB_INTERVAL_MS
        );
        for invalid in ["", "-1", "1.5", "abc", "60001"] {
            assert!(
                parse_background_job_interval_ms(invalid).is_err(),
                "invalid interval unexpectedly accepted: {invalid}"
            );
        }
    }

    #[gpui::test]
    fn background_jobs_tab_scheduling_drafts_parse_without_saving(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings(cx);
        // `debug_bounds` accumulates across frames, so the negative check must run
        // before the card is ever drawn: visit the system tab first.
        click_tab(visual, SettingsTab::System);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::System);
        });
        assert!(
            visual.debug_bounds("ai-pdf-compact-reading").is_some(),
            "the system configuration tab must still render its remaining preferences"
        );
        assert!(
            visual
                .debug_bounds("ai-background-job-scheduling")
                .is_none(),
            "task scheduling must not be rendered on the system configuration tab"
        );

        click_tab(visual, SettingsTab::BackgroundJobs);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.active_tab, SettingsTab::BackgroundJobs);
        });
        assert!(
            visual
                .debug_bounds("ai-background-job-scheduling")
                .is_some(),
            "task scheduling must live on the background jobs tab"
        );
        let (concurrency, interval) = settings.read_with(visual, |view, _| {
            (
                view.background_job_concurrency_input.clone(),
                view.background_job_interval_input.clone(),
            )
        });
        edit_input(visual, &concurrency, "0");
        settings.read_with(visual, |view, cx| {
            assert!(
                view.entered_settings(cx).is_err(),
                "out-of-range concurrency must block saving"
            );
        });

        edit_input(visual, &concurrency, "3");
        edit_input(visual, &interval, "250");
        settings.read_with(visual, |view, cx| {
            let entered = view.entered_settings(cx).unwrap();
            assert_eq!(entered.background_job_concurrency, 3);
            assert_eq!(entered.background_job_interval_ms, 250);
            let saved = view.services.provider_settings().unwrap();
            assert_eq!(
                saved.background_job_concurrency,
                DEFAULT_BACKGROUND_JOB_CONCURRENCY
            );
            assert_eq!(
                saved.background_job_interval_ms,
                DEFAULT_BACKGROUND_JOB_INTERVAL_MS
            );
        });
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
                SettingsTab::System,
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
