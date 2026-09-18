//! Serving and shrinking outline results.

use super::*;

pub(crate) fn sanitize_outlines(
    records: Vec<BookOutlineRecord>,
    host_scope: &AllowedBookScope,
    requested: &HashSet<String>,
    max_books: usize,
    max_depth: usize,
    max_nodes: usize,
) -> (Vec<BookOutlineRecord>, bool) {
    let mut remaining_nodes = max_nodes;
    let mut truncated = false;
    let mut seen_books = HashSet::new();
    let mut output = Vec::new();
    for record in records {
        if output.len() >= max_books {
            truncated = true;
            break;
        }
        if !host_scope.contains(&record.book_id)
            || !requested.contains(record.book_id.as_str())
            || !valid_identifier(&record.book_id)
            || !seen_books.insert(record.book_id.clone())
        {
            continue;
        }
        let nodes = sanitize_outline_nodes(
            record.nodes,
            &record.book_id,
            1,
            max_depth,
            &mut remaining_nodes,
            &mut truncated,
        );
        output.push(BookOutlineRecord {
            book_id: record.book_id,
            book_title: truncate_utf8(&record.book_title, 512),
            nodes,
        });
    }
    (output, truncated)
}

pub(crate) fn sanitize_outline_nodes(
    nodes: Vec<OutlineNodeRecord>,
    book_id: &str,
    depth: usize,
    max_depth: usize,
    remaining: &mut usize,
    truncated: &mut bool,
) -> Vec<OutlineNodeRecord> {
    let mut output = Vec::new();
    for node in nodes {
        if *remaining == 0 {
            *truncated = true;
            break;
        }
        if node.locator.book_id != book_id || node.locator.validate().is_err() {
            continue;
        }
        *remaining -= 1;
        let children = if depth < max_depth {
            sanitize_outline_nodes(
                node.children,
                book_id,
                depth + 1,
                max_depth,
                remaining,
                truncated,
            )
        } else {
            if !node.children.is_empty() {
                *truncated = true;
            }
            Vec::new()
        };
        output.push(OutlineNodeRecord {
            title: truncate_utf8(&node.title, 512),
            locator: node.locator,
            children,
        });
    }
    output
}

/// Model-facing outline node. Locators are dropped: an outline is orientation
/// only, the host registers no citation for it, and exposing coordinates
/// invites the model to emit them as source markers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ServedOutlineNode {
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) children: Vec<ServedOutlineNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ServedOutlineBook {
    pub(crate) book_title: String,
    pub(crate) nodes: Vec<ServedOutlineNode>,
}

pub(crate) fn served_outline(records: &[BookOutlineRecord]) -> Vec<ServedOutlineBook> {
    records
        .iter()
        .map(|record| ServedOutlineBook {
            book_title: record.book_title.clone(),
            nodes: record.nodes.iter().map(served_outline_node).collect(),
        })
        .collect()
}

pub(crate) fn served_outline_node(node: &OutlineNodeRecord) -> ServedOutlineNode {
    ServedOutlineNode {
        title: node.title.clone(),
        children: node.children.iter().map(served_outline_node).collect(),
    }
}

pub(crate) fn fit_outline_output(
    mut records: Vec<BookOutlineRecord>,
    mut truncated: bool,
    budget: usize,
) -> AgentResult<String> {
    loop {
        let content = serde_json::to_string(&json!({
            "tool": GET_OUTLINE_TOOL,
            "books": served_outline(&records),
            "truncated": truncated,
        }))
        .map_err(|error| AgentError::Backend(error.to_string()))?;
        if content.len() <= budget {
            return Ok(content);
        }
        truncated = true;
        if !pop_last_outline_node(&mut records) && records.pop().is_none() {
            return Err(AgentError::LimitExceeded("context size"));
        }
    }
}

pub(crate) fn pop_last_outline_node(records: &mut [BookOutlineRecord]) -> bool {
    for book in records.iter_mut().rev() {
        if pop_last_node(&mut book.nodes) {
            return true;
        }
    }
    false
}

pub(crate) fn pop_last_node(nodes: &mut Vec<OutlineNodeRecord>) -> bool {
    let Some(last) = nodes.last_mut() else {
        return false;
    };
    if pop_last_node(&mut last.children) {
        true
    } else {
        nodes.pop();
        true
    }
}
