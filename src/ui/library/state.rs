//! In-memory view state: the group filter, the modal stack and the library
//! mutation lifecycle.

use super::*;

/// Which slice of the library is on screen.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum GroupFilter {
    #[default]
    All,
    Ungrouped,
    Group(String),
}

impl GroupFilter {
    pub(super) fn group_id(&self) -> Option<&str> {
        match self {
            Self::Group(id) => Some(id.as_str()),
            _ => None,
        }
    }
}

/// The group a newly imported book is filed into: the shelf the user is on,
/// but only when that shelf is one concrete group. "全部图书" and "未分组"
/// leave the import as it is, and a group that disappeared after it was picked
/// leaves the import unfiled instead of failing a durable import.
pub(crate) fn import_target_group(library: &LibraryStore, filter: &GroupFilter) -> Option<String> {
    let group_id = filter.group_id()?;
    library.group(group_id).map(|group| group.id.clone())
}

/// Groups the book group picker expands on open: every ancestor of the current
/// group, so the current selection is visible without expanding the tree by
/// hand. The group itself stays collapsed unless it has expanded children.
pub(crate) fn picker_expanded_groups(path: &[BookGroup]) -> HashSet<String> {
    path.iter()
        .take(path.len().saturating_sub(1))
        .map(|group| group.id.clone())
        .collect()
}

pub(crate) fn should_apply_library_projection(
    incoming_generation: u64,
    applied_generation: u64,
) -> bool {
    incoming_generation >= applied_generation
}

#[derive(Debug, Default)]
pub(crate) struct LibraryMutationLifecycle {
    pub(crate) pending: usize,
    pub(crate) close_requested: bool,
    pub(crate) close_scheduled: bool,
}

impl LibraryMutationLifecycle {
    pub(super) fn begin(&mut self) -> bool {
        if self.close_requested {
            return false;
        }
        self.pending = self
            .pending
            .checked_add(1)
            .expect("pending library mutation count overflowed");
        true
    }

    pub(super) fn finish(&mut self) -> bool {
        self.pending = self
            .pending
            .checked_sub(1)
            .expect("library mutation completed without a matching begin");
        self.take_close_ready()
    }

    pub(super) fn request_close(&mut self) -> bool {
        self.close_requested = true;
        self.take_close_ready()
    }

    pub(super) fn close_requested(&self) -> bool {
        self.close_requested
    }

    pub(super) fn take_close_ready(&mut self) -> bool {
        if self.close_requested && self.pending == 0 && !self.close_scheduled {
            self.close_scheduled = true;
            true
        } else {
            false
        }
    }
}

pub(crate) fn library_ai_scope(library: &LibraryStore, filter: &GroupFilter) -> AiSidebarScope {
    let label = match filter {
        GroupFilter::All => "全部图书".to_string(),
        GroupFilter::Ungrouped => "未分组".to_string(),
        GroupFilter::Group(id) => {
            let path = library
                .group_path(id)
                .into_iter()
                .map(|group| group.name)
                .collect::<Vec<_>>()
                .join(" / ");
            if path.is_empty() {
                "当前分组".to_string()
            } else {
                path
            }
        }
    };
    let subtree = match filter {
        GroupFilter::Group(id) => Some(
            library
                .group_subtree_ids(id)
                .into_iter()
                .collect::<HashSet<_>>(),
        ),
        _ => None,
    };
    let books = library
        .books()
        .iter()
        .filter(|book| match filter {
            GroupFilter::All => true,
            GroupFilter::Ungrouped => book.group_id.is_none(),
            GroupFilter::Group(_) => book
                .group_id
                .as_ref()
                .is_some_and(|group_id| subtree.as_ref().is_some_and(|ids| ids.contains(group_id))),
        })
        .map(|book| AiBookOption::new(book.id.clone(), book.title.clone()))
        .collect();
    AiSidebarScope::library(label, books)
}

#[derive(Clone)]
pub(crate) enum NameInputAction {
    CreateBook,
    CreateGroup(Option<String>),
    RenameGroup(String),
}

/// Modal dialogs for library and group actions. The pinned `gpui-component`
/// revision predates its `dialog` module, so these are rendered as an in-app
/// overlay.
#[derive(Clone)]
pub(crate) enum GroupModal {
    NameInput {
        title: String,
        confirm_label: String,
        action: NameInputAction,
        input: Entity<InputState>,
        pending_request: Option<u64>,
        error: Option<String>,
        /// Modal to reopen once this one completes, so an inline "新建分组"
        /// dialog can hand control back to the group tree picker.
        return_to: Option<Box<GroupModal>>,
    },
    DeleteConfirm {
        group_id: String,
        name: String,
        description: String,
    },
    DeleteBook {
        book_id: String,
        title: String,
        description: String,
    },
    OfficeTrust {
        book_id: String,
        title: String,
        description: String,
    },
    /// Group tree used to set one book's group. The flat context-menu variant
    /// grew a menu item per group, which became unusable once a library had
    /// more than a handful of nested groups.
    BookGroupPicker {
        book_id: String,
        book_title: String,
        selected: Option<String>,
        expanded: HashSet<String>,
    },
}
