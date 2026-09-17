//! AI 设置的五个独立窗口。
//!
//! 设置按 Endpoint、对话模型、联网搜索、后台任务、系统配置分成五块，每一块一个原生
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AiSettingsSection {
    Endpoint,
    Models,
    WebSearch,
    BackgroundJobs,
    System,
}

impl AiSettingsSection {
    /// 菜单里的展示顺序。
    pub(super) const ALL: [Self; 5] = [
        Self::Endpoint,
        Self::Models,
        Self::WebSearch,
        Self::BackgroundJobs,
        Self::System,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint",
            Self::Models => "对话模型",
            Self::WebSearch => "联网搜索",
            Self::BackgroundJobs => "后台任务",
            Self::System => "系统配置",
        }
    }

    /// 单例键、app_id 与调试选择器串里用的短名。
    fn slug(self) -> &'static str {
        match self {
            Self::Endpoint => "endpoint",
            Self::Models => "models",
            Self::WebSearch => "web-search",
            Self::BackgroundJobs => "background-jobs",
            Self::System => "system",
        }
    }

    /// 窗口副标题：一句话说明这个窗口负责什么。
    fn subtitle(self) -> &'static str {
        match self {
            Self::Endpoint => "连接、密钥与远程授权；每个 Endpoint 独立保存。",
            Self::Models => "三个模型各自选择 Endpoint，并配置对话生成参数。",
            Self::WebSearch => "书中检索不到时的可选联网补充。",
            Self::BackgroundJobs => "新任务的运行方式与调度节奏。",
            Self::System => "界面外观、阅读偏好、翻译默认与存储位置。",
        }
    }

    /// 标题栏右侧的一句边界说明。
    fn header_note(self) -> &'static str {
        match self {
            Self::Endpoint => "墨页不会启动、停止或下载 Ollama 模型",
            Self::Models => "检测只读取模型列表，不改动服务端",
            Self::WebSearch => "默认关闭，关闭时不联网",
            Self::BackgroundJobs => "调度在这里；运行明细在图书库工具栏的「后台任务」窗口",
            Self::System => "只影响本机体验，不改动图书内容",
        }
    }

    /// 提交成功后的提示。每个窗口保存的只是它自己那一块。
    fn success_notice(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint 设置已保存；新的请求会使用新的连接与密钥。",
            Self::Models => "对话模型设置已保存；新的搜索与问答请求会使用它们。",
            Self::WebSearch => "联网搜索设置已保存；新的搜索请求会使用它们。",
            Self::BackgroundJobs => {
                "后台任务设置已保存；调度立即对新任务生效，正在运行的任务不受影响。"
            }
            Self::System => "系统配置已保存，并已应用到已经打开的窗口。",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Endpoint => IconName::Settings2,
            Self::Models => IconName::Bot,
            Self::WebSearch => IconName::Globe,
            Self::BackgroundJobs => IconName::LoaderCircle,
            Self::System => IconName::Palette,
        }
    }

    fn window_title(self) -> String {
        format!("墨页 · AI 设置 · {}", self.label())
    }

    fn app_id(self) -> String {
        format!("dev.ngy.book-studio.ai-settings.{}", self.slug())
    }

    fn window_size(self) -> gpui::Size<gpui::Pixels> {
        match self {
            Self::Endpoint => size(px(760.), px(820.)),
            Self::Models => size(px(820.), px(880.)),
            Self::WebSearch => size(px(760.), px(860.)),
            Self::BackgroundJobs => size(px(720.), px(620.)),
            Self::System => size(px(800.), px(900.)),
        }
    }

    fn window_min_size(self) -> gpui::Size<gpui::Pixels> {
        size(px(560.), px(440.))
    }

    /// 本窗口内的调试选择器：`ai-<窗口>-<name>`。
    fn selector(self, name: &str) -> String {
        format!("ai-{}-{name}", self.slug())
    }

    /// [`Self::selector`] 的 `debug_selector` 形态。
    fn debug_selector(self, name: &str) -> impl Fn() -> String + 'static {
        let selector = self.selector(name);
        move || selector.clone()
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

