//! The block document: the depth-guarded tree and its hand-written serde.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDocument {
    pub schema_version: u32,
    pub blocks: Vec<Block>,
}

impl Default for BlockDocument {
    fn default() -> Self {
        Self {
            schema_version: BLOCK_SCHEMA_VERSION,
            blocks: Vec::new(),
        }
    }
}

impl BlockDocument {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            schema_version: BLOCK_SCHEMA_VERSION,
            blocks,
        }
    }

    pub fn plain_text(&self) -> String {
        join_block_text_at_depth(&self.blocks, "\n\n", 0)
    }

    pub fn content_hash(&self) -> String {
        stable_json_hash(self)
    }

    pub fn find_block(&self, block_id: &str) -> Option<&Block> {
        self.blocks
            .iter()
            .find_map(|block| block.find_block(block_id, 0))
    }

    pub fn referenced_asset_ids(&self) -> Vec<&str> {
        let mut result = Vec::new();
        for block in &self.blocks {
            block.collect_asset_ids(&mut result, 0);
        }
        result
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("document", &mut validator);
        validator.finish()
    }

    pub(crate) fn validate_into(&self, path: &str, validator: &mut Validator) {
        if self.schema_version != BLOCK_SCHEMA_VERSION {
            validator.issue(
                format!("{path}.schema_version"),
                format!(
                    "unsupported block schema version {}; expected {BLOCK_SCHEMA_VERSION}",
                    self.schema_version
                ),
            );
        }
        let mut block_ids = HashSet::new();
        validate_blocks(
            &self.blocks,
            &format!("{path}.blocks"),
            0,
            &mut block_ids,
            validator,
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph {
        id: String,
        content: Vec<Inline>,
    },
    Heading {
        id: String,
        level: u8,
        content: Vec<Inline>,
    },
    BlockQuote {
        id: String,
        blocks: Vec<Block>,
    },
    BulletList {
        id: String,
        items: Vec<ListItem>,
    },
    OrderedList {
        id: String,
        start: u64,
        items: Vec<ListItem>,
    },
    CodeBlock {
        id: String,
        language: Option<String>,
        code: String,
    },
    ThematicBreak {
        id: String,
    },
    Table {
        id: String,
        header: Option<TableRow>,
        rows: Vec<TableRow>,
    },
    Image {
        id: String,
        asset_id: String,
        alt: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Audio {
        id: String,
        asset_id: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Video {
        id: String,
        asset_id: String,
        poster_asset_id: Option<String>,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    /// An unsupported but preserved HTML subtree. `plain_text` must be the
    /// inert visible-text projection created by the HTML importer.
    RawHtml {
        id: String,
        source: String,
        plain_text: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Block", tag = "type", rename_all = "snake_case")]
pub(crate) enum BlockSerde {
    Paragraph {
        id: String,
        content: Vec<Inline>,
    },
    Heading {
        id: String,
        level: u8,
        content: Vec<Inline>,
    },
    BlockQuote {
        id: String,
        blocks: Vec<Block>,
    },
    BulletList {
        id: String,
        items: Vec<ListItem>,
    },
    OrderedList {
        id: String,
        start: u64,
        items: Vec<ListItem>,
    },
    CodeBlock {
        id: String,
        language: Option<String>,
        code: String,
    },
    ThematicBreak {
        id: String,
    },
    Table {
        id: String,
        header: Option<TableRow>,
        rows: Vec<TableRow>,
    },
    Image {
        id: String,
        asset_id: String,
        alt: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Audio {
        id: String,
        asset_id: String,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    Video {
        id: String,
        asset_id: String,
        poster_asset_id: Option<String>,
        title: Option<String>,
        caption: Vec<Inline>,
    },
    RawHtml {
        id: String,
        source: String,
        plain_text: String,
    },
}

impl Serialize for Block {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::ser::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        BlockSerde::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::de::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        BlockSerde::deserialize(deserializer)
    }
}

impl Block {
    pub fn paragraph(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Paragraph {
            id: id.into(),
            content: vec![Inline::text(text)],
        }
    }

    pub fn heading(id: impl Into<String>, level: u8, text: impl Into<String>) -> Self {
        Self::Heading {
            id: id.into(),
            level,
            content: vec![Inline::text(text)],
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Paragraph { id, .. }
            | Self::Heading { id, .. }
            | Self::BlockQuote { id, .. }
            | Self::BulletList { id, .. }
            | Self::OrderedList { id, .. }
            | Self::CodeBlock { id, .. }
            | Self::ThematicBreak { id }
            | Self::Table { id, .. }
            | Self::Image { id, .. }
            | Self::Audio { id, .. }
            | Self::Video { id, .. }
            | Self::RawHtml { id, .. } => id,
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
            Self::Paragraph { content, .. } | Self::Heading { content, .. } => {
                inline_plain_text_at_depth(content, depth + 1)
            }
            Self::BlockQuote { blocks, .. } => join_block_text_at_depth(blocks, "\n\n", depth + 1),
            Self::BulletList { items, .. } | Self::OrderedList { items, .. } => items
                .iter()
                .map(|item| item.plain_text_at_depth(depth + 1))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::CodeBlock { code, .. } => code.clone(),
            Self::ThematicBreak { .. } => String::new(),
            Self::Table { header, rows, .. } => header
                .iter()
                .chain(rows.iter())
                .map(|row| row.plain_text_at_depth(depth + 1))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Image {
                alt,
                title,
                caption,
                ..
            } => {
                let caption = inline_plain_text_at_depth(caption, depth + 1);
                join_visible_text([
                    title.as_deref().unwrap_or_default(),
                    alt.as_str(),
                    caption.as_str(),
                ])
            }
            Self::Audio { title, caption, .. } | Self::Video { title, caption, .. } => {
                let caption = inline_plain_text_at_depth(caption, depth + 1);
                join_visible_text([title.as_deref().unwrap_or_default(), caption.as_str()])
            }
            Self::RawHtml { plain_text, .. } => plain_text.clone(),
        }
    }

    pub(crate) fn find_block(&self, block_id: &str, depth: usize) -> Option<&Block> {
        if depth > MAX_DOCUMENT_DEPTH {
            return None;
        }
        if self.id() == block_id {
            return Some(self);
        }
        match self {
            Self::BlockQuote { blocks, .. } => blocks
                .iter()
                .find_map(|block| block.find_block(block_id, depth + 1)),
            Self::BulletList { items, .. } | Self::OrderedList { items, .. } => items
                .iter()
                .flat_map(|item| item.blocks.iter())
                .find_map(|block| block.find_block(block_id, depth + 1)),
            _ => None,
        }
    }

    pub(crate) fn collect_asset_ids<'a>(&'a self, output: &mut Vec<&'a str>, depth: usize) {
        if depth > MAX_DOCUMENT_DEPTH {
            return;
        }
        match self {
            Self::BlockQuote { blocks, .. } => {
                for block in blocks {
                    block.collect_asset_ids(output, depth + 1);
                }
            }
            Self::BulletList { items, .. } | Self::OrderedList { items, .. } => {
                for item in items {
                    for block in &item.blocks {
                        block.collect_asset_ids(output, depth + 1);
                    }
                }
            }
            Self::Paragraph { content, .. } | Self::Heading { content, .. } => {
                collect_inline_asset_ids(content, output, depth + 1)
            }
            Self::Table { header, rows, .. } => {
                for row in header.iter().chain(rows.iter()) {
                    for cell in &row.cells {
                        collect_inline_asset_ids(&cell.content, output, depth + 1);
                    }
                }
            }
            Self::Image {
                asset_id, caption, ..
            }
            | Self::Audio {
                asset_id, caption, ..
            } => {
                output.push(asset_id);
                collect_inline_asset_ids(caption, output, depth + 1);
            }
            Self::Video {
                asset_id,
                poster_asset_id,
                caption,
                ..
            } => {
                output.push(asset_id);
                if let Some(poster_asset_id) = poster_asset_id.as_deref() {
                    output.push(poster_asset_id);
                }
                collect_inline_asset_ids(caption, output, depth + 1);
            }
            Self::CodeBlock { .. } | Self::ThematicBreak { .. } | Self::RawHtml { .. } => {}
        }
    }
}
