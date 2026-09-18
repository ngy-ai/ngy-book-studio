//! Shared input, notice and card helpers.

use super::*;

/// 输入框行的统一样式：标签、输入框、说明文字。
pub(crate) fn render_input_field(
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

pub(crate) fn render_notice(notice: &SettingsNotice, theme: Palette) -> gpui::AnyElement {
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
pub(crate) fn panel(theme: Palette, children: Vec<gpui::AnyElement>) -> gpui::AnyElement {
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
pub(crate) fn card_title(theme: Palette, text: impl Into<SharedString>) -> gpui::AnyElement {
    div()
        .text_sm()
        .font_semibold()
        .text_color(rgb(theme.ink))
        .child(text.into())
        .into_any_element()
}

/// 面板里的说明文字。
pub(crate) fn card_note(theme: Palette, text: impl Into<SharedString>) -> gpui::AnyElement {
    div()
        .text_xs()
        .line_height(gpui::relative(1.5))
        .text_color(rgb(theme.muted))
        .child(text.into())
        .into_any_element()
}

pub(crate) fn api_key_update_for_input(value: &str, delete: bool) -> ApiKeyUpdate {
    if delete {
        ApiKeyUpdate::Delete
    } else if value.trim().is_empty() {
        ApiKeyUpdate::Keep
    } else {
        ApiKeyUpdate::Set(value.to_string())
    }
}
