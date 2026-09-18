//! The rendered page window.

use super::*;

impl Render for PageImageApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let previous = cx.entity().clone();
        let next = cx.entity().clone();
        let zoom_in = cx.entity().clone();
        let zoom_out = cx.entity().clone();
        let reset_zoom = cx.entity().clone();
        let toggle_text = cx.entity().clone();
        let page = &self.pages[self.current];
        let (subtitle, subtitle_color) = self
            .notice
            .as_ref()
            .map(|notice| {
                (
                    notice.text.clone(),
                    if notice.error {
                        palette(cx).danger
                    } else {
                        palette(cx).muted
                    },
                )
            })
            .unwrap_or_else(|| (page_subtitle(page, self.kind), palette(cx).muted));
        let page_image = Arc::clone(&page.image);
        let (natural_width, natural_height) = (page.width, page.height);
        let page_number = self.current + 1;
        let page_count = self.pages.len();
        let zoom_milli = self.zoom_milli;
        let kind = self.kind;
        let text_panel_open = self.text_panel_open && kind == VisualPageSourceKind::Djvu;
        let page_text = self
            .page_texts
            .get(self.current)
            .and_then(|text| text.as_deref())
            .unwrap_or_default();
        let has_hidden_text = !page_text.trim().is_empty();

        let page_area: AnyElement = if zoom_milli == ZOOM_FIT {
            div()
                .id("page-image-fit")
                .size_full()
                .p_5()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .on_scroll_wheel(zoom_wheel_listener(cx.entity()))
                .child(img(page_image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element()
        } else {
            let (width, height) = zoomed_size(natural_width, natural_height, zoom_milli);
            let scroll = window
                .use_keyed_state(SharedString::from("page-image-zoom/scroll"), cx, |_, _| {
                    gpui::ScrollHandle::default()
                })
                .read(cx)
                .clone();
            div()
                .id("page-image-zoom")
                .size_full()
                .relative()
                .child(
                    div()
                        .id("page-image-zoom-scroll")
                        .size_full()
                        .overflow_x_scroll()
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .child(
                            // The listener sits inside the scroller on purpose:
                            // bubble handlers run from the innermost element
                            // outwards, so this one can stop a Ctrl + wheel
                            // notch before the scroller turns it into a scroll.
                            div()
                                .id("page-image-zoom-content")
                                .p_4()
                                .flex_shrink_0()
                                .on_scroll_wheel(zoom_wheel_listener(cx.entity()))
                                .child(img(page_image).w(px(width)).h(px(height))),
                        ),
                )
                .vertical_scrollbar(&scroll)
                .into_any_element()
        };

        let text_panel = text_panel_open.then(|| {
            div()
                .w(px(PAGE_TEXT_PANEL_WIDTH))
                .h_full()
                .flex_none()
                .v_flex()
                .border_l_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).surface))
                .child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(palette(cx).border))
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child("本页文字（可选中复制）"),
                        ),
                )
                .child(scrollable_page_text(
                    SharedString::from(format!("page-text-{}", self.current)),
                    SharedString::from(page_text_html(
                        &page_text_preview(page_text),
                        has_hidden_text,
                    )),
                    window,
                    cx,
                ))
        });

        div()
            .size_full()
            .v_flex()
            .bg(rgb(palette(cx).paper))
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .justify_between()
                    .gap_4()
                    .px_5()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(palette(cx).border))
                    .bg(rgb(palette(cx).surface))
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .font_semibold()
                                    .text_color(rgb(palette(cx).ink))
                                    .child(self.book_title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(subtitle_color))
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .flex_none()
                            .gap_2()
                            .when(kind == VisualPageSourceKind::Djvu, |bar| {
                                bar.child(
                                    Button::new("page-image-text-panel")
                                        .outline()
                                        .label(if text_panel_open {
                                            "隐藏文字"
                                        } else {
                                            "显示文字"
                                        })
                                        .disabled(!has_hidden_text)
                                        .on_click(move |_, _, cx| {
                                            toggle_text
                                                .update(cx, |this, cx| this.toggle_text_panel(cx));
                                        }),
                                )
                            })
                            .child(
                                Button::new("page-image-zoom-out")
                                    .outline()
                                    .label("缩小")
                                    .disabled(zoom_milli == ZOOM_FIT)
                                    .on_click(move |_, _, cx| {
                                        zoom_out.update(cx, |this, cx| this.zoom_out(cx));
                                    }),
                            )
                            .child(
                                div()
                                    .min_w(px(64.))
                                    .text_center()
                                    .text_sm()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(if zoom_milli == ZOOM_FIT {
                                        "适应窗口".to_string()
                                    } else {
                                        format!("{}%", zoom_milli / 10)
                                    }),
                            )
                            .child(
                                Button::new("page-image-zoom-in")
                                    .outline()
                                    .label("放大")
                                    .disabled(zoom_milli == ZOOM_MAX_MILLI)
                                    .on_click(move |_, _, cx| {
                                        zoom_in.update(cx, |this, cx| this.zoom_in(cx));
                                    }),
                            )
                            .child(
                                Button::new("page-image-zoom-fit")
                                    .outline()
                                    .label("适应窗口")
                                    .disabled(zoom_milli == ZOOM_FIT)
                                    .on_click(move |_, _, cx| {
                                        reset_zoom.update(cx, |this, cx| this.reset_zoom(cx));
                                    }),
                            )
                            .child(
                                Button::new("page-image-previous")
                                    .outline()
                                    .icon(IconName::ChevronLeft)
                                    .label("上一页")
                                    .disabled(self.current == 0)
                                    .on_click(move |_, _, cx| {
                                        previous.update(cx, |this, cx| this.previous(cx));
                                    }),
                            )
                            .child(
                                div()
                                    .min_w(px(86.))
                                    .text_center()
                                    .text_sm()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(format!("{page_number} / {page_count}")),
                            )
                            .child(
                                Button::new("page-image-next")
                                    .outline()
                                    .icon(IconName::ChevronRight)
                                    .label("下一页")
                                    .disabled(self.current + 1 >= self.pages.len())
                                    .on_click(move |_, _, cx| {
                                        next.update(cx, |this, cx| this.next(cx));
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .overflow_hidden()
                            .child(page_area),
                    )
                    .children(text_panel)
                    .child(self.ai_sidebar.clone()),
            )
    }
}