/// 界面等待一次设置保存的上限。
///
/// 保存自己没有超时：它要依次拿两把进程级门闩（`AppServices::ai_configuration` 与索引
/// 协调器的 `transitions`），被后台任务堵住时可以无限期等下去。界面如果陪着一起等，
/// 窗口就永远停在「正在安全保存设置…」——保存按钮被 `PendingOperation::Saving`
/// 禁用、关窗又被告知「保存未结束」，用户除了杀进程没有别的出路（现场 2026-09-17）。
///
/// 超时只放弃**等**，不放弃写入：任务已经在 `AppServices` 的 runtime 上跑，它会自己
/// 提交或回滚（见 `configure_providers` 的文档），所以提示必须写成「可能已提交」，
/// 不能写成失败。这个上限同时是退出软件时等待结果的上界。
const SAVE_WAIT_LIMIT: Duration = Duration::from_secs(60);

/// 一条显示在底部的提示（成功为绿色、失败为红色）。
#[derive(Clone, Debug)]
struct SettingsNotice {
    text: String,
    error: bool,
}

/// 一个设置窗口的运行状态。五个窗口各有一份。
struct SettingsWindowState {
    operation: PendingOperation,
    notice: Option<SettingsNotice>,
    /// 关窗时用它在当前帧之后移除本窗口。
    window_handle: gpui::AnyWindowHandle,
    /// 正文的滚动位置；每个窗口独立，互不影响。
    scroll_handle: gpui::ScrollHandle,
}

impl SettingsWindowState {
    fn new(window: &Window) -> Self {
        Self {
            operation: PendingOperation::Idle,
            notice: None,
            window_handle: gpui::Window::window_handle(window),
            scroll_handle: gpui::ScrollHandle::new(),
        }
    }

    fn busy(&self) -> bool {
        self.operation.busy()
    }

    fn notice(&self) -> Option<&SettingsNotice> {
        self.notice.as_ref()
    }

    fn report(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(SettingsNotice {
            text: text.into(),
            error,
        });
    }

    /// 开始一次提交：按钮进入「正在保存…」并给出提示。
    fn begin_save(&mut self) {
        self.operation = PendingOperation::Saving;
        self.report("正在安全保存设置…", false);
    }

    fn begin_detection(&mut self, text: impl Into<String>) {
        self.operation = PendingOperation::Detecting;
        self.report(text, false);
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

/// 一个设置窗口的应用级登记：同一个窗口重复打开时激活已经打开的那个，不新建。
struct SettingsWindowTracker<V: 'static> {
    opening: bool,
    closing: bool,
    handle: Option<gpui::AnyWindowHandle>,
    view: Option<WeakEntity<V>>,
}

impl<V: 'static> Default for SettingsWindowTracker<V> {
    fn default() -> Self {
        Self {
            opening: false,
            closing: false,
            handle: None,
            view: None,
        }
    }
}

/// 五个窗口各自的登记表。窗口类型不同，所以每条登记单独一个字段。
#[derive(Default)]
struct SettingsWindowTrackers {
    endpoint: SettingsWindowTracker<EndpointSettingsWindow>,
    models: SettingsWindowTracker<ModelsSettingsWindow>,
    web_search: SettingsWindowTracker<WebSearchSettingsWindow>,
    background_jobs: SettingsWindowTracker<BackgroundJobsSettingsWindow>,
    system: SettingsWindowTracker<SystemSettingsWindow>,
}

impl gpui::Global for SettingsWindowTrackers {}

fn ensure_settings_window_trackers(cx: &mut App) {
    if !cx.has_global::<SettingsWindowTrackers>() {
        cx.set_global(SettingsWindowTrackers::default());
    }
}

/// 一次提交前需要用户确认的对话框。`cancelled` 是用户选择取消时的提示文案。
struct SaveConfirmation {
    answer: Pin<Box<dyn Future<Output = bool> + 'static>>,
    cancelled: &'static str,
}

impl SaveConfirmation {
    fn new(answer: impl Future<Output = bool> + 'static, cancelled: &'static str) -> Self {
        Self {
            answer: Box::pin(answer),
            cancelled,
        }
    }
}

