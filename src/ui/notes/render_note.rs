//! Rendering a note thought, including its tables.

use super::*;

#[inline(never)]
pub(crate) fn selectable_note_text(
    id: String,
    html: SharedString,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    TextView::html(SharedString::from(id), html, window, cx)
        .style(TextViewStyle::default().paragraph_gap(gpui::rems(0.4)))
        .selectable(true)
        .scrollable(false)
        .w(width)
        .h_auto()
        .flex_shrink_0()
}

#[inline(never)]
pub(crate) fn selectable_note_thought(
    id: String,
    markdown: SharedString,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    TextView::markdown(SharedString::from(id), markdown, window, cx)
        .style(TextViewStyle::default().paragraph_gap(gpui::rems(0.4)))
        .selectable(true)
        .scrollable(false)
        .w(width)
        .h_auto()
        .flex_shrink_0()
}

#[inline(never)]
pub(crate) fn render_note_thought(
    id: String,
    display: Arc<ThoughtDisplay>,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    if display.parts.is_empty() {
        return selectable_note_thought(id, display.markdown.clone(), width, window, cx)
            .into_any_element();
    }
    let mut body = div().v_flex().w(width).gap_2();
    for (index, part) in display.parts.iter().enumerate() {
        let part_id = format!("{id}-part-{index}");
        match part {
            ThoughtPart::Markdown(markdown) => {
                body = body.child(selectable_note_thought(
                    part_id,
                    markdown.clone(),
                    width,
                    window,
                    cx,
                ));
            }
            ThoughtPart::Table { rows, nested } => {
                if *nested {
                    body = body.child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette(cx).muted))
                            .child("完整表格"),
                    );
                }
                body = body.child(render_note_table(&part_id, rows, width, window, cx));
            }
        }
    }
    body.into_any_element()
}

#[inline(never)]
pub(crate) fn render_note_table(
    id: &str,
    rows: &[Vec<SharedString>],
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let column_width = ((width - px(2.)) / columns as f32).max(px(180.));
    let table_width = column_width * columns as f32;
    let scroll = window
        .use_keyed_state(
            SharedString::from(format!("{id}-scroll-state")),
            cx,
            |_, _| ScrollHandle::default(),
        )
        .read(cx)
        .clone();
    let mut table = div()
        .v_flex()
        .w(table_width + px(2.))
        .flex_shrink_0()
        .border_1()
        .border_color(rgb(palette(cx).border));
    for (row_index, row) in rows.iter().enumerate() {
        let mut line = div()
            .flex()
            .flex_row()
            .w(table_width)
            .flex_shrink_0()
            .when(row_index == 0, |line| {
                line.bg(rgb(palette(cx).sidebar)).font_semibold()
            })
            .when(row_index + 1 < rows.len(), |line| {
                line.border_b_1().border_color(rgb(palette(cx).border))
            });
        for column in 0..columns {
            let cell_id = format!("{id}-cell-{row_index}-{column}");
            let source = row.get(column).cloned().unwrap_or_else(|| "".into());
            let cell = div()
                .id(SharedString::from(cell_id.clone()))
                .debug_selector({
                    let cell_id = cell_id.clone();
                    move || cell_id.clone()
                })
                .w(column_width)
                .min_h(px(40.))
                .flex_shrink_0()
                .p_3()
                .when(column + 1 < columns, |cell| {
                    cell.border_r_1().border_color(rgb(palette(cx).border))
                })
                .child(selectable_note_thought(
                    format!("{cell_id}-text"),
                    source,
                    (column_width - px(26.)).max(px(0.)),
                    window,
                    cx,
                ));
            line = line.child(cell);
        }
        table = table.child(line);
    }
    // Keep the natural table height. The component's overflow_x_scrollbar()
    // wrapper uses size_full(), so attach the scrollbar to our own viewport.
    div()
        .relative()
        .w(width)
        .flex_shrink_0()
        .child(
            div()
                .id(SharedString::from(format!("{id}-viewport")))
                .w(width)
                .pb_3()
                .overflow_x_scroll()
                .track_scroll(&scroll)
                .child(table),
        )
        .horizontal_scrollbar(&scroll)
        .into_any_element()
}
