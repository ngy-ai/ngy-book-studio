//! The resizable editor shell and the appearance script injected into both
//! WebViews.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorResizablePane {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub(crate) struct EditorPaneResizeDrag(pub(crate) EditorResizablePane);

pub(crate) struct EditorPaneResizePreview;

impl Render for EditorPaneResizePreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub(crate) fn constrained_editor_pane_width(
    requested: Pixels,
    minimum: Pixels,
    maximum: Pixels,
    viewport_width: Pixels,
    opposite_width: Pixels,
) -> Pixels {
    let reserved =
        opposite_width + px(EDITOR_CONTENT_MIN_WIDTH) + px(EDITOR_RESIZE_HANDLE_WIDTH * 2.);
    let available_maximum = (viewport_width - reserved).max(minimum);
    requested.clamp(minimum, maximum.min(available_maximum))
}

pub(crate) fn editor_pane_width_from_pointer(
    pane: EditorResizablePane,
    pointer_x: Pixels,
    viewport_width: Pixels,
) -> Pixels {
    let half_handle = px(EDITOR_RESIZE_HANDLE_WIDTH / 2.);
    match pane {
        EditorResizablePane::Left => pointer_x - half_handle,
        EditorResizablePane::Right => viewport_width - pointer_x - half_handle,
    }
}

// The editor bundle is built and pinned under `web/editor`. Cargo consumes the
// checked-in artifact so normal Rust builds never require Node.js.
pub(crate) const EDITOR_INITIALIZATION_SCRIPT: &str =
    include_str!("../../../assets/editor/prosemirror.js");

/// 编辑器 WebView 的外观覆盖规则。
///
/// 编辑器 bundle（`assets/editor/prosemirror.js`）随包发布，里面的工具栏、按钮、
/// 表格线与代码块底色都是写死的浅色，改不了它的源码就不能靠它换皮肤。宿主因此
/// 再注入一层覆盖：规则本身只写一次，颜色全部取自 `--ngy-*` 调色板变量，两套皮肤
/// 自动各取各的值。
///
/// 选择器统一加 `html[data-ngy-appearance]` 前缀，特异性从 bundle 的 (0,1,0) 提到
/// (0,2,1)：不用 `!important` 也能稳定覆盖，且不会连累 bundle 里其它规则。
pub(crate) const EDITOR_APPEARANCE_RULES: &str = r#"
html[data-ngy-appearance] body[data-ngy-rich-editor="true"] { background: var(--ngy-paper); color: var(--ngy-text); caret-color: var(--ngy-accent); }
html[data-ngy-appearance] [data-ngy-editor-ui="toolbar"] { border-color: var(--ngy-border); background: var(--ngy-surface); color: var(--ngy-text); }
html[data-ngy-appearance] [data-ngy-editor-ui="toolbar"] button { border-color: var(--ngy-border); background: var(--ngy-surface); color: var(--ngy-text); }
html[data-ngy-appearance] [data-ngy-editor-ui="toolbar"] button:hover { background: var(--ngy-accent-soft); border-color: var(--ngy-accent); }
html[data-ngy-appearance] [data-ngy-editor-ui="surface"] .ProseMirror-selectednode { outline-color: var(--ngy-accent); }
html[data-ngy-appearance] [data-ngy-editor-ui="surface"] th,
html[data-ngy-appearance] [data-ngy-editor-ui="surface"] td { border-color: var(--ngy-border); }
html[data-ngy-appearance] [data-ngy-editor-ui="surface"] .column-resize-handle { background: var(--ngy-accent); }
html[data-ngy-appearance] [data-ngy-editor-ui="surface"] pre[data-ngy-raw-html] { background: var(--ngy-sidebar); border-left-color: var(--ngy-accent); }
"#;

/// 把当前皮肤写进编辑器文档的脚本。
///
/// 与阅读器同理，但编辑器多一层：bundle 的样式表要覆盖，所以还要注入一个
/// `<style id="ngy-editor-appearance">`。样式表内容用 JSON 序列化后塞进 JS 字符串，
/// 免得 CSS 里的引号和换行把脚本写坏。
pub(crate) fn editor_appearance_script(appearance: Appearance) -> String {
    let palette = Palette::for_appearance(appearance);
    let setters = palette.css_variable_setters();
    let rules = serde_json::to_string(EDITOR_APPEARANCE_RULES).unwrap_or_else(|_| "\"\"".into());
    let scheme = appearance.color_scheme();
    format!(
        "(() => {{ const root = document.documentElement; \
         root.setAttribute('data-ngy-appearance', '{scheme}'); \
         root.style.colorScheme = '{scheme}'; \
         {setters}\
         let style = document.getElementById('ngy-editor-appearance'); \
         if (!style) {{ style = document.createElement('style'); \
         style.id = 'ngy-editor-appearance'; \
         (document.head || document.documentElement).appendChild(style); }} \
         style.textContent = {rules}; }})()"
    )
}
