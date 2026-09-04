//! Transactional, format-neutral editing operations for [`BookDocument`].
//!
//! Every public mutation is applied to a clone and validated before it becomes
//! visible. This keeps UI code from persisting half-applied TOC or unit edits.

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    document::{
        BookDocument, ContentUnit, ContentUnitKind, SourceKind, SourceLocator, TocNode, TocTarget,
        deterministic_id,
    },
    markup::parse_source_for_unit,
};

#[derive(Clone, Debug)]
pub struct NewContentUnit {
    pub title: String,
    pub kind: ContentUnitKind,
    pub source_kind: SourceKind,
    pub source: String,
    /// Optional TOC parent. `None` inserts at the root.
    pub toc_parent_id: Option<String>,
    pub toc_position: usize,
    pub linear_position: usize,
}

impl NewContentUnit {
    pub fn markdown_chapter(title: impl Into<String>, linear_position: usize) -> Self {
        let title = title.into();
        Self {
            source: format!("# {title}\n"),
            title,
            kind: ContentUnitKind::Chapter,
            source_kind: SourceKind::Markdown,
            toc_parent_id: None,
            toc_position: linear_position,
            linear_position,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DocumentEditor {
    document: BookDocument,
}

impl DocumentEditor {
    pub fn new(document: BookDocument) -> Result<Self> {
        document.validate().context("无法编辑无效图书")?;
        Ok(Self { document })
    }

    pub fn document(&self) -> &BookDocument {
        &self.document
    }

    pub fn into_document(self) -> BookDocument {
        self.document
    }

    pub fn set_metadata(
        &mut self,
        title: impl Into<String>,
        authors: Vec<String>,
        language: Option<String>,
        description: Option<String>,
    ) -> Result<()> {
        self.mutate(|candidate| {
            candidate.title = title.into().trim().to_string();
            candidate.authors = authors
                .into_iter()
                .map(|author| author.trim().to_string())
                .filter(|author| !author.is_empty())
                .collect();
            candidate.language = trim_option(language);
            candidate.description = trim_option(description);
            Ok(())
        })
    }

    pub fn add_unit(&mut self, request: NewContentUnit) -> Result<String> {
        let title = request.title.trim().to_string();
        ensure!(!title.is_empty(), "内容单元标题不能为空");
        let mut added_id = String::new();
        self.mutate(|candidate| {
            ensure!(
                request.linear_position <= candidate.units.len(),
                "线性内容插入位置越界"
            );
            let unit_id = unique_id(
                candidate,
                "unit",
                &format!(
                    "{}\0{}\0{}\0{}",
                    candidate.id,
                    candidate.revision.get(),
                    request.linear_position,
                    title
                ),
            );
            let parsed = parse_source_for_unit(request.source_kind, &request.source, &unit_id)?;
            let unit = ContentUnit::new(
                unit_id.clone(),
                request.kind,
                title.clone(),
                request.source_kind,
                parsed.canonical_source,
                parsed.document,
            )
            .with_source_locator(SourceLocator::created());
            candidate.units.insert(request.linear_position, unit);

            let toc_id = unique_id(
                candidate,
                "toc",
                &format!("{}\0{}\0{unit_id}", candidate.id, candidate.toc.len()),
            );
            let node = TocNode::new(toc_id, title.clone(), TocTarget::unit(unit_id.clone()));
            insert_toc_node(
                &mut candidate.toc,
                request.toc_parent_id.as_deref(),
                request.toc_position,
                node,
            )?;
            added_id = unit_id;
            Ok(())
        })?;
        Ok(added_id)
    }

    pub fn update_unit_source(
        &mut self,
        unit_id: &str,
        source_kind: SourceKind,
        source: &str,
    ) -> Result<()> {
        self.mutate(|candidate| {
            let unit = candidate
                .units
                .iter_mut()
                .find(|unit| unit.id == unit_id)
                .context("内容单元不存在")?;
            let parsed = parse_source_for_unit(source_kind, source, unit_id)?;
            unit.source_kind = source_kind;
            unit.source = parsed.canonical_source;
            unit.document = parsed.document;
            Ok(())
        })
    }

    pub fn update_unit_identity(
        &mut self,
        unit_id: &str,
        title: impl Into<String>,
        kind: ContentUnitKind,
    ) -> Result<()> {
        let title = title.into().trim().to_string();
        ensure!(!title.is_empty(), "内容单元标题不能为空");
        self.mutate(|candidate| {
            let unit = candidate
                .units
                .iter_mut()
                .find(|unit| unit.id == unit_id)
                .context("内容单元不存在")?;
            unit.title = title.clone();
            unit.kind = kind;
            rename_toc_targets(&mut candidate.toc, unit_id, &title);
            Ok(())
        })
    }

    pub fn remove_unit(&mut self, unit_id: &str) -> Result<()> {
        self.mutate(|candidate| {
            ensure!(candidate.units.len() > 1, "图书至少需要保留一个内容单元");
            let old_len = candidate.units.len();
            candidate.units.retain(|unit| unit.id != unit_id);
            ensure!(candidate.units.len() != old_len, "内容单元不存在");
            remove_toc_targets(&mut candidate.toc, unit_id);
            Ok(())
        })
    }

    pub fn move_unit(&mut self, unit_id: &str, new_position: usize) -> Result<()> {
        self.mutate(|candidate| {
            ensure!(new_position < candidate.units.len(), "线性内容移动位置越界");
            let old_position = candidate
                .units
                .iter()
                .position(|unit| unit.id == unit_id)
                .context("内容单元不存在")?;
            let unit = candidate.units.remove(old_position);
            candidate.units.insert(new_position, unit);
            Ok(())
        })
    }

    /// Moves a TOC node independently from linear reading order.
    pub fn move_toc_node(
        &mut self,
        toc_id: &str,
        new_parent_id: Option<&str>,
        new_position: usize,
    ) -> Result<()> {
        self.mutate(|candidate| {
            if new_parent_id == Some(toc_id) {
                bail!("目录节点不能成为自己的子节点");
            }
            let node = detach_toc_node(&mut candidate.toc, toc_id).context("目录节点不存在")?;
            ensure!(
                new_parent_id.is_none_or(|parent| !contains_toc_id(&node.children, parent)),
                "目录节点不能移动到自己的后代中"
            );
            insert_toc_node(&mut candidate.toc, new_parent_id, new_position, node)
        })
    }

    fn mutate(&mut self, operation: impl FnOnce(&mut BookDocument) -> Result<()>) -> Result<()> {
        let mut candidate = self.document.clone();
        operation(&mut candidate)?;
        candidate.validate().context("编辑后的图书结构无效")?;
        self.document = candidate;
        Ok(())
    }
}

fn trim_option(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn unique_id(document: &BookDocument, prefix: &str, seed: &str) -> String {
    for salt in 0_u64.. {
        let id = deterministic_id(prefix, format!("{seed}\0{salt}").as_bytes());
        let exists = match prefix {
            "unit" => document.units.iter().any(|unit| unit.id == id),
            "toc" => contains_toc_id(&document.toc, &id),
            _ => false,
        };
        if !exists {
            return id;
        }
    }
    unreachable!("u64 ID salt space exhausted")
}

fn insert_toc_node(
    nodes: &mut Vec<TocNode>,
    parent_id: Option<&str>,
    position: usize,
    node: TocNode,
) -> Result<()> {
    match parent_id {
        None => {
            ensure!(position <= nodes.len(), "目录插入位置越界");
            nodes.insert(position, node);
            Ok(())
        }
        Some(parent_id) => {
            let parent = find_toc_node_mut(nodes, parent_id).context("父目录节点不存在")?;
            ensure!(position <= parent.children.len(), "目录插入位置越界");
            parent.children.insert(position, node);
            Ok(())
        }
    }
}

fn find_toc_node_mut<'a>(nodes: &'a mut [TocNode], id: &str) -> Option<&'a mut TocNode> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find_toc_node_mut(&mut node.children, id) {
            return Some(found);
        }
    }
    None
}

