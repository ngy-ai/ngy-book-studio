//! Shared constants, the Python highlighter registration and small text
//! helpers.

use super::*;

pub(crate) const MAX_VISIBLE_TRACES: usize = 1_000;
pub(crate) const CODE_LANGUAGE: &str = "python";

pub(crate) fn register_python_highlighting() {
    use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        // The component's default language bundle only contains JSON. Register
        // Python explicitly before editors or Markdown code blocks create a parser.
        LanguageRegistry::singleton().register(
            CODE_LANGUAGE,
            &LanguageConfig::new(
                CODE_LANGUAGE,
                tree_sitter_python::LANGUAGE.into(),
                vec![],
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
    });
}

// The TextView anchors its selection geometry to the text it paints. Its
// internal virtual list scrolls text inside fixed bounds, so repainting would
// select different characters after a wheel event. Scroll the whole, naturally
// sized TextView instead: the text and the selection then move together.
#[inline(never)]
pub(crate) fn scrollable_learning_text(
    id: impl Into<SharedString>,
    markdown: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::AnyElement {
    let id = id.into();
    let scroll_handle = window
        .use_keyed_state(SharedString::from(format!("{id}/scroll")), cx, |_, _| {
            gpui_kit::ScrollHandle::default()
        })
        .read(cx)
        .clone();
    div()
        .id(SharedString::from(format!("{id}/viewport")))
        .size_full()
        .relative()
        .child(
            div()
                .id("text-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .child(
                    TextView::markdown(id, markdown)
                        .selectable(true)
                        .scrollable(false)
                        .w_full()
                        .h_auto()
                        .flex_shrink_0(),
                ),
        )
        .vertical_scrollbar(&scroll_handle)
        .into_any_element()
}

pub(crate) const HELP_LEVELS: [(&str, &str); 5] = [
    ("H0", "H0 · 独立尝试"),
    ("H1", "H1 · 定位提示"),
    ("H2", "H2 · 结构提示"),
    ("H3", "H3 · 局部示范"),
    ("S", "S · 完整示范 / 代写"),
];
