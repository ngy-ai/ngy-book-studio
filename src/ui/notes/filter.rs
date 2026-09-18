//! The type/book/chapter filter and its query string.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum NoteFilter {
    #[default]
    All,
    Marks,
    Human,
    Ai,
}

impl NoteFilter {
    pub(super) const ALL: [Self; 4] = [Self::All, Self::Marks, Self::Human, Self::Ai];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Marks => "划线",
            Self::Human => "人工想法",
            Self::Ai => "AI 想法",
        }
    }

    pub(super) fn includes(self, kind: AnnotationKind) -> bool {
        match self {
            Self::All => true,
            Self::Marks => kind.is_mark(),
            Self::Human => kind == AnnotationKind::HumanComment,
            Self::Ai => kind == AnnotationKind::AiComment,
        }
    }
}
