//! Persistent reading notes: three exclusive marks, human thoughts, and AI thoughts.
//!
//! Anchors use UTF-16 offsets in the reading chapter's DOM text after removing
//! ECMAScript whitespace. The quote and both revisions are verified by the
//! host; repeated text is never relocated by searching for the first match.

use anyhow::{Result, ensure};
use html5ever::{parse_document, tendril::TendrilSink as _};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde::{Deserialize, Serialize};

pub const MAX_ANNOTATION_QUOTE_BYTES: usize = 32 * 1024;
pub const MAX_HUMAN_COMMENT_BYTES: usize = 64 * 1024;
pub const MAX_AI_COMMENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    Highlight,
    Wavy,
    Underline,
    HumanComment,
    AiComment,
}

impl AnnotationKind {
    pub fn is_mark(self) -> bool {
        matches!(self, Self::Highlight | Self::Wavy | Self::Underline)
    }

    pub fn is_comment(self) -> bool {
        matches!(self, Self::HumanComment | Self::AiComment)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Wavy => "wavy",
            Self::Underline => "underline",
            Self::HumanComment => "human_comment",
            Self::AiComment => "ai_comment",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextAnchor {
    pub quote: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationDraft {
    pub content_unit_id: String,
    pub document_revision: u64,
    pub unit_revision: u64,
    pub anchor: TextAnchor,
    pub kind: AnnotationKind,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: String,
    pub book_id: String,
    pub content_unit_id: String,
    pub document_revision: u64,
    pub unit_revision: u64,
    pub anchor: TextAnchor,
    pub kind: AnnotationKind,
    pub comment: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Derived from current ownership and revisions; stale notes retain their
    /// quote and thought but must not be painted onto newer chapter text.
    pub stale: bool,
}

/// A saved note and the current catalog information needed to browse it.
/// Deleted chapters keep their note content but no longer have a location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationOverview {
    pub annotation: Annotation,
    pub book_title: String,
    pub chapter_title: Option<String>,
    /// Zero-based position in the current book's content-unit sequence.
    pub chapter_index: Option<usize>,
}

pub(crate) fn validate_draft(draft: &AnnotationDraft) -> Result<()> {
    ensure!(!draft.content_unit_id.is_empty(), "笔记缺少章节定位");
    ensure!(
        draft.document_revision <= i64::MAX as u64 && draft.unit_revision <= i64::MAX as u64,
        "笔记版本超出支持范围"
    );
    ensure!(
        draft.anchor.start < draft.anchor.end,
        "请选择非空文本后添加笔记"
    );
    ensure!(
        draft.anchor.quote.len() <= MAX_ANNOTATION_QUOTE_BYTES,
        "所选文本过长，请缩小笔记范围"
    );
    ensure!(
        !compact_text(&draft.anchor.quote).is_empty(),
        "请选择非空文本后添加笔记"
    );
    if draft.kind.is_comment() {
        validate_comment(draft.comment.as_deref().unwrap_or_default(), draft.kind)?;
    } else {
        ensure!(draft.comment.is_none(), "划线笔记不能包含想法正文");
    }
    Ok(())
}

pub(crate) fn validate_comment(comment: &str, kind: AnnotationKind) -> Result<()> {
    ensure!(!comment.trim().is_empty(), "想法内容不能为空");
    let max_bytes = match kind {
        AnnotationKind::HumanComment => MAX_HUMAN_COMMENT_BYTES,
        AnnotationKind::AiComment => MAX_AI_COMMENT_BYTES,
        _ => anyhow::bail!("划线笔记不能包含想法正文"),
    };
    ensure!(comment.len() <= max_bytes, "想法内容过长");
    Ok(())
}

/// Exact ECMAScript WhiteSpace + LineTerminator set used by JavaScript /\s/.
fn is_js_whitespace(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

pub fn compact_text(text: &str) -> String {
    text.chars().filter(|ch| !is_js_whitespace(*ch)).collect()
}

pub(crate) fn reader_body_text(html: &str) -> String {
    let dom = parse_document(RcDom::default(), Default::default()).one(html);
    // Keep the root alive: rcdom's Node::drop clears descendant child lists,
    // even when this traversal still holds handles to those descendants.
    let mut pending: Vec<(Handle, bool)> = vec![(dom.document.clone(), false)];
    let mut output = String::new();
    while let Some((node, in_body)) = pending.pop() {
        let in_body = match &node.data {
            NodeData::Element { name, .. } => {
                let tag = name.local.as_ref();
                if matches!(tag, "script" | "style" | "noscript" | "template") {
                    continue;
                }
                in_body || tag == "body"
            }
            NodeData::Text { contents } if in_body => {
                output.extend(
                    contents
                        .borrow()
                        .chars()
                        .filter(|ch| !is_js_whitespace(*ch)),
                );
                continue;
            }
            _ => in_body,
        };
        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .cloned()
                .map(|child| (child, in_body)),
        );
    }
    output
}

pub(crate) fn validate_anchor(text: &str, anchor: &TextAnchor) -> Result<()> {
    let utf16 = text.encode_utf16().collect::<Vec<_>>();
    let selected = utf16
        .get(anchor.start as usize..anchor.end as usize)
        .and_then(|slice| String::from_utf16(slice).ok());
    ensure!(
        selected.as_deref() == Some(compact_text(&anchor.quote).as_str()),
        "所选文本与当前章节不一致，请重新打开章节后选择"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_projection_matches_browser_text_and_ecmascript_whitespace() {
        let html = "<html><head><title>不属于正文</title></head><body><p>甲 &amp; <b>😀</b></p><script>bad</script><style>bad</style><template>bad</template><noscript>bad</noscript><p>乙\u{feff}\u{0085}丙</p></body></html>";
        assert_eq!(reader_body_text(html), "甲&😀乙\u{0085}丙");
    }

    #[test]
    fn anchor_uses_exact_duplicate_occurrence_and_whole_utf16_characters() {
        let text = "甲😀重复乙重复";
        validate_anchor(
            text,
            &TextAnchor {
                quote: "重 复".into(),
                start: 6,
                end: 8,
            },
        )
        .unwrap();
        assert!(
            validate_anchor(
                text,
                &TextAnchor {
                    quote: "重复".into(),
                    start: 5,
                    end: 7
                }
            )
            .is_err()
        );
        validate_anchor(
            text,
            &TextAnchor {
                quote: "😀".into(),
                start: 1,
                end: 3,
            },
        )
        .unwrap();
        assert!(
            validate_anchor(
                text,
                &TextAnchor {
                    quote: "😀".into(),
                    start: 1,
                    end: 2
                }
            )
            .is_err()
        );
    }
}
