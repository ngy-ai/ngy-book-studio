//! The chapter / search-result list in the editor sidebar.

use super::*;
use crate::ui::editor::navigation::EditorTocRow;
use gpui::AnyElement;

/// Builds the rows of the chapter list, or the search results when searching.
pub(crate) fn editor_chapter_list(
    app: &EditorApp,
    view: &Entity<EditorApp>,
    toc_rows: &[EditorTocRow],
    selected_toc_id: Option<&str>,
    write_busy: bool,
    cx: &mut Context<EditorApp>,
) -> Vec<AnyElement> {
    let chapter_list = if app.search_query.is_empty() {
        let listed_units = toc_rows
            .iter()
            .map(|row| row.unit_id.as_str())
            .collect::<HashSet<_>>();
        let mut items = toc_rows
            .iter()
            .map(|row| {
                let selected = selected_toc_id.as_deref() == Some(row.id.as_str());
                let id = row.id.clone();
                let list_view = view.clone();
                Button::new(SharedString::from(format!("editor-toc-{}", row.id)))
                    .debug_selector(|| format!("editor-toc-{}", row.id))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .min_h(px(38.))
                    .flex_none()
                    .justify_start()
                    .pl(px(18. + row.depth.min(4) as f32 * 14.))
                    .pr_3()
                    .py_2()
                    .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                    .disabled(write_busy)
                    .on_click(move |_, window, cx| {
                        list_view.update(cx, |this, cx| this.select_toc(&id, window, cx));
                    })
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .text_left()
                            .text_sm()
                            .line_clamp(2)
                            .text_color(if selected {
                                rgb(palette(cx).accent)
                            } else {
                                rgb(palette(cx).ink)
                            })
                            .child(row.label.clone()),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let unlisted = app
            .unit_states
            .iter()
            .enumerate()
            .filter(|(_, unit)| !listed_units.contains(unit.id.as_str()))
            .collect::<Vec<_>>();
        if !toc_rows.is_empty() && !unlisted.is_empty() {
            items.push(
                div()
                    .px_3()
                    .py_2()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("未列入目录的章节")
                    .into_any_element(),
            );
        }
        for (index, _) in unlisted {
            let Some(chapter) = app.chapters.get(index) else {
                continue;
            };
            let selected = index == app.selected;
            let list_view = view.clone();
            items.push(
                Button::new(("editor-chapter", index))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .min_h(px(38.))
                    .flex_none()
                    .justify_start()
                    .px_3()
                    .py_2()
                    .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                    .disabled(write_busy)
                    .on_click(move |_, window, cx| {
                        list_view.update(cx, |this, cx| this.select_chapter(index, window, cx));
                    })
                    .child(
                        div()
                            .w_full()
                            .text_left()
                            .text_sm()
                            .line_clamp(2)
                            .child(chapter.title.clone()),
                    )
                    .into_any_element(),
            );
        }
        items
    } else {
        if app.search_results.is_empty() {
            vec![
                div()
                    .p_4()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(palette(cx).muted))
                    .child("当前草稿没有匹配章节")
                    .into_any_element(),
            ]
        } else {
            app.search_results
                .iter()
                .enumerate()
                .map(|(index, hit)| {
                    let selected = hit.chapter_index == app.selected;
                    let chapter_index = hit.chapter_index;
                    let result_view = view.clone();
                    Button::new(("editor-search-result", index))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(66.))
                        .justify_start()
                        .px_2()
                        .py_2()
                        .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                        .disabled(write_busy)
                        .on_click(move |_, window, cx| {
                            result_view.update(cx, |this, cx| {
                                this.select_chapter(chapter_index, window, cx)
                            });
                        })
                        .child(
                            div()
                                .v_flex()
                                .w_full()
                                .min_w(px(0.))
                                .gap_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_left()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(if selected {
                                            rgb(palette(cx).accent)
                                        } else {
                                            rgb(palette(cx).ink)
                                        })
                                        .child(hit.chapter_title.clone()),
                                )
                                .child(
                                    div()
                                        .line_clamp(2)
                                        .text_left()
                                        .text_xs()
                                        .text_color(rgb(palette(cx).muted))
                                        .child(hit.snippet.clone()),
                                ),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        }
    };

    chapter_list
}
