#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Application, Bounds, WeakEntity, Window, WindowBounds,
    WindowOptions, px, size,
};
use gpui_component_assets::Assets;
use native_dialog::{DialogBuilder, MessageLevel};
use ngy_book_studio::{
    library::LibraryStore,
    logging,
    services::AppServices,
    startup::{self, DataDirSelection, LaunchPlan, ReselectReason},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use ui::{DataDirSetupWindow, EpubReaderApp, open_data_dir_setup_window, wrap_root};

#[cfg(target_os = "windows")]
const WEBVIEW2_DOWNLOAD_URL: &str =
    "https://developer.microsoft.com/microsoft-edge/webview2/consumer/";

/// 日志 subscriber 只能安装一次。用户在设置窗口里改过数据目录后重试启动时，仍沿用
/// 第一次装好的那个。
static LOGGING_INSTALLED: AtomicBool = AtomicBool::new(false);

/// 日志 worker guard 必须存活到进程退出，否则非阻塞写入的尾部日志会丢失。
/// `ngy_utils_tracing::init` 返回的 guard 存这里，进程退出前显式取走并 drop 以刷盘。
static LOG_GUARD: Mutex<Option<ngy_utils_tracing::InitResult>> = Mutex::new(None);

fn main() {
    // GPUI's DirectComposition renderer and a child HWND WebView cannot be layered
    // reliably on Windows. This is set before GPUI starts any worker threads.
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1");
    }

    #[cfg(target_os = "windows")]
    if !ensure_webview2_runtime() {
        return;
    }

    // 数据目录的设置界面是应用自己的窗口，所以这里只判断「能不能直接用」：需要用户
    // 确认时把计划带进 GPUI，由设置窗口负责界面，确认后再打开图书库。
    let plan = match startup::plan_launch() {
        Ok(plan) => plan,
        Err(error) => {
            show_fatal_error("无法启动墨页", format!("无法确定数据目录：\n\n{error:#}"));
            return;
        }
    };

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            match plan {
                LaunchPlan::Ready(selection) => start_application(selection, None, cx),
                LaunchPlan::NeedsSetup { reason, suggestion } => {
                    open_data_dir_setup(cx, reason, suggestion);
                }
            }
        });

    // run 返回即进程退出：显式取走并 drop 日志 guard，确保非阻塞写入的尾部日志落盘。
    if let Ok(mut guard) = LOG_GUARD.lock() {
        if let Some(result) = guard.take() {
            drop(result);
        }
    }
}

/// 设置窗口的句柄：窗口自己（启动失败时把原因显示回去）和它的窗口句柄（启动成功后
/// 关掉它）。
struct SetupHandles {
    view: WeakEntity<DataDirSetupWindow>,
    window: AnyWindowHandle,
}

/// 启动阶段失败的善后：显示什么、回到哪个界面、要不要清除引导配置。
struct StartupFailure {
    message: String,
    reason: ReselectReason,
    suggestion: PathBuf,
    /// 是否清除引导配置。搬迁没完成时不能清 —— 待搬的源目录就记在配置里。
    forget_config: bool,
}

impl StartupFailure {
    fn library(data_dir: PathBuf, error: anyhow::Error) -> Self {
        let message = format!("无法打开图书库：\n\n{error:#}");
        Self {
            reason: ReselectReason::Unusable(message.clone()),
            message,
            suggestion: data_dir,
            forget_config: true,
        }
    }

    fn window(data_dir: PathBuf, error: anyhow::Error) -> Self {
        let message = format!("无法创建应用窗口：\n\n{error:#}");
        Self {
            reason: ReselectReason::Unusable(message.clone()),
            message,
            suggestion: data_dir,
            forget_config: true,
        }
    }

    /// 搬迁失败：数据还在旧目录，必须保留配置，下次启动才会继续搬。
    fn move_failed(from: Option<PathBuf>, data_dir: PathBuf, error: anyhow::Error) -> Self {
        let message = format!("无法把原数据目录的内容搬到新目录：\n\n{error:#}");
        Self {
            reason: ReselectReason::MoveFailed {
                from: from.unwrap_or_else(|| data_dir.clone()),
                error: format!("{error:#}"),
            },
            message,
            suggestion: data_dir,
            forget_config: false,
        }
    }
}

/// 打开数据目录设置窗口。用户在窗口里确认后由 [`start_application`] 接手。
fn open_data_dir_setup(cx: &mut App, reason: ReselectReason, suggestion: PathBuf) {
    let config_path = match startup::bootstrap_path() {
        Ok(path) => path,
        Err(error) => {
            show_fatal_error(
                "无法启动墨页",
                format!("无法定位目录设置文件：\n\n{error:#}"),
            );
            cx.quit();
            return;
        }
    };
    let opened = open_data_dir_setup_window(
        cx,
        config_path,
        reason,
        suggestion,
        |selection, window, cx, view| {
            let handles = SetupHandles {
                view,
                window: Window::window_handle(window),
            };
            start_application(selection, Some(handles), cx);
        },
    );
    if let Err(error) = opened {
        // 连设置窗口都开不出来，已经没有可以显示错误的界面了。
        show_fatal_error("无法启动墨页", format!("无法创建设置窗口：\n\n{error:#}"));
        cx.quit();
    }
}