/// 一个设置窗口的最小契约：身份、状态、提交，以及提交成功后要立刻应用的本地偏好。
///
/// 只在 `ai_settings` 内部使用：窗口类型对外只通过各自的 `open_*` 函数暴露。
trait SettingsWindowView: Render + Sized + 'static {
    fn section() -> AiSettingsSection;

    /// 本窗口在 [`SettingsWindowTrackers`] 里的登记。
    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self>;

    fn state(&self) -> &SettingsWindowState;

    fn state_mut(&mut self) -> &mut SettingsWindowState;

    fn services(&self) -> &Arc<AppServices>;

    /// 校验草稿并提交。只覆盖本窗口负责的字段。
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>);

    /// 提交成功后立刻应用的本地偏好（皮肤、PDF 阅读、翻译显示方式）。
    fn apply_saved_preferences(&mut self, _cx: &mut App) {}
}

/// 需要在窗口里列出各 Endpoint 的设置窗口（Endpoint 与对话模型）。
trait EndpointBackedWindow: SettingsWindowView {
    fn endpoints(&self) -> &[EndpointDraft];

    fn endpoints_mut(&mut self) -> &mut Vec<EndpointDraft>;

    /// 某个角色当前填写的模型名。Endpoint 窗口没有模型输入，返回 `None`。
    fn role_model(&self, _role: ModelRole, _cx: &App) -> Option<String> {
        None
    }

