//! In-memory view state: the group filter, the sort order, the display mode,
//! the modal stack and the library mutation lifecycle.

use super::*;

/// Which slice of the library is on screen.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum GroupFilter {
    #[default]
    All,
    Ungrouped,
    Group(String),
}

/// The field the visible books are sorted by. The default keeps the catalog
/// order (newest import first) until the user picks one of the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BookSortField {
    Title,
    Author,
    #[default]
    AddedAt,
}

impl BookSortField {
    /// Menu label; the sort button reuses it as "按名称" and friends.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Title => "名称",
            Self::Author => "作者",
            Self::AddedAt => "创建时间",
        }
    }

    /// Direction a field starts in when it is picked: text reads top-down,
    /// time reads newest-first.
    pub(crate) fn default_direction(self) -> BookSortDirection {
        match self {
            Self::Title | Self::Author => BookSortDirection::Ascending,
            Self::AddedAt => BookSortDirection::Descending,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BookSortDirection {
    Ascending,
    Descending,
}

impl BookSortDirection {
    pub(crate) fn is_ascending(self) -> bool {
        matches!(self, Self::Ascending)
    }
}

/// How the visible books are laid out: cover cards in a grid, or one book per
/// row with its metadata in columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BookViewMode {
    #[default]
    Cards,
    List,
}

/// Sorts the visible indices in place. Sorting is stable, so books that share
/// a key (same author, same import timestamp) keep the catalog's relative
/// order instead of jumping between renders.
pub(crate) fn sort_book_indices(
    books: &[BookRecord],
    indices: &mut [usize],
    field: BookSortField,
    direction: BookSortDirection,
) {
    // Text keys are lowercased so "ai engineering" and "AI Engineering" do not
    // split the list. Chinese titles sort by code point, not by pinyin.
    match field {
        BookSortField::Title => {
            sort_indices_by_key(indices, direction, |index| {
                books[*index].title.to_lowercase()
            });
        }
        BookSortField::Author => {
            sort_indices_by_key(indices, direction, |index| {
                (
                    books[*index].author.to_lowercase(),
                    books[*index].title.to_lowercase(),
                )
            });
        }
        BookSortField::AddedAt => {
            sort_indices_by_key(indices, direction, |index| books[*index].added_at);
        }
    }
}

fn sort_indices_by_key<K: Ord>(
    indices: &mut [usize],
    direction: BookSortDirection,
    key: impl Fn(&usize) -> K,
) {
    match direction {
        BookSortDirection::Ascending => indices.sort_by_cached_key(key),
        BookSortDirection::Descending => {
            indices.sort_by_cached_key(|index| std::cmp::Reverse(key(index)));
        }
    }
}

/// `BookRecord::added_at` is stored in nanoseconds; the list only shows the
/// day, rendered in UTC like the rest of the app's timestamps.
pub(crate) fn format_added_date(added_at_nanos: u64) -> String {
    let seconds = added_at_nanos / 1_000_000_000;
    let days = (seconds / 86_400) as i64;
    // Civil date conversion from Unix days, independent of local timezone and
    // platform date APIs. Same algorithm as the job diagnostics formatter.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
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
    RenameBook(String),
    SetBookAuthor(String),
    CreateGroup(Option<String>),
    RenameGroup(String),
}

/// The inline error shown for a blank name, or `None` when blank is a
/// meaningful value for this action: clearing a book's authors is legitimate,
/// so an empty author is submitted instead of rejected.
pub(crate) fn name_input_required_message(action: &NameInputAction) -> Option<&'static str> {
    match action {
        NameInputAction::CreateBook | NameInputAction::RenameBook(_) => Some("图书名称不能为空"),
        NameInputAction::CreateGroup(_) | NameInputAction::RenameGroup(_) => {
            Some("分组名称不能为空")
        }
        NameInputAction::SetBookAuthor(_) => None,
    }
}

/// Modal dialogs for library and group actions. They are rendered as an in-app
/// overlay layered over the library view rather than through the component
/// library's dialog host.
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
