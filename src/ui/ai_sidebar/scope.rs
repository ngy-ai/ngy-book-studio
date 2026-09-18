//! The scope the sidebar answers against: library, group or a single book.

use super::*;

/// Scope is supplied by the owning window. This component never queries the
/// library or database itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiSidebarScope {
    Library {
        label: String,
        books: Vec<AiBookOption>,
    },
    Book {
        current: AiBookOption,
        available: Vec<AiBookOption>,
        additional_ids: Vec<String>,
    },
}

impl AiSidebarScope {
    pub(crate) fn library(label: impl Into<String>, books: Vec<AiBookOption>) -> Self {
        Self::Library {
            label: non_empty_label(label.into(), "当前书架"),
            books: normalize_books(books),
        }
    }

    pub(crate) fn book(current: AiBookOption, available: Vec<AiBookOption>) -> Self {
        let current = AiBookOption::new(current.id, current.title);
        let mut available = normalize_books(available);
        if !available.iter().any(|book| book.id == current.id) {
            available.insert(0, current.clone());
        }
        Self::Book {
            current,
            available,
            additional_ids: Vec::new(),
        }
    }

    pub(crate) fn book_ids(&self) -> Vec<String> {
        match self {
            Self::Library { books, .. } => books.iter().map(|book| book.id.clone()).collect(),
            Self::Book {
                current,
                additional_ids,
                ..
            } => std::iter::once(current.id.clone())
                .chain(additional_ids.iter().cloned())
                .collect(),
        }
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Library { label, .. } => label.clone(),
            Self::Book {
                current,
                additional_ids,
                ..
            } => {
                if additional_ids.is_empty() {
                    current.title.clone()
                } else {
                    format!("{} 等 {} 本", current.title, additional_ids.len() + 1)
                }
            }
        }
    }

    pub(super) fn selected_books(&self) -> Vec<AiBookOption> {
        match self {
            Self::Library { books, .. } => books.clone(),
            Self::Book {
                current,
                available,
                additional_ids,
            } => {
                let mut selected = vec![current.clone()];
                for id in additional_ids {
                    if let Some(book) = available.iter().find(|book| book.id == *id) {
                        selected.push(book.clone());
                    }
                }
                selected
            }
        }
    }

    pub(super) fn available_books_to_add(&self) -> Vec<AiBookOption> {
        let Self::Book {
            current,
            available,
            additional_ids,
        } = self
        else {
            return Vec::new();
        };
        available
            .iter()
            .filter(|book| book.id != current.id && !additional_ids.contains(&book.id))
            .cloned()
            .collect()
    }

    pub(super) fn add_book(&mut self, book_id: &str) -> bool {
        let Self::Book {
            current,
            available,
            additional_ids,
        } = self
        else {
            return false;
        };
        if book_id == current.id
            || additional_ids.iter().any(|id| id == book_id)
            || !available.iter().any(|book| book.id == book_id)
        {
            return false;
        }
        additional_ids.push(book_id.to_string());
        true
    }

    pub(super) fn remove_book(&mut self, book_id: &str) -> bool {
        let Self::Book {
            current,
            additional_ids,
            ..
        } = self
        else {
            return false;
        };
        if book_id == current.id {
            return false;
        }
        let before = additional_ids.len();
        additional_ids.retain(|id| id != book_id);
        before != additional_ids.len()
    }
}

pub(crate) fn normalize_books(books: Vec<AiBookOption>) -> Vec<AiBookOption> {
    let mut seen = HashSet::new();
    books
        .into_iter()
        .filter_map(|book| {
            let id = book.id.trim().to_string();
            if id.is_empty() || !seen.insert(id.clone()) {
                return None;
            }
            Some(AiBookOption::new(id, book.title))
        })
        .collect()
}

pub(crate) fn non_empty_label(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value
    }
}
