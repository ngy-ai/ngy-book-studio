//! Reusing an existing settings window or opening a new one.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExistingWindowDisposition<Handle> {
    OpenNew,
    SuppressReentrantOpen,
    Activate(Handle),
    ClearStale(Handle),
}

pub(crate) fn existing_window_disposition<Handle: Copy>(
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
pub(crate) struct SettingsWindowTracker<V: 'static> {
    pub(crate) opening: bool,
    pub(crate) closing: bool,
    pub(crate) handle: Option<gpui_kit::AnyWindowHandle>,
    pub(crate) view: Option<WeakEntity<V>>,
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
pub(crate) struct SettingsWindowTrackers {
    pub(crate) endpoint: SettingsWindowTracker<EndpointSettingsWindow>,
    pub(crate) models: SettingsWindowTracker<ModelsSettingsWindow>,
    pub(crate) web_search: SettingsWindowTracker<WebSearchSettingsWindow>,
    pub(crate) background_jobs: SettingsWindowTracker<BackgroundJobsSettingsWindow>,
    pub(crate) system: SettingsWindowTracker<SystemSettingsWindow>,
}

impl gpui_kit::Global for SettingsWindowTrackers {}

pub(crate) fn ensure_settings_window_trackers(cx: &mut App) {
    if !cx.has_global::<SettingsWindowTrackers>() {
        cx.set_global(SettingsWindowTrackers::default());
    }
}
