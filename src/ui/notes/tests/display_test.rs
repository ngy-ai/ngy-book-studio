//! Tests for note display, markdown flattening and table rendering.

use super::super::*;
use super::*;

#[test]
fn stale_notes_keep_complete_original_text_and_never_gain_a_chapter() {
    let mut old_note = note(0, "书", AnnotationKind::AiComment);
    old_note.annotation.stale = true;
    old_note.chapter_index = None;
    old_note.chapter_title = None;
    let thought = format!(
        "<script>alert('x')</script>\r\n{}\n末尾",
        "长想法".repeat(12_000)
    );
    old_note.annotation.comment = Some(thought.clone());
    let copied = copy_text(&old_note);
    assert!(copied.ends_with(&thought));
    assert_eq!(chapter_label(&old_note), "章节已移除");
    let mut listing = NoteListing::default();
    listing.replace(vec![old_note]);
    let display = &listing.cards[0].comment_markdown.as_ref().unwrap().markdown;
    assert!(display.starts_with("&#60;script&#62;"));
    assert!(!display.contains("<script>"));
    assert!(display.ends_with("末尾"));
    assert_eq!(display.matches("长想法").count(), 12_000);
    assert!(listing.notes[0].annotation.stale);
    assert!(listing.notes[0].chapter_index.is_none());
}

#[test]
fn plain_text_display_escapes_active_content_and_preserves_line_boundaries() {
    assert_eq!(
        plain_text_html("<img src='https://x'>&\"\r\n一\r二\n三"),
        "<p>&lt;img src=&#39;https://x&#39;&gt;&amp;&quot;</p><p>一</p><p>二</p><p>三</p>"
    );
}

const THOUGHT_MARKDOWN: &str = "# 想法标题\n\n**重点**、*强调*与 `inline_code`。\n\n> 引用理解\n\n- 第一项\n- 第二项\n\n1. 步骤一\n2. 步骤二\n\n```python\nprint('code_value')\n```\n\n| 字段 | 含义 |\n| --- | --- |\n| source | 证据 |\n\n[链接标签](https://example.invalid/hidden)\n\n![图片说明](https://example.invalid/image)\n\n<img src='https://example.invalid/raw'>\n";

#[test]
fn human_and_ai_thoughts_share_safe_markdown_and_keep_original_copy_and_search() {
    use ::markdown::{ParseOptions, mdast::Node, to_mdast};

    let mut listing = NoteListing::default();
    let notes = [AnnotationKind::HumanComment, AnnotationKind::AiComment]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut note = note(index, "书", kind);
            note.annotation.anchor.quote = "**引文保留标点**".into();
            note.annotation.comment = Some(THOUGHT_MARKDOWN.into());
            note
        })
        .collect();
    listing.replace(notes);
    assert_eq!(listing.cards.len(), 2);
    assert_eq!(
        listing.cards[0].comment_markdown,
        listing.cards[1].comment_markdown
    );
    for card in &listing.cards {
        let note = &listing.notes[card.index];
        assert_eq!(note.annotation.comment.as_deref(), Some(THOUGHT_MARKDOWN));
        assert!(copy_text(note).ends_with(THOUGHT_MARKDOWN));
        assert!(card.quote_html.contains("**引文保留标点**"));
        let display = &card.comment_markdown.as_ref().unwrap().markdown;
        let tree = to_mdast(display, &ParseOptions::gfm()).unwrap();
        let mut structures = HashSet::new();
        let mut pending = vec![&tree];
        while let Some(node) = pending.pop() {
            let structure = match node {
                Node::Heading(_) => Some("heading"),
                Node::Strong(_) => Some("strong"),
                Node::Emphasis(_) => Some("emphasis"),
                Node::InlineCode(_) => Some("inline-code"),
                Node::List(_) => Some("list"),
                Node::Blockquote(_) => Some("quote"),
                Node::Code(_) => Some("code"),
                Node::Table(_) => Some("table"),
                Node::Html(_)
                | Node::Image(_)
                | Node::ImageReference(_)
                | Node::Link(_)
                | Node::LinkReference(_) => panic!("active content: {node:?}"),
                _ => None,
            };
            if let Some(structure) = structure {
                structures.insert(structure);
            }
            if let Some(children) = node.children() {
                pending.extend(children);
            }
        }
        for structure in [
            "heading",
            "strong",
            "emphasis",
            "inline-code",
            "list",
            "quote",
            "code",
            "table",
        ] {
            assert!(
                structures.contains(structure),
                "missing {structure}: {display}"
            );
        }
        assert!(display.contains("链接标签"));
        assert!(display.contains("图片说明"));
        assert!(!display.contains("https://example.invalid/hidden"));
        assert!(!display.contains("https://example.invalid/image"));
    }
    // Search still finds the original Markdown, including a destination
    // deliberately omitted from the inert presentation.
    listing.query = "https://example.invalid/hidden".into();
    listing.apply_filter();
    assert_eq!(listing.cards.len(), 2);
}

