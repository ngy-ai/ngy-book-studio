//! Inline content and the list, row and cell wrappers around it.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItem {
    /// `Some` denotes a GFM task-list item; `None` denotes a normal item.
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

impl ListItem {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            checked: None,
            blocks,
        }
    }

    pub fn task(checked: bool, blocks: Vec<Block>) -> Self {
        Self {
            checked: Some(checked),
            blocks,
        }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    pub(crate) fn plain_text_at_depth(&self, depth: usize) -> String {
        join_block_text_at_depth(&self.blocks, "\n", depth)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

impl TableRow {
    pub fn new(cells: Vec<TableCell>) -> Self {
        Self { cells }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    pub(crate) fn plain_text_at_depth(&self, depth: usize) -> String {
        self.cells
            .iter()
            .map(|cell| cell.plain_text_at_depth(depth))
            .collect::<Vec<_>>()
            .join("\t")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCell {
    pub content: Vec<Inline>,
}

impl TableCell {
    pub fn new(content: Vec<Inline>) -> Self {
        Self { content }
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self::new(vec![Inline::text(value)])
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    pub(crate) fn plain_text_at_depth(&self, depth: usize) -> String {
        inline_plain_text_at_depth(&self.content, depth)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text {
        value: String,
    },
    Emphasis {
        content: Vec<Inline>,
    },
    Strong {
        content: Vec<Inline>,
    },
    Strikethrough {
        content: Vec<Inline>,
    },
    Code {
        value: String,
    },
    Link {
        href: String,
        title: Option<String>,
        content: Vec<Inline>,
    },
    HardBreak,
    SoftBreak,
    Image {
        asset_id: String,
        alt: String,
        title: Option<String>,
    },
    RawHtml {
        source: String,
        plain_text: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Inline", tag = "type", rename_all = "snake_case")]
pub(crate) enum InlineSerde {
    Text {
        value: String,
    },
    Emphasis {
        content: Vec<Inline>,
    },
    Strong {
        content: Vec<Inline>,
    },
    Strikethrough {
        content: Vec<Inline>,
    },
    Code {
        value: String,
    },
    Link {
        href: String,
        title: Option<String>,
        content: Vec<Inline>,
    },
    HardBreak,
    SoftBreak,
    Image {
        asset_id: String,
        alt: String,
        title: Option<String>,
    },
    RawHtml {
        source: String,
        plain_text: String,
    },
}

impl Serialize for Inline {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::ser::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        InlineSerde::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for Inline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::de::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        InlineSerde::deserialize(deserializer)
    }
}

impl Inline {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text {
            value: value.into(),
        }
    }

    pub fn plain_text(&self) -> String {
        self.plain_text_at_depth(0)
    }

    pub(crate) fn plain_text_at_depth(&self, depth: usize) -> String {
        if depth > MAX_DOCUMENT_DEPTH {
            return String::new();
        }
        match self {
            Self::Text { value } | Self::Code { value } => value.clone(),
            Self::Emphasis { content }
            | Self::Strong { content }
            | Self::Strikethrough { content }
            | Self::Link { content, .. } => inline_plain_text_at_depth(content, depth + 1),
            Self::HardBreak => "\n".to_string(),
            Self::SoftBreak => " ".to_string(),
            Self::Image { alt, title, .. } => {
                join_visible_text([title.as_deref().unwrap_or_default(), alt.as_str()])
            }
            Self::RawHtml { plain_text, .. } => plain_text.clone(),
        }
    }
}
