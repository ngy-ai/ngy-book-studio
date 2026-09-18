//! Fixtures for the table-of-contents navigation tests.

use super::*;

pub(crate) const TITLE_TOC_LABEL: &str = "书名页：人工智能代理系统的设计、实现与实践指南";
pub(crate) const INTRO_TOC_LABEL: &str = "引言：从阅读实例到构建可以验证的完整应用";

pub(crate) fn prepare_toc_document(document: &mut BookDocument) {
    // The linear sequence includes a cover and a navigation document that the
    // actual TOC omits. Its row ordinals must never become unit ordinals.
    document.units = [
        ("unit-cover", "封面内部标题", "只有封面内容"),
        ("unit-title", "书名页内部标题", "书名页正确正文"),
        ("unit-nav", "导航文件内部标题", "仅用于导航的文件内容"),
        ("unit-intro", "引言内部标题", "引言正确正文"),
        ("unit-chapter", "第一章内部标题", "第一章正确正文"),
    ]
    .into_iter()
    .map(|(id, title, body)| {
        let source = format!("<h1>{title}</h1><p>{body}</p>");
        let parsed = parse_source_for_unit(&source, id).unwrap();
        DocumentUnit::new(
            id,
            ContentUnitKind::Chapter,
            title,
            parsed.canonical_source,
            parsed.document,
        )
        .with_source_locator(SourceLocator::created())
    })
    .collect();
    let mut introduction =
        TocNode::new("toc-intro", INTRO_TOC_LABEL, TocTarget::unit("unit-intro"));
    introduction.children = vec![
        TocNode::new(
            "toc-intro-a",
            "引言第一节：为什么需要工具调用",
            TocTarget::unit("unit-intro"),
        ),
        TocNode::new(
            "toc-intro-b",
            "引言第二节：如何验证运行结果",
            TocTarget::unit("unit-intro"),
        ),
    ];
    document.toc = vec![
        TocNode::new("toc-title", TITLE_TOC_LABEL, TocTarget::unit("unit-title")),
        introduction,
        TocNode::new(
            "toc-chapter",
            "第一章：一条完整的执行路径",
            TocTarget::unit("unit-chapter"),
        ),
    ];
}

pub(crate) fn redraw_editor(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear());
    visual.run_until_parked();
}

pub(crate) fn click_toc_button(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .expect("the actual TOC node button must be rendered");
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    visual.simulate_click(bounds.center(), Modifiers::none());
    redraw_editor(visual);
}

pub(crate) fn select_toc_node(
    fixture: &EditorFixture,
    visual: &mut VisualTestContext,
    node_id: &str,
) {
    visual.update(|window, cx| {
        fixture.editor.update(cx, |editor, cx| {
            editor.select_toc(node_id, window, cx);
        });
    });
    visual.run_until_parked();
}

pub(crate) fn assert_toc_selection(
    fixture: &EditorFixture,
    visual: &VisualTestContext,
    node_id: &str,
    unit_id: &str,
    unit_index: usize,
    body: &str,
) {
    fixture.editor.read_with(visual, |editor, cx| {
        assert_eq!(editor.selected_toc_id.as_deref(), Some(node_id));
        assert_eq!(editor.current_toc_id().as_deref(), Some(node_id));
        assert_eq!(editor.selected, unit_index);
        assert_eq!(editor.unit_states[editor.selected].id, unit_id);
        assert!(editor.body_input.read(cx).value().contains(body));
    });
}