fn contains_toc_id(nodes: &[TocNode], id: &str) -> bool {
    nodes
        .iter()
        .any(|node| node.id == id || contains_toc_id(&node.children, id))
}

fn detach_toc_node(nodes: &mut Vec<TocNode>, id: &str) -> Option<TocNode> {
    if let Some(position) = nodes.iter().position(|node| node.id == id) {
        return Some(nodes.remove(position));
    }
    for node in nodes {
        if let Some(found) = detach_toc_node(&mut node.children, id) {
            return Some(found);
        }
    }
    None
}

fn remove_toc_targets(nodes: &mut Vec<TocNode>, unit_id: &str) {
    nodes.retain(|node| node.target.unit_id() != unit_id);
    for node in nodes {
        remove_toc_targets(&mut node.children, unit_id);
    }
}

fn rename_toc_targets(nodes: &mut [TocNode], unit_id: &str, title: &str) {
    for node in nodes {
        if node.target.unit_id() == unit_id {
            node.label = title.to_string();
        }
        rename_toc_targets(&mut node.children, unit_id, title);
    }
}

#[cfg(test)]
mod tests {
    use crate::document::{Block, BlockDocument, Revision};

    use super::*;

    fn fixture() -> BookDocument {
        let mut document = BookDocument::created("book-1", "测试");
        document.revision = Revision::new(3);
        document.units.push(
            ContentUnit::new(
                "unit-1",
                ContentUnitKind::Chapter,
                "第一章",
                SourceKind::Markdown,
                "第一章",
                BlockDocument::new(vec![Block::paragraph("block-1", "第一章")]),
            )
            .with_source_locator(SourceLocator::created()),
        );
        document
            .toc
            .push(TocNode::new("toc-1", "第一章", TocTarget::unit("unit-1")));
        document.validate().unwrap();
        document
    }

    #[test]
    fn add_edit_reorder_and_remove_units_keeps_valid_document() {
        let mut editor = DocumentEditor::new(fixture()).unwrap();
        let second = editor
            .add_unit(NewContentUnit::markdown_chapter("第二章", 1))
            .unwrap();
        editor
            .update_unit_source(&second, SourceKind::Markdown, "## 修改\n\n正文")
            .unwrap();
        editor.move_unit(&second, 0).unwrap();
        assert_eq!(editor.document().units[0].id, second);
        editor.remove_unit("unit-1").unwrap();
        assert_eq!(editor.document().units.len(), 1);
        editor.document().validate().unwrap();
    }

    #[test]
    fn toc_can_be_nested_without_changing_linear_order() {
        let mut editor = DocumentEditor::new(fixture()).unwrap();
        let second = editor
            .add_unit(NewContentUnit::markdown_chapter("第二章", 1))
            .unwrap();
        let second_toc = editor
            .document()
            .toc
            .iter()
            .find(|node| node.target.unit_id() == second)
            .unwrap()
            .id
            .clone();
        editor.move_toc_node(&second_toc, Some("toc-1"), 0).unwrap();
        assert_eq!(editor.document().toc[0].children[0].id, second_toc);
        assert_eq!(editor.document().units[1].id, second);
    }

    #[test]
    fn failed_edit_does_not_mutate_document() {
        let mut editor = DocumentEditor::new(fixture()).unwrap();
        let before = editor.document().clone();
        assert!(editor.remove_unit("unit-1").is_err());
        assert_eq!(editor.document(), &before);
    }
}