/// 用确定下来的数据目录启动：完成待办搬迁、装日志、打开图书库，然后开主窗口。
///
/// 全在后台执行器上跑 —— 数据目录可能在网络盘上，而且 GPUI 回调里阻塞会直接卡住界面。
/// 失败时把原因显示回设置窗口（没有窗口就先开一个）。
fn start_application(selection: DataDirSelection, setup: Option<SetupHandles>, cx: &mut App) {
    let data_dir = selection.data_dir.clone();
    let config_path = startup::bootstrap_path();
    let pending = config_path
        .as_ref()
        .ok()
        .and_then(|path| startup::load_pending_move(path).ok().flatten());
    let opened = cx.background_executor().spawn({
        let data_dir = data_dir.clone();
        let selection = selection.clone();
        async move {
            // 搬迁必须排在日志与图书库之前：日志写在数据目录里，库一打开就占住旧目录，
            // 两者都会让「整体重命名」失败（Windows 不允许重命名含打开文件的目录）。
            // 传本次真正要用的目录：`NGY_DATA_DIR` 覆盖时会与配置里记的不一致，那就不搬。
            if let Ok(config_path) = &config_path {
                match startup::complete_pending_move(config_path, &data_dir) {
                    Ok(Some(from)) => tracing::info!(
                        from = %from.display(),
                        to = %data_dir.display(),
                        "数据目录搬迁完成"
                    ),
                    Ok(None) => {}
                    Err(error) => {
                        return Err(StartupFailure::move_failed(
                            pending.map(|pending| pending.from),
                            data_dir.clone(),
                            error,
                        ));
                    }
                }
            }
            install_logging(&selection);
            open_library(&data_dir)
                .map_err(|error| StartupFailure::library(data_dir.clone(), error))
        }
    });

    cx.spawn(async move |cx| {
        let outcome = opened.await;
        let _ = cx.update(|cx| match outcome {
            Ok((services, library)) => match open_main_window(cx, library, services) {
                Ok(()) => {
                    if let Some(handles) = setup {
                        let _ =
                            cx.update_window(handles.window, |_, window, _| window.remove_window());
                    }
                }
                Err(error) => {
                    report_startup_failure(cx, setup, StartupFailure::window(data_dir, error));
                }
            },
            Err(failure) => report_startup_failure(cx, setup, failure),
        });
    })
    .detach();
}

/// 启动失败：说明原因，让用户换成别的目录再试。
fn report_startup_failure(cx: &mut App, setup: Option<SetupHandles>, failure: StartupFailure) {
    // 搬迁失败时文件日志还没装上，另写一份到 stderr，保证这条错误一定看得到。
    eprintln!("[ngy-book-studio] {}", failure.message);
    tracing::error!("{}", failure.message);
    if failure.forget_config {
        // 记录留着的后果是每次启动都直接使用同一个打不开的目录；清除后下次启动会重新询问。
        match startup::bootstrap_path() {
            Ok(config_path) => {
                if let Err(error) = startup::forget_remembered_data_dir(&config_path) {
                    tracing::error!(%error, "failed to forget the remembered data directory");
                }
            }
            Err(error) => tracing::error!(%error, "failed to locate the bootstrap config"),
        }
    }

    match setup {
        Some(handles) => {
            let _ = handles
                .view
                .update(cx, |this, cx| this.report_failure(failure.message, cx));
        }
        None => open_data_dir_setup(cx, failure.reason, failure.suggestion),
    }
}

