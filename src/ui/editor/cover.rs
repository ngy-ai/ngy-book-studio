//! The media metadata modal.

use super::*;

impl EditorApp {
    pub(crate) fn render_media_metadata_modal(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let modal = self.media_modal.clone()?;
        let view = cx.entity().clone();
        let kind = match modal.kind {
            MediaKind::Image => "图片",
            MediaKind::Audio => "音频",
            MediaKind::Video => "视频",
        };
        let (heading, confirm_label) = match modal.action {
            EditorMediaModalAction::InsertOrReplace {
                replace_block_id: Some(_),
                ..
            } => (format!("替换{kind}并编辑信息"), "替换"),
            EditorMediaModalAction::InsertOrReplace { .. } => {
                (format!("插入{kind}并编辑信息"), "插入")
            }
            EditorMediaModalAction::Edit { .. } => (format!("编辑{kind}信息"), "保存信息"),
        };
        let cancel_view = view.clone();
        let confirm_view = view.clone();
        Some(
            div()
                .id("editor-media-metadata-overlay")
                .absolute()
                .inset_0()
                .occlude()
                .bg(palette(cx).scrim_with_alpha(0.5))
                .v_flex()
                .items_center()
                .justify_center()
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .occlude()
                        .v_flex()
                        .w(px(460.))
                        .p_5()
                        .gap_4()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(rgb(palette(cx).surface))
                        .shadow_lg()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child(heading),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(palette(cx).ink))
                                        .child("标题（可选）"),
                                )
                                .child(Input::new(&modal.title_input)),
                        )
                        .child(
                            div()
                                .v_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(palette(cx).ink))
                                        .child("说明（可选）"),
                                )
                                .child(
                                    div()
                                        .h(px(120.))
                                        .child(Textarea::new(&modal.description_input).h_full()),
                                )
                                .child(div().text_xs().text_color(rgb(palette(cx).muted)).child(
                                    "图片说明会写入 alt 与 caption；所有说明都会参与全文检索。",
                                )),
                        )
                        .child(
                            div()
                                .h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("editor-media-metadata-cancel")
                                        .label("取消")
                                        .outline()
                                        .on_click(move |_, window, cx| {
                                            cancel_view.update(cx, |this, cx| {
                                                this.close_media_metadata_modal(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-media-metadata-confirm")
                                        .label(confirm_label)
                                        .primary()
                                        .on_click(move |_, window, cx| {
                                            confirm_view.update(cx, |this, cx| {
                                                this.confirm_media_metadata_modal(window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
