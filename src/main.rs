#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use gpui::{App, AppContext as _, Application, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component_assets::Assets;
use moye_epub_editor::{library::LibraryStore, services::AppServices};
use native_dialog::{DialogBuilder, MessageLevel};
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::EnvFilter;
use ui::{EpubReaderApp, wrap_root};

#[cfg(target_os = "windows")]
const WEBVIEW2_DOWNLOAD_URL: &str =
    "https://developer.microsoft.com/microsoft-edge/webview2/consumer/";

fn main() {
    // GPUI's DirectComposition renderer and a child HWND WebView cannot be layered
    // reliably on Windows. This is set before GPUI starts any worker threads.
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1");
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .try_init();

    #[cfg(target_os = "windows")]
    if !ensure_webview2_runtime() {
        return;
    }

    let services = match load_services() {
        Ok(services) => services,
        Err(error) => {
            show_fatal_error(
                "无法初始化墨页",
                format!("本地图书库加载失败：\n\n{error:#}"),
            );
            return;
        }
    };
    let library = match services.library_snapshot() {
        Ok(library) => library,
        Err(error) => {
            show_fatal_error(
                "无法初始化墨页",
                format!("无法读取图书库视图：\n\n{error:#}"),
            );
            return;
        }
    };
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let bounds = Bounds::centered(None, size(px(1260.), px(820.)), cx);
            let result = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(940.), px(640.))),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("墨页 · 私人图书馆".into()),
                        ..Default::default()
                    }),
                    app_id: Some("dev.moye.epub-editor".to_string()),
                    ..Default::default()
                },
                move |window, cx| {
                    let app =
                        cx.new(|cx| EpubReaderApp::new(library, Arc::clone(&services), window, cx));
                    cx.new(|cx| wrap_root(app, window, cx))
                },
            );
            if let Err(error) = result {
                show_fatal_error("无法启动墨页", format!("应用窗口创建失败：\n\n{error}"));
                cx.quit();
                return;
            }
            cx.activate(true);
        });
}

fn load_services() -> anyhow::Result<Arc<AppServices>> {
    let data_dir = app_data_dir()?;
    Ok(Arc::new(AppServices::open(data_dir)?))
}

fn app_data_dir() -> anyhow::Result<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(data_dir) = std::env::var_os("MOYE_DATA_DIR") {
        return Ok(data_dir.into());
    }
    LibraryStore::default_data_dir()
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

fn show_fatal_error(title: &str, text: String) {
    let _ = DialogBuilder::message()
        .set_level(MessageLevel::Error)
        .set_title(title)
        .set_text(text)
        .alert()
        .show();
}