/// 安装日志：由 `ngy_utils_tracing` 安装全局 subscriber，文件日志落到
/// `<数据目录>/logs/`（默认 Production 模式，Development/Test 模式行为见 `ngy_utils_tracing`）。
///
/// 数据目录在启动早期才能确定，所以这里才装 subscriber；在此之前（含设置窗口阶段）
/// 的 tracing 事件会丢弃。subscriber 只能装一次：`LOGGING_INSTALLED` 保证本函数只跑一次。
///
/// 模式由 `MOYE_APP_MODE` 环境变量（或第一个命令行参数覆盖）决定；`ngy_utils_tracing`
/// 未设置时默认 `production`，文件日志始终写到数据目录。该库先加载 `.env` 再读取模式，
/// 因此 `.env` 里的 `MOYE_APP_MODE` 也生效，且因用 `dotenv_override`，`.env` 的值会覆盖
/// 进程环境变量；命令行参数的覆盖优先级最高。目录创建、按日滚动、保留最近
/// [`logging::LOG_MAX_FILES`] 个文件与过期清理也由该库在 `init` 时完成（并在后台定期执行），
/// 本项目不再自行实现。
fn install_logging(selection: &DataDirSelection) {
    if LOGGING_INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }

    // `RUST_LOG` 未设置时给一个项目默认过滤；`ngy_utils_tracing` 的控制台层与文件层都读取它。
    if std::env::var_os("RUST_LOG").is_none() {
        // SAFETY: 启动早期、全局 subscriber 安装之前调用，此时还没有其它线程读该环境变量。
        unsafe {
            std::env::set_var("RUST_LOG", logging::DEFAULT_LOG_FILTER);
        }
    }

    // 文件日志落到 `<数据目录>/logs/`（`log_dir` 直接传入，优先级高于 `LOG_DIR`），
    // 前缀 `ngy-book-studio`，保留最近 `LOG_MAX_FILES` 个日滚动文件。
    let log_dir = logging::log_directory(&selection.data_dir);
    let crates = vec!["ngy-book-studio".to_string()];
    let options = ngy_utils_tracing::InitOptions::default()
        // 第一个命令行参数作为模式覆盖（development/test/production），优先级高于 `MOYE_APP_MODE`。
        .mode_override(std::env::args().nth(1))
        .crates(crates.clone())
        .log_dir(Some(log_dir.to_string_lossy().into_owned()))
        .log_prefix(Some(logging::LOG_PREFIX.to_string()))
        .max_log_files(Some(logging::LOG_MAX_FILES))
        .mode_env_var(Some("MOYE_APP_MODE".to_string()));
    match ngy_utils_tracing::init(options) {
        Ok(result) => {
            *LOG_GUARD.lock().expect("log guard lock poisoned") = Some(result);
            tracing::info!(
                log_dir = %log_dir.display(),
                "{}",
                startup::describe(selection)
            );
        }
        Err(error) => {
            eprintln!("[ngy-book-studio] 日志初始化失败，仅输出到控制台：{error:#}");
            // 全局 subscriber 尚未安装时退回控制台日志；已经装过则忽略。
            if ngy_utils_tracing::console_tracing(crates).is_err() {
                eprintln!("[ngy-book-studio] 控制台日志也未能安装，本次运行的日志可能全部丢失");
            }
        }
    }
}

/// 打开图书库并取一次视图快照。回调返回给主窗口构造使用。
fn open_library(data_dir: &Path) -> anyhow::Result<(Arc<AppServices>, LibraryStore)> {
    let services = Arc::new(AppServices::open(data_dir)?);
    let library = services.library_snapshot()?;
    Ok((services, library))
}

fn open_main_window(
    cx: &mut App,
    library: LibraryStore,
    services: Arc<AppServices>,
) -> anyhow::Result<()> {
    let bounds = Bounds::centered(None, size(px(1260.), px(820.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(940.), px(640.))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("墨页 · 私人图书馆".into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let app = cx.new(|cx| EpubReaderApp::new(library, Arc::clone(&services), window, cx));
            cx.new(|cx| wrap_root(app, window, cx))
        },
    )?;
    cx.activate(true);
    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_webview2_runtime() -> bool {
    let detection_error = match gpui_component::wry::webview_version() {
        Ok(version) if !version.trim().is_empty() => {
            tracing::debug!(%version, "WebView2 Runtime detected");
            return true;
        }
        Ok(_) => "未返回版本信息".to_string(),
        Err(error) => error.to_string(),
    };

    tracing::error!(%detection_error, "WebView2 Runtime is unavailable");
    let prompt = DialogBuilder::message()
        .set_level(MessageLevel::Error)
        .set_title("需要安装 WebView2 Runtime")
        .set_text(format!(
            "墨页需要 Microsoft Edge WebView2 Runtime 才能显示图书正文、PDF 和编辑器，但当前系统未检测到可用版本。\n\n是否打开微软官方安装页面？\n\n安装完成后，请重新启动墨页。\n\n检测信息：{detection_error}"
        ))
        .confirm()
        .show();

    match prompt {
        Ok(true) => {
            if let Err(error) = open::that_detached(WEBVIEW2_DOWNLOAD_URL) {
                tracing::error!(%error, "failed to open WebView2 download page");
                show_fatal_error(
                    "无法打开安装页面",
                    format!("请在浏览器中手动访问：\n\n{WEBVIEW2_DOWNLOAD_URL}\n\n{error}"),
                );
            }
        }
        Ok(false) => {}
        Err(error) => tracing::error!(%error, "failed to show WebView2 installation prompt"),
    }

    false
}

/// 原生错误提示，只用于 GPUI 还没启动、或已经没有可用窗口的阶段。
fn show_fatal_error(title: &str, text: String) {
    let _ = DialogBuilder::message()
        .set_level(MessageLevel::Error)
        .set_title(title)
        .set_text(text)
        .alert()
        .show();
}
