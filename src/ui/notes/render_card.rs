//! One note card.

use super::*;

impl NotesWindow {
    #[inline(never)]
    pub(super) fn render_card(
        &self,
        card: &NoteCard,
        width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let note = &self.listing.notes[card.index];
        let annotation = &note.annotation;
        let id = annotation.id.clone();
        let locate_id = id.clone();
        let locate_view = cx.entity();
        let copy_id = id.clone();
        let view = cx.entity();
        let mut content = div().v_flex().gap_3().child(
            div()
                .border_l_2()
                .border_color(rgb(palette(cx).accent_soft))
                .pl_3()
                .text_color(rgb(palette(cx).muted))
                .child(selectable_note_text(
                    format!("note-{id}-quote"),
                    card.quote_html.clone(),
                    (width - px(14.)).max(px(0.)),
                    window,
                    cx,
                )),
        );
        if let Some(comment_markdown) = &card.comment_markdown {
            content = content.child(render_note_thought(
                format!("note-{id}-thought-markdown"),
                comment_markdown.clone(),
                width,
                window,
                cx,
            ));
        }
        div()
            .id(SharedString::from(format!("note-card-{id}")))
            .v_flex()
            .flex_shrink_0()
            .gap_3()
            .p_4()
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).surface))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_1()
                            .child(
                                div()
                                    .font_semibold()
                                    .child(format!("《{}》", note.book_title)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(chapter_label(note)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .px_2()
                            .py_1()
                            .rounded(px(5.))
                            .bg(rgb(palette(cx).accent_soft))
                            .text_color(rgb(palette(cx).accent_strong))
                            .child(kind_label(annotation.kind)),
                    ),
            )
            .child(content)
            .when(annotation.stale, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child("笔记位置已失效，保留的引文和想法仍可查看、复制。"),
                )
            })
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("note-copy-{id}")))
                            .small()
                            .ghost()
                            .icon(IconName::Copy)
                            .label(if self.copied_note.as_ref() == Some(&id) {
                                "已复制"
                            } else {
                                "复制笔记"
                            })
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if let Some(note) = this
                                        .listing
                                        .notes
                                        .iter()
                                        .find(|note| note.annotation.id == copy_id)
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_text(note),
                                        ));
                                        this.copied_note = Some(copy_id.clone());
                                        cx.notify();
                                    }
                                });
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("note-open-{id}")))
                            .small()
                            .outline()
                            .label(if self.opening_note.as_ref() == Some(&id) {
                                "正在打开…"
                            } else {
                                "打开章节"
                            })
                            .disabled(
                                annotation.stale
                                    || note.chapter_index.is_none()
                                    || self.opening_note.is_some(),
                            )
                            .on_click(move |_, window, cx| {
                                locate_view.update(cx, |this, cx| {
                                    this.open_annotation(&locate_id, window, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    }
}
