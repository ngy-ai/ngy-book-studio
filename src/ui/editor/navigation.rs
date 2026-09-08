use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditorTocRow {
    pub id: String,
    pub label: String,
    pub unit_id: String,
    pub depth: usize,
    pub parent_id: Option<String>,
}

/// Preserve the independent navigation tree, including multiple destinations
/// within one content unit and labels that differ from the unit title.
pub(super) fn editor_toc_rows(nodes: &[TocNode]) -> Vec<EditorTocRow> {
    fn append_rows(
        nodes: &[TocNode],
        depth: usize,
        parent_id: Option<&str>,
        rows: &mut Vec<EditorTocRow>,
    ) {
        for node in nodes {
            rows.push(EditorTocRow {
                id: node.id.clone(),
                label: node.label.clone(),
                unit_id: node.target.unit_id().to_string(),
                depth,
                parent_id: parent_id.map(str::to_string),
            });
            append_rows(&node.children, depth + 1, Some(&node.id), rows);
        }
    }

    let mut rows = Vec::new();
    append_rows(nodes, 0, None, &mut rows);
    rows
}

/// Indenting appends the selected node to its previous sibling's children.
pub(super) fn toc_indent_destination(nodes: &[TocNode], id: &str) -> Option<(String, usize)> {
    if let Some(index) = nodes.iter().position(|node| node.id == id) {
        let previous = nodes.get(index.checked_sub(1)?)?;
        return Some((previous.id.clone(), previous.children.len()));
    }
    nodes
        .iter()
        .find_map(|node| toc_indent_destination(&node.children, id))
}

