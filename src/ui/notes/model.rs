//! The card model and the parsed thought display.

use super::*;

pub(crate) struct NoteCard {
    pub(crate) index: usize,
    pub(crate) quote_html: SharedString,
    pub(crate) comment_markdown: Option<Arc<ThoughtDisplay>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ThoughtDisplay {
    pub(crate) markdown: SharedString,
    pub(crate) parts: Vec<ThoughtPart>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ThoughtPart {
    Markdown(SharedString),
    Table {
        rows: Vec<Vec<SharedString>>,
        nested: bool,
    },
}

impl ThoughtDisplay {
    pub(super) fn new(markdown: String) -> Self {
        use ::markdown::{ParseOptions, mdast::Node, to_mdast};

        let mut parts = Vec::new();
        if let Ok(tree) = to_mdast(&markdown, &ParseOptions::gfm()) {
            let mut cursor = 0;
            for child in tree.children().into_iter().flatten() {
                if let Node::Table(table) = child {
                    if let Some(position) = child.position()
                        && let Some(before) = markdown.get(cursor..position.start.offset)
                    {
                        push_markdown_part(&mut parts, before);
                        parts.push(ThoughtPart::Table {
                            rows: table_rows(table, &markdown),
                            nested: false,
                        });
                        cursor = position.end.offset;
                    }
                } else {
                    let mut nested = Vec::new();
                    let mut pending = vec![child];
                    while let Some(node) = pending.pop() {
                        if let Node::Table(table) = node {
                            nested.push(table_rows(table, &markdown));
                        } else if let Some(children) = node.children() {
                            pending.extend(children.iter().rev());
                        }
                    }
                    // The component can still render the surrounding list or
                    // quote. Add complete native tables immediately after it,
                    // so nested cell content never depends on its truncation.
                    if !nested.is_empty()
                        && let Some(position) = child.position()
                        && let Some(before) = markdown.get(cursor..position.end.offset)
                    {
                        push_markdown_part(&mut parts, before);
                        for rows in nested {
                            parts.push(ThoughtPart::Table { rows, nested: true });
                        }
                        cursor = position.end.offset;
                    }
                }
            }
            if let Some(rest) = markdown.get(cursor..) {
                push_markdown_part(&mut parts, rest);
            }
        }
        Self {
            markdown: markdown.into(),
            parts,
        }
    }
}
