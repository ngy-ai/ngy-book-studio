//! The cover panel of the editor sidebar.

use super::*;
use gpui::AnyElement;

/// Renders the cover thumbnail, its status note and the change/clear actions.
pub(crate) fn editor_cover_panel(
    app: &EditorApp,
    change_cover_view: Entity<EditorApp>,
    clear_cover_view: Entity<EditorApp>,
    media_modal_open: bool,
    write_busy: bool,
    cx: &mut Context<EditorApp>,
) -> AnyElement {
    let cover_thumbnail = if let Some(image) = app.cover_preview.as_ref() {
        img(image.clone())
            .size_full()
            .rounded(px(7.))
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        div()
            .v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded(px(7.))
            .bg(rgb(palette(cx).sidebar))
            .text_color(rgb(palette(cx).accent))
            .child(Icon::new(IconName::BookOpen).small())
            .child(
                div().text_xs().font_medium().child(
                    app.canonical_document
                        .as_ref()
                        .map(editor_book_format_label)
                        .unwrap_or("图书"),
                ),
            )
            .into_any_element()
    };
    let cover_status = if app.cover_loading {
        "正在读取…"
    } else if app.cover_dirty && app.cover.is_none() {
        "封面已清除 · 待保存"
    } else if app.cover_dirty {
        "新封面 · 待保存"
    } else if app.cover.is_some() {
        "当前封面"
    } else {
        "暂无封面"
    };
    let change_cover_disabled = app.cover_loading
        || app.canonical_document.is_none()
        || app.media_loading
        || media_modal_open
        || write_busy
        || app.webview_build_gate.building
        || app.pending_snapshot.is_some()
        || app.pending_ready_action.is_some()
        || app.closing;
    let clear_cover_enabled =
        editor_clear_cover_enabled(app.cover.is_some(), change_cover_disabled);
    render_editor_section(|| {
        div()
            .h_flex()
            .min_h(px(104.))
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .w(px(56.))
                    .h(px(76.))
                    .flex_none()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(palette(cx).border))
                    .bg(white())
                    .child(cover_thumbnail),
            )
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .flex_1()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if app.cover_dirty {
                                rgb(palette(cx).accent)
                            } else {
                                rgb(palette(cx).muted)
                            })
                            .child(cover_status),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("editor-change-cover")
                                    .small()
                                    .outline()
                                    .label(if app.cover_loading {
                                        "读取中…"
                                    } else if app.cover.is_some() {
                                        "更换封面"
                                    } else {
                                        "选择封面"
                                    })
                                    .disabled(change_cover_disabled)
                                    .on_click(move |_, window, cx| {
                                        change_cover_view
                                            .update(cx, |this, cx| this.change_cover(window, cx));
                                    }),
                            )
                            .child(
                                Button::new("editor-clear-cover")
                                    .small()
                                    .outline()
                                    .label("清除封面")
                                    .disabled(!clear_cover_enabled)
                                    .on_click(move |_, _, cx| {
                                        clear_cover_view
                                            .update(cx, |this, cx| this.clear_cover(cx));
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    })
}