#[test]
fn table_segments_keep_all_cell_text_inline_markdown_and_neighboring_paragraphs() {
    let long_cell = format!(
        "**重点**、*强调*、`inline_code` {} CELL_END",
        "完整文字 ".repeat(200)
    );
    let source = format!(
        "开始 **说明**\n\n| 字段 | 详情 |\n| --- | --- |\n| source \\| suffix | {long_cell} |\n\n结束说明"
    );
    let display = ThoughtDisplay::new(assistant_display_markdown(&source));
    assert_eq!(display.parts.len(), 3);
    assert!(
        matches!(&display.parts[0], ThoughtPart::Markdown(text) if text.contains("开始 **说明**"))
    );
    let ThoughtPart::Table { rows, nested } = &display.parts[1] else {
        panic!("top-level table must use native cells");
    };
    assert!(!nested);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][0].to_string(), "字段");
    assert_eq!(rows[1][0].to_string(), "source \\| suffix");
    assert_eq!(rows[1][1].to_string(), long_cell);
    assert!(matches!(&display.parts[2], ThoughtPart::Markdown(text) if text.contains("结束说明")));
    // The normal Markdown renderer never receives this table, so its
    // built-in cell truncation cannot hide the saved tail.
    assert!(display.parts.iter().all(|part| match part {
        ThoughtPart::Markdown(text) => !text.contains("CELL_END"),
        ThoughtPart::Table { .. } => true,
    }));
}

#[test]
fn nested_tables_get_a_complete_native_view_and_preserve_surrounding_markdown() {
    let source = "> 引用说明\n>\n> | 名称 | 内容 |\n> | --- | --- |\n> | 条目 | **粗体** 与 `code` NESTED_END |\n\n后续正文";
    let display = ThoughtDisplay::new(assistant_display_markdown(source));
    let tables = display
        .parts
        .iter()
        .filter_map(|part| match part {
            ThoughtPart::Table { rows, nested } => Some((rows, nested)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 1);
    assert!(*tables[0].1);
    assert_eq!(
        tables[0].0[1][1].to_string(),
        "**粗体** 与 `code` NESTED_END"
    );
    assert!(
        display
            .parts
            .iter()
            .any(|part| matches!(part, ThoughtPart::Markdown(text) if text.contains("后续正文")))
    );
}

struct TextProbe;

impl Render for TextProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("notes-text-probe")
            .debug_selector(|| "notes-text-probe".into())
            .w(px(420.))
            .h(px(180.))
            .text_size(px(16.))
            .child(selectable_note_text(
                "notes-text-selection".into(),
                plain_text_html("**THOUGHT**\n<img src='https://example.invalid/a'>").into(),
                px(400.),
                window,
                cx,
            ))
    }
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

#[gpui::test]
fn displayed_quote_is_selectable_plain_text_including_markdown_and_html(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let text = cx.new(|_| TextProbe);
        Root::new(text, window, cx)
    });
    redraw(visual);
    let bounds = visual.debug_bounds("notes-text-probe").unwrap();
    let start = bounds.origin + point(px(1.), px(1.));
    let end = bounds.origin + point(px(395.), px(170.));
    visual.simulate_mouse_move(start, None, Modifiers::none());
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    redraw(visual);
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    redraw(visual);
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(visual);
    visual.write_to_clipboard(ClipboardItem::new_string("unhandled-copy".into()));
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-c");
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-c");
    let copied = visual.read_from_clipboard().unwrap().text().unwrap();
    assert!(copied.contains("**THOUGHT**"), "{copied:?}");
    assert!(
        copied.contains("<img src='https://example.invalid/a'>"),
        "{copied:?}"
    );
}

struct ThoughtProbe {
    markdown: Arc<ThoughtDisplay>,
}

impl Render for ThoughtProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("notes-thought-probe")
            .debug_selector(|| "notes-thought-probe".into())
            .w(px(480.))
            .h(px(440.))
            .text_size(px(16.))
            .child(render_note_thought(
                "notes-thought-selection".into(),
                self.markdown.clone(),
                px(460.),
                window,
                cx,
            ))
    }
}