    /// 采纳检测结果里的模型名。Endpoint 窗口没有模型输入，默认忽略。
    fn adopt_detected_model(
        &mut self,
        _role: ModelRole,
        _model: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}

/// 打开五个设置窗口里的一个。
pub(super) fn open_ai_settings_window(
    services: Arc<AppServices>,
    section: AiSettingsSection,
    cx: &mut App,
) -> Result<()> {
    // 配置读不出来就不开窗口：让调用方把原因显示在主窗口的错误栏里。
    let settings = services.provider_settings()?;
    match section {
        AiSettingsSection::Endpoint => {
            open_settings_window::<EndpointSettingsWindow>(cx, move |window, cx| {
                EndpointSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::Models => {
            open_settings_window::<ModelsSettingsWindow>(cx, move |window, cx| {
                ModelsSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::WebSearch => {
            open_settings_window::<WebSearchSettingsWindow>(cx, move |window, cx| {
                WebSearchSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::BackgroundJobs => {
            open_settings_window::<BackgroundJobsSettingsWindow>(cx, move |window, cx| {
                BackgroundJobsSettingsWindow::new(services, settings, window, cx)
            })
        }
        AiSettingsSection::System => {
            open_settings_window::<SystemSettingsWindow>(cx, move |window, cx| {
                SystemSettingsWindow::new(services, settings, window, cx)
            })
        }
    }
}

fn open_settings_window<V: SettingsWindowView>(
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    ensure_settings_window_trackers(cx);
    if activate_existing_settings_window::<V>(cx)? {
        return Ok(());
    }
    let section = V::section();
    {
        let tracker = V::tracker(cx);
        tracker.opening = true;
        tracker.closing = false;
        tracker.handle = None;
        tracker.view = None;
    }
    let opened: Result<gpui::AnyWindowHandle> = (|| {
        let bounds = Bounds::centered(None, section.window_size(), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(section.window_min_size()),
                    titlebar: Some(TitlebarOptions {
                        title: Some(section.window_title().into()),
                        ..Default::default()
                    }),
                    app_id: Some(section.app_id()),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| build(window, cx));
                    V::tracker(cx).view = Some(view.downgrade());
                    let settings_window = gpui::Window::window_handle(window);
                    let close_view = view.downgrade();
                    let close_requested = Cell::new(false);
                    on_window_close(window, cx, move |window, cx| {
                        // 退出软件时才等这次保存的结果（`ApplicationExit` 逐个关子窗口，
                        // 已接受的凭据/SQLite 写入不能在半路被丢掉）。用户自己关窗不拦：
                        // 写入仍由运行中的任务提交或回滚，视图消失只是丢掉结果回调
                        // （下面的 `let _ = view.update(...)` 本来就能容忍）。
                        // 无条件拦下来会把一次被后台任务堵住的保存变成关不掉的死窗口
                        // （现场 2026-09-17）；等待的上界由 [`SAVE_WAIT_LIMIT`] 给出。
                        if application_is_exiting(cx)
                            && close_view.upgrade().is_some_and(|view| {
                                view.read(cx).state().operation == PendingOperation::Saving
                            })
                        {
                            return false;
                        }
                        let tracker = V::tracker(cx);
                        if tracker.handle == Some(settings_window) {
                            tracker.closing = true;
                        }
                        window.minimize_window();
                        if !close_requested.replace(true) {
                            remove_window_after_current_frame(window, cx, None);
                        }
                        false
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .with_context(|| format!("无法创建「{}」设置窗口", section.label()))?;
        Ok(handle.into())
    })();

    let tracker = V::tracker(cx);
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

fn activate_existing_settings_window<V: SettingsWindowView>(cx: &mut App) -> Result<bool> {
    let (opening, closing, handle, view_is_alive) = {
        let tracker = V::tracker(cx);
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
            let tracker = V::tracker(cx);
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
                .with_context(|| {
                    format!("无法激活已经打开的「{}」设置窗口", V::section().label())
                })?;
            Ok(true)
        }
    }
}

/// 提交一份设置，并在成功后关掉发起它的窗口。
///
/// 等待有上限（[`SAVE_WAIT_LIMIT`]）：保存要依次拿两把进程级门闩，被后台任务堵住时
/// 不会自己结束。超时只丢掉这次「等待」——写入仍由已经在跑的任务完成或回滚，所以结果
/// 是不确定的，提示必须如实说。
fn commit_settings<V: SettingsWindowView>(
    services: Arc<AppServices>,
    settings: ProviderSettings,
    key_updates: BTreeMap<String, ApiKeyUpdate>,
    web_key_update: ApiKeyUpdate,
    confirmation: Option<SaveConfirmation>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let success_notice = V::section().success_notice();
    cx.spawn_in(window, async move |view, cx| {
        if let Some(confirmation) = confirmation
            && !confirmation.answer.await
        {
            let _ = view.update(cx, |this, cx| {
                this.state_mut().operation = PendingOperation::Idle;
                this.state_mut().report(confirmation.cancelled, false);
                cx.notify();
            });
            return;
        }
        // AppServices moves credential and SQLite work to its dedicated runtime;
        // this UI future only awaits and applies the result.
        let save = Box::pin(services.configure_providers(settings, key_updates));
        let outcome =
            match futures_util::future::select(save, Box::pin(Timer::after(SAVE_WAIT_LIMIT))).await
            {
                futures_util::future::Either::Left((outcome, _)) => Some(outcome),
                futures_util::future::Either::Right((_, _)) => None,
            };
        let outcome = outcome.map(|outcome| {
            outcome.and_then(|()| match &web_key_update {
                ApiKeyUpdate::Keep => Ok(()),
                ApiKeyUpdate::Set(value) => services.set_web_search_api_key(value),
                ApiKeyUpdate::Delete => services.delete_web_search_api_key(),
            })
        });
        let _ = view.update(cx, |this, cx| {
            let window_handle = this.state().window_handle;
            this.state_mut().operation = PendingOperation::Idle;
            let Some(outcome) = outcome else {
                this.state_mut().report(
                    format!(
                        "保存已等待 {} 秒仍未结束，仍在后台进行：设置可能已经保存，也可能仍被后台任务阻塞。\
                         可以直接关闭本窗口，稍后重新打开确认后再决定是否重存。",
                        SAVE_WAIT_LIMIT.as_secs()
                    ),
                    true,
                );
                // 正在退出软件时不能把退出流程留在这里等：这次等待的上限已经到了，
                // 窗口不再替它把关（写入仍在运行，进程退出可能打断它，后果与用户
                // 直接结束进程相同）。非退出时保留窗口，由用户决定关闭还是稍后重存。
                if application_is_exiting(cx) {
                    cx.spawn(async move |_entity, cx| {
                        let _ = window_handle.update(cx, |_, window, cx| {
                            remove_window_after_current_frame(window, cx, None);
                        });
                    })
                    .detach();
                }
                cx.notify();
                return;
            };
            match outcome {
                Ok(()) => {
                    this.state_mut().report(success_notice, false);
                    // Saved settings now reach documents that are already open; the
                    // write above is the source of truth.
                    this.apply_saved_preferences(cx);
                    cx.spawn(async move |_entity, cx| {
                        let _ = window_handle.update(cx, |_, window, cx| {
                            remove_window_after_current_frame(window, cx, None);
                        });
                    })
                    .detach();
                }
                Err(error) => {
                    cancel_application_exit(cx);
                    this.state_mut()
                        .report(format!("保存设置失败：{error:#}"), true);
                }
            }
            cx.notify();
        });
    })
    .detach();
}

/// 五个设置窗口共用的外框：标题栏、可滚动正文、提示条与底部保存栏。
fn settings_frame<V: SettingsWindowView>(
    view: &V,
    body: gpui::AnyElement,
    footer_extra: Option<gpui::AnyElement>,
    cx: &mut Context<V>,
) -> gpui::AnyElement {
    let section = V::section();
    let state = view.state();
    let busy = state.busy();
    let scroll_handle = state.scroll_handle.clone();
    let notice = state.notice().cloned();
    let save_label = if state.operation == PendingOperation::Saving {
        "正在保存…"
    } else {
        "保存设置"
    };

    div()
        .v_flex()
        .size_full()
        .bg(rgb(palette(cx).paper))
        .child(
            div()
                .h_flex()
                .flex_none()
                .justify_between()
                .gap_3()
                .px_6()
                .py_4()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
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
                                .bg(rgb(palette(cx).accent_soft))
                                .text_color(rgb(palette(cx).accent))
                                .child(Icon::new(section.icon())),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_semibold()
                                        .text_color(rgb(palette(cx).ink))
                                        .child(format!("AI 设置 · {}", section.label())),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(palette(cx).muted))
                                        .child(section.subtitle()),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child(section.header_note()),
                ),
        )
        .child(
            div()
                .id(SharedString::from(section.selector("scroll")))
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .child(div().p_6().child(body)),
        )
        .when_some(notice, |this, notice| {
            this.child(
                div()
                    .flex_none()
                    .px_6()
                    .py_2()
                    .child(render_notice(&notice, palette(cx))),
            )
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
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
                .child(
                    div()
                        .max_w(px(440.))
                        .text_xs()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(palette(cx).muted))
                        .child("保存只写入本窗口的设置；其它设置窗口里未保存的输入不会被覆盖。"),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .when_some(footer_extra, |this, extra| this.child(extra))
                        .child(
                            Button::new(SharedString::from(section.selector("save-settings")))
                                .debug_selector(section.debug_selector("save-settings"))
                                .primary()
                                .icon(IconName::Check)
                                .label(save_label)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                        ),
                ),
        )
        .into_any_element()
}

/// 输入框行的统一样式：标签、输入框、说明文字。
fn render_input_field(
    theme: Palette,
    busy: bool,
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
                .text_color(rgb(theme.ink))
                .child(label),
        )
        .child(Input::new(input).disabled(busy))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted))
                .child(description),
        )
        .into_any_element()
}

fn render_notice(notice: &SettingsNotice, theme: Palette) -> gpui::AnyElement {
    let (background, foreground, icon) = if notice.error {
        (
            rgb(theme.danger_soft),
            rgb(theme.danger),
            IconName::TriangleAlert,
        )
    } else {
        (
            rgb(theme.success_soft),
            rgb(theme.success),
            IconName::CircleCheck,
        )
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
}

/// 设置面板的统一容器：白底、圆角、细边框。
fn panel(theme: Palette, children: Vec<gpui::AnyElement>) -> gpui::AnyElement {
    div()
        .v_flex()
        .gap_3()
        .p_4()
        .rounded(px(12.))
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.surface))
        .children(children)
        .into_any_element()
}

/// 面板里的小节标题。
fn card_title(theme: Palette, text: impl Into<SharedString>) -> gpui::AnyElement {
    div()
        .text_sm()
        .font_semibold()
        .text_color(rgb(theme.ink))
        .child(text.into())
        .into_any_element()
}

/// 面板里的说明文字。
fn card_note(theme: Palette, text: impl Into<SharedString>) -> gpui::AnyElement {
    div()
        .text_xs()
        .line_height(gpui::relative(1.5))
        .text_color(rgb(theme.muted))
        .child(text.into())
        .into_any_element()
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
    fn every_section_has_a_distinct_singleton_selector_and_app_id() {
        let mut slugs = std::collections::BTreeSet::new();
        for section in AiSettingsSection::ALL {
            assert!(slugs.insert(section.slug()));
            assert_eq!(
                section.selector("save-settings"),
                format!("ai-{}-save-settings", section.slug())
            );
            assert!(section.app_id().ends_with(section.slug()));
            assert!(!section.label().is_empty());
            assert!(section.window_min_size().width <= section.window_size().width);
            assert!(section.window_min_size().height <= section.window_size().height);
        }
    }
}
