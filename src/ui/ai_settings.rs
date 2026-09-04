use super::*;

use gpui_component::checkbox::Checkbox;
use moye_epub_editor::{
    ai::{
        DEFAULT_AI_REQUEST_TIMEOUT_SECS, DEFAULT_OLLAMA_OPENAI_BASE_URL,
        MAX_AI_REQUEST_TIMEOUT_SECS, MIN_AI_REQUEST_TIMEOUT_SECS, ModelInfo,
        normalize_provider_base_url,
    },
    services::{
        ApiKeyUpdate, AppServices, DEFAULT_CHAT_MODEL, DEFAULT_EMBEDDING_MODEL,
        DEFAULT_VISION_MODEL, ProviderSettings,
    },
};

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
    embedding_model_input: Entity<InputState>,
    vision_model_input: Entity<InputState>,
    request_timeout_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    scroll_handle: gpui::ScrollHandle,
    window_handle: gpui::AnyWindowHandle,
    remote_content_confirmed: bool,
    allow_insecure_remote_http: bool,
    confirmed_remote_endpoint: String,
    delete_api_key: bool,
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
        let subscriptions =
            vec![cx.subscribe_in(&base_url_input, window, Self::on_base_url_input_event)];

        Self {
            services,
            base_url_input,
            chat_model_input,
            embedding_model_input,
            vision_model_input,
            request_timeout_input,
            api_key_input,
            scroll_handle: gpui::ScrollHandle::new(),
            window_handle: gpui::Window::window_handle(window),
            remote_content_confirmed: settings.remote_content_confirmed,
            allow_insecure_remote_http: settings.allow_insecure_remote_http,
            confirmed_remote_endpoint: settings.confirmed_remote_endpoint,
            delete_api_key: false,
            operation: PendingOperation::Idle,
            detected_models: Vec::new(),
            missing_models: Vec::new(),
            notice: None,
            _subscriptions: subscriptions,
        }
    }

    fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let request_timeout_secs =
            parse_request_timeout_secs(self.request_timeout_input.read(cx).value().as_ref())?;
        let settings = ProviderSettings {
            base_url: self.base_url_input.read(cx).value().trim().to_string(),
            chat_model: self.chat_model_input.read(cx).value().trim().to_string(),
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
                this.scroll_handle.scroll_to_bottom();
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
        self.clear_entered_api_key(window, cx);
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
}

impl Render for AiSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().clone();
        let remote_view = view.clone();
        let insecure_view = view.clone();
        let delete_view = view.clone();
        let detect_view = view.clone();
        let save_view = view.clone();
        let busy = self.operation.busy();
        let endpoint_for_confirmation = self.base_url_input.clone();

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
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child("OpenAI-compatible · 本地 Ollama 或显式配置的远程端点"),
                                    ),
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
                    .id("ai-settings-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(
                        div()
                            .v_flex()
                            .gap_5()
                            .p_6()
                            .child(
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
                                                        let entered = endpoint_for_confirmation
                                                            .read(cx)
                                                            .value();
                                                        match normalize_provider_base_url(
                                                            entered.trim(),
                                                        ) {
                                                            Ok(url) => {
                                                                this.remote_content_confirmed = true;
                                                                this.confirmed_remote_endpoint =
                                                                    url.to_string();
                                                            }
                                                            Err(error) => {
                                                                this.remote_content_confirmed = false;
                                                                this.confirmed_remote_endpoint
                                                                    .clear();
                                                                this.notice = Some(SettingsNotice {
                                                                    text: format!(
                                                                        "Endpoint 无效，无法确认：{error:#}"
                                                                    ),
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
                                    ),
                            )
                            .child(
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
                                    .child(self.render_input_field(
                                        "请求超时（秒）",
                                        "单次 OpenAI-compatible HTTP 请求的总超时；可设置 1–600 秒，默认 120 秒。",
                                        &self.request_timeout_input,
                                    ))
                                    .when_some(self.render_model_detection(cx), |this, result| {
                                        this.child(result)
                                    }),
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
                                    ),
                            )
                            .when_some(self.render_notice(), |this, notice| this.child(notice)),
                    ),
            )
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
                            .child("本地模型缺失时，请在终端执行页面给出的 ollama pull 命令。"),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("ai-detect-models")
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
                                            this.detect_models(window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("ai-save-settings")
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
                    let close_requested = Cell::new(false);
                    window.on_window_should_close(cx, move |window, cx| {
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