#[gpui::test]
fn human_and_ai_markdown_display_selection_reads_formatted_text(cx: &mut TestAppContext) {
    const SOURCE: &str = "# TITLE\n\n**BOLD**\n\n- FIRST_ITEM\n- SECOND_ITEM\n\n[LINK_LABEL](https://example.invalid/link)\n\n![IMAGE_LABEL](https://example.invalid/image)\n\n<img src='https://example.invalid/raw'>";
    cx.update(gpui_component::init);
    for kind in [AnnotationKind::HumanComment, AnnotationKind::AiComment] {
        let mut note = note(0, "书", kind);
        note.annotation.comment = Some(SOURCE.into());
        let mut listing = NoteListing::default();
        listing.replace(vec![note]);
        let markdown = listing.cards[0].comment_markdown.clone().unwrap();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let text = cx.new(|_| ThoughtProbe { markdown });
            Root::new(text, window, cx)
        });
        redraw(visual);
        let bounds = visual.debug_bounds("notes-thought-probe").unwrap();
        let start = bounds.origin + point(px(1.), px(1.));
        let end = bounds.origin + point(px(450.), px(430.));
        visual.simulate_mouse_move(start, None, Modifiers::none());
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.write_to_clipboard(ClipboardItem::new_string("unhandled-copy".into()));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-c");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-c");
        let copied = visual.read_from_clipboard().unwrap().text().unwrap();
        for visible in [
            "TITLE",
            "BOLD",
            "FIRST_ITEM",
            "SECOND_ITEM",
            "LINK_LABEL",
            "IMAGE_LABEL",
            "<img src='https://example.invalid/raw'>",
        ] {
            assert!(
                copied.contains(visible),
                "{kind:?} missing {visible}: {copied:?}"
            );
        }
        assert!(!copied.contains("# TITLE"), "{copied:?}");
        assert!(!copied.contains("**BOLD**"), "{copied:?}");
        assert!(
            !copied.contains("https://example.invalid/link"),
            "{copied:?}"
        );
        assert!(
            !copied.contains("https://example.invalid/image"),
            "{copied:?}"
        );
        assert!(copy_text(&listing.notes[0]).ends_with(SOURCE));
    }
}

struct TableProbe {
    display: Arc<ThoughtDisplay>,
}

impl Render for TableProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("notes-table-probe")
            .debug_selector(|| "notes-table-probe".into())
            .w(px(400.))
            .h(px(680.))
            .text_size(px(16.))
            .child(render_note_thought(
                "notes-table-view".into(),
                self.display.clone(),
                px(400.),
                window,
                cx,
            ))
    }
}

#[gpui::test]
fn native_table_wraps_long_cells_and_scrolls_to_later_columns(cx: &mut TestAppContext) {
    let long_cell = format!(
        "**BOLD** *EMPHASIS* `CODE` {} TABLE_END",
        "Every word remains readable. ".repeat(8)
    );
    let source = format!(
        "| First | Second | Third | Last |\n| --- | --- | --- | --- |\n| {long_cell} | Two | Three | LAST_COLUMN |"
    );
    let display = Arc::new(ThoughtDisplay::new(assistant_display_markdown(&source)));
    cx.update(gpui_component::init);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let table = cx.new(|_| TableProbe { display });
        Root::new(table, window, cx)
    });
    redraw(visual);
    let cell = visual
        .debug_bounds("notes-table-view-part-0-cell-1-0")
        .unwrap();
    assert!(cell.size.height > px(100.), "long cell must wrap: {cell:?}");
    assert!(cell.bottom() < px(680.), "{cell:?}");
    let start = cell.origin + point(px(13.), px(13.));
    let end = point(cell.right() - px(14.), cell.bottom() - px(13.));
    visual.simulate_mouse_move(start, None, Modifiers::none());
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    redraw(visual);
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    redraw(visual);
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(visual);
    visual.write_to_clipboard(ClipboardItem::new_string("unhandled-copy".into()));
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-c");
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-c");
    let copied = visual.read_from_clipboard().unwrap().text().unwrap();
    for expected in ["BOLD", "EMPHASIS", "CODE", "TABLE_END"] {
        assert!(copied.contains(expected), "lost {expected}: {copied:?}");
    }
    assert!(!copied.contains("**BOLD**"));
    let last_before = visual
        .debug_bounds("notes-table-view-part-0-cell-1-3")
        .unwrap();
    assert!(last_before.left() >= px(400.));
    let position = point(px(200.), cell.top() + px(20.));
    visual.simulate_mouse_move(position, None, Modifiers::none());
    visual.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(-1_000.), px(0.))),
        ..Default::default()
    });
    redraw(visual);
    let last_after = visual
        .debug_bounds("notes-table-view-part-0-cell-1-3")
        .unwrap();
    assert!(
        last_after.left() < last_before.left(),
        "{last_before:?} -> {last_after:?}"
    );
    assert!(
        last_after.right() <= px(400.),
        "last column must be reachable: {last_after:?}"
    );
}