/// Outdenting moves exactly one level, immediately after the selected node's
/// parent. The destination remains nested when the parent has a parent.
pub(super) fn toc_outdent_destination(
    nodes: &[TocNode],
    id: &str,
) -> Option<(Option<String>, usize)> {
    fn find_destination(
        nodes: &[TocNode],
        id: &str,
        parent_id: Option<&str>,
    ) -> Option<(Option<String>, usize)> {
        for (index, node) in nodes.iter().enumerate() {
            if node.children.iter().any(|child| child.id == id) {
                return Some((parent_id.map(str::to_string), index + 1));
            }
            if let Some(destination) = find_destination(&node.children, id, Some(&node.id)) {
                return Some(destination);
            }
        }
        None
    }

    find_destination(nodes, id, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moye_epub_editor::document::{BlockDocument, ContentUnit, TocTarget};

    fn document_with_fifteen_units() -> BookDocument {
        let mut document = BookDocument::created("book-navigation", "目录与线性正文");
        for index in 0..15 {
            let unit_id = format!("unit-{index}");
            document.units.push(ContentUnit::new(
                &unit_id,
                ContentUnitKind::Chapter,
                format!("正文文件 {index}"),
                SourceKind::Markdown,
                "正文",
                BlockDocument::new(vec![Block::paragraph(format!("block-{index}"), "正文")]),
            ));
            let mut node = TocNode::new(
                format!("toc-{index}"),
                format!("第 {index} 章"),
                TocTarget::unit(&unit_id),
            );
            node.children.push(TocNode::new(
                format!("section-{index}"),
                "本章说明",
                TocTarget::block(unit_id, format!("block-{index}")),
            ));
            document.toc.push(node);
        }
        document.validate().unwrap();
        document
    }

    fn node(id: &str, children: Vec<TocNode>) -> TocNode {
        let mut node = TocNode::new(id, id, TocTarget::unit("unit-0"));
        node.children = children;
        node
    }

    fn branched_document() -> BookDocument {
        let mut document = document_with_fifteen_units();
        document.toc = vec![
            node(
                "chapter",
                vec![
                    node("before", vec![node("before-child", vec![])]),
                    node("selected", vec![node("selected-child", vec![])]),
                    node("after", vec![]),
                ],
            ),
            node("other-chapter", vec![node("other-child", vec![])]),
        ];
        document.validate().unwrap();
        document
    }

    #[test]
    fn fifteen_content_units_expose_all_thirty_navigation_entries() {
        let document = document_with_fifteen_units();
        let rows = editor_toc_rows(&document.toc);

        assert_eq!(document.units.len(), 15);
        assert_eq!(rows.len(), 30);
        for index in 0..15 {
            let chapter = &rows[index * 2];
            let section = &rows[index * 2 + 1];
            assert_eq!(chapter.id, format!("toc-{index}"));
            assert_eq!(chapter.label, format!("第 {index} 章"));
            assert_ne!(chapter.label, document.units[index].title);
            assert_eq!(chapter.unit_id, document.units[index].id);
            assert_eq!(chapter.depth, 0);
            assert_eq!(chapter.parent_id, None);
            assert_eq!(section.id, format!("section-{index}"));
            assert_eq!(section.unit_id, chapter.unit_id);
            assert_eq!(section.depth, 1);
            assert_eq!(section.parent_id.as_deref(), Some(chapter.id.as_str()));
        }
    }

    #[test]
    fn repeated_labels_and_unit_targets_keep_distinct_rows_in_tree_order() {
        let mut document = branched_document();
        document.toc[0].label = "同名目录".to_string();
        document.toc[0].children[0].label = "同名目录".to_string();
        document.toc[0].children[1].label = "同名目录".to_string();
        let rows = editor_toc_rows(&document.toc);

        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            [
                "chapter",
                "before",
                "before-child",
                "selected",
                "selected-child",
                "after",
                "other-chapter",
                "other-child",
            ]
        );
        assert!(rows.iter().all(|row| row.unit_id == "unit-0"));
        assert_eq!(rows.iter().filter(|row| row.label == "同名目录").count(), 3);
        assert_eq!(rows[4].depth, 2);
        assert_eq!(rows[4].parent_id.as_deref(), Some("selected"));
    }

    #[test]
    fn indent_moves_selected_section_and_preserves_other_branches_and_reading_order() {
        let document = branched_document();
        let mut expected_toc = document.toc.clone();
        let selected = expected_toc[0].children.remove(1);
        expected_toc[0].children[0].children.push(selected);
        let expected_units = document.units.clone();
        let mut editor = DocumentEditor::new(document).unwrap();
        let (parent, position) =
            toc_indent_destination(&editor.document().toc, "selected").unwrap();

        assert_eq!((parent.as_str(), position), ("before", 1));
        editor
            .move_toc_node("selected", Some(&parent), position)
            .unwrap();

        assert_eq!(editor.document().toc, expected_toc);
        assert_eq!(editor.document().units, expected_units);
    }

    #[test]
    fn outdent_of_nested_section_moves_one_level_after_parent_without_losing_children() {
        let document = branched_document();
        let mut expected_toc = document.toc.clone();
        let selected = expected_toc[0].children[1].children.remove(0);
        expected_toc[0].children.insert(2, selected);
        let expected_units = document.units.clone();
        let mut editor = DocumentEditor::new(document).unwrap();
        let (parent, position) =
            toc_outdent_destination(&editor.document().toc, "selected-child").unwrap();

        assert_eq!((parent.as_deref(), position), (Some("chapter"), 2));
        editor
            .move_toc_node("selected-child", parent.as_deref(), position)
            .unwrap();

        assert_eq!(editor.document().toc, expected_toc);
        assert_eq!(editor.document().units, expected_units);
    }

    #[test]
    fn outdent_of_section_places_subtree_after_its_root_parent() {
        let document = branched_document();
        let mut expected_toc = document.toc.clone();
        let selected = expected_toc[0].children.remove(1);
        expected_toc.insert(1, selected);
        let mut editor = DocumentEditor::new(document).unwrap();
        let (parent, position) =
            toc_outdent_destination(&editor.document().toc, "selected").unwrap();

        assert_eq!((parent.as_deref(), position), (None, 1));
        editor
            .move_toc_node("selected", parent.as_deref(), position)
            .unwrap();

        assert_eq!(editor.document().toc, expected_toc);
    }

    #[test]
    fn first_siblings_roots_and_unknown_ids_have_no_invalid_destination() {
        let document = branched_document();
        assert_eq!(toc_indent_destination(&document.toc, "chapter"), None);
        assert_eq!(toc_indent_destination(&document.toc, "before"), None);
        assert_eq!(toc_indent_destination(&document.toc, "before-child"), None);
        assert_eq!(toc_indent_destination(&document.toc, "unit-0"), None);
        assert_eq!(toc_outdent_destination(&document.toc, "chapter"), None);
        assert_eq!(
            toc_outdent_destination(&document.toc, "other-chapter"),
            None
        );
        assert_eq!(toc_outdent_destination(&document.toc, "unit-0"), None);
        assert!(editor_toc_rows(&[]).is_empty());
    }
}
