//! Table-of-contents nodes and their targets.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocNode {
    pub id: String,
    pub label: String,
    pub target: TocTarget,
    pub children: Vec<TocNode>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "TocNode")]
pub(crate) struct TocNodeSerde {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) target: TocTarget,
    pub(crate) children: Vec<TocNode>,
}

impl Serialize for TocNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::ser::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        TocNodeSerde::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for TocNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(_depth_guard) = enter_model_serde_depth() else {
            return Err(serde::de::Error::custom(format!(
                "document nesting exceeds {MAX_DOCUMENT_DEPTH}"
            )));
        };
        TocNodeSerde::deserialize(deserializer)
    }
}

impl TocNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>, target: TocTarget) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            target,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TocTarget {
    Unit { unit_id: String },
    Block { unit_id: String, block_id: String },
}

impl TocTarget {
    pub fn unit(unit_id: impl Into<String>) -> Self {
        Self::Unit {
            unit_id: unit_id.into(),
        }
    }

    pub fn block(unit_id: impl Into<String>, block_id: impl Into<String>) -> Self {
        Self::Block {
            unit_id: unit_id.into(),
            block_id: block_id.into(),
        }
    }

    pub fn unit_id(&self) -> &str {
        match self {
            Self::Unit { unit_id } | Self::Block { unit_id, .. } => unit_id,
        }
    }
}
