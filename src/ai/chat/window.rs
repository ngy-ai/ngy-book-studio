//! The kind of window a chat thread belongs to.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatWindowKind {
    Library,
    Reader,
    Editor,
}

impl ChatWindowKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Reader => "reader",
            Self::Editor => "editor",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "library" => Ok(Self::Library),
            "reader" => Ok(Self::Reader),
            "editor" => Ok(Self::Editor),
            _ => bail!("unknown chat window kind: {value}"),
        }
    }
}
