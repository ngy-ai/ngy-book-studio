//! Persistent, window-scoped AI conversations.
//!
//! SQLite row types stay in `db`; this module exposes validated domain types
//! and runs every database operation on the application's dedicated I/O
//! runtime so GPUI callbacks never have to execute SQLite work directly.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ai::ChatRole,
    db,
    document::{DocumentLocator, Revision, SourceLocator, deterministic_id},
    runtime::IoRuntime,
};

const MAX_SCOPE_BOOKS: usize = 512;
const MAX_TITLE_CHARS: usize = 256;
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CITATIONS: usize = 256;
const MAX_QUOTE_BYTES: usize = 256 * 1024;

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatWindowKind {
    Library,
    Reader,
    Editor,
}

impl ChatWindowKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Reader => "reader",
            Self::Editor => "editor",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "library" => Ok(Self::Library),
            "reader" => Ok(Self::Reader),
            "editor" => Ok(Self::Editor),
            _ => bail!("unknown chat window kind: {value}"),
        }
    }
}

/// Host-computed book authority persisted with a conversation.
///
/// Model-provided book IDs never update this value. A question may narrow this
/// scope through the Agent API, but it cannot widen it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatScope {
    pub book_ids: Vec<String>,
}

impl ChatScope {
    pub fn new<I, S>(book_ids: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut ids = BTreeSet::new();
        for id in book_ids {
            let id = id.into();
            validate_id("book ID", &id)?;
            ids.insert(id);
        }
        ensure!(
            ids.len() <= MAX_SCOPE_BOOKS,
            "chat scope exceeds {MAX_SCOPE_BOOKS} books"
        );
        Ok(Self {
            book_ids: ids.into_iter().collect(),
        })
    }

    pub fn contains(&self, book_id: &str) -> bool {
        self.book_ids
            .binary_search_by(|candidate| candidate.as_str().cmp(book_id))
            .is_ok()
    }

    fn normalized(&self) -> Result<Self> {
        Self::new(self.book_ids.iter().cloned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatThread {
    pub id: String,
    /// Reader/Editor conversations retain their mandatory current book here.
    /// Library conversations use `None` and persist their group-derived scope.
    pub primary_book_id: Option<String>,
    pub title: String,
    pub scope: ChatScope,
    pub window_kind: ChatWindowKind,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatThread {
    pub primary_book_id: Option<String>,
    pub title: String,
    pub scope: ChatScope,
    pub window_kind: ChatWindowKind,
}

impl NewChatThread {
    fn validate(&self) -> Result<ChatScope> {
        validate_title(&self.title)?;
        let scope = self.scope.normalized()?;
        match self.window_kind {
            ChatWindowKind::Library => {}
            ChatWindowKind::Reader | ChatWindowKind::Editor => {
                let primary = self
                    .primary_book_id
                    .as_deref()
                    .context("Reader/Editor chat needs a primary book")?;
                validate_id("primary book ID", primary)?;
                ensure!(
                    scope.contains(primary),
                    "Reader/Editor chat scope must contain its primary book"
                );
            }
        }
        if let Some(primary) = self.primary_book_id.as_deref() {
            validate_id("primary book ID", primary)?;
        }
        Ok(scope)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatCitation {
    pub id: String,
    pub content_unit_id: Option<String>,
    pub search_chunk_id: Option<String>,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub quote: String,
    pub locator: DocumentLocator,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatCitation {
    pub content_unit_id: Option<String>,
    pub search_chunk_id: Option<String>,
    pub document_revision: Revision,
    pub unit_revision: Revision,
    pub quote: String,
    pub locator: DocumentLocator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredChatMessage {
    pub id: String,
    pub thread_id: String,
    pub parent_id: Option<String>,
    pub ordinal: usize,
    pub role: ChatRole,
    pub content: String,
    pub model: Option<String>,
    pub created_at: u64,
    pub citations: Vec<ChatCitation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatMessage {
    pub parent_id: Option<String>,
    pub role: ChatRole,
    pub content: String,
    pub model: Option<String>,
    pub citations: Vec<NewChatCitation>,
}

impl NewChatMessage {
    pub fn text(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            parent_id: None,
            role,
            content: content.into(),
            model: None,
            citations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatSession {
    pub thread: ChatThread,
    pub messages: Vec<StoredChatMessage>,
}

/// Async repository backed by one SQLite connection per operation.
///
/// The mutex serializes ordinal allocation across cloned handles in this
/// process. SQLite's unique index remains the final consistency guard.
#[derive(Clone, Debug)]
pub struct ChatRepository {
    db_path: PathBuf,
    runtime: IoRuntime,
    writes: Arc<Mutex<()>>,
}

impl ChatRepository {
    pub fn new(db_path: impl Into<PathBuf>, runtime: IoRuntime) -> Self {
        Self {
            db_path: db_path.into(),
            runtime,
            writes: Arc::new(Mutex::new(())),
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub async fn create_thread(&self, draft: NewChatThread) -> Result<ChatThread> {
        let db_path = self.db_path.clone();
        let writes = Arc::clone(&self.writes);
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let scope = draft.validate()?;
                let now = unix_timestamp()?;
                let thread = ChatThread {
                    id: next_id("chat-thread")?,
                    primary_book_id: draft.primary_book_id,
                    title: draft.title.trim().to_string(),
                    scope,
                    window_kind: draft.window_kind,
                    created_at: now,
                    updated_at: now,
                };
                let row = thread_to_db(&thread)?;
                let _write = writes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("chat write lock is poisoned"))?;
                let conn = db::open_conn(&db_path)?;
                ensure!(
                    db::chat_threads::insert(&conn, &row)? == 1,
                    "chat thread was not stored"
                );
                Ok(thread)
            })
            .await
            .context("chat thread worker stopped")?
    }

    pub async fn thread(&self, thread_id: &str) -> Result<Option<ChatThread>> {
        validate_id("thread ID", thread_id)?;
        let db_path = self.db_path.clone();
        let thread_id = thread_id.to_string();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                db::chat_threads::get(&db::open_conn(&db_path)?, &thread_id)?
                    .map(thread_from_db)
                    .transpose()
            })
            .await
            .context("chat thread query worker stopped")?
    }

    pub async fn list_threads(
        &self,
        window_kind: ChatWindowKind,
        primary_book_id: Option<&str>,
    ) -> Result<Vec<ChatThread>> {
        if let Some(id) = primary_book_id {
            validate_id("primary book ID", id)?;
        }
        let db_path = self.db_path.clone();
        let primary_book_id = primary_book_id.map(str::to_string);
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let rows = db::chat_threads::list_for_book(
                    &db::open_conn(&db_path)?,
                    primary_book_id.as_deref(),
                )?;
                rows.into_iter()
                    .filter(|row| row.window_kind == window_kind.as_str())
                    .map(thread_from_db)
                    .collect()
            })
            .await
            .context("chat thread list worker stopped")?
    }

    pub async fn update_thread(
        &self,
        thread_id: &str,
        title: impl Into<String>,
        scope: ChatScope,
    ) -> Result<ChatThread> {
        validate_id("thread ID", thread_id)?;
        let title = title.into();
        validate_title(&title)?;
        let scope = scope.normalized()?;
        let db_path = self.db_path.clone();
        let thread_id = thread_id.to_string();
        let writes = Arc::clone(&self.writes);
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let _write = writes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("chat write lock is poisoned"))?;
                let conn = db::open_conn(&db_path)?;
                let current = db::chat_threads::get(&conn, &thread_id)?
                    .context("chat thread does not exist")?;
                if matches!(current.window_kind.as_str(), "reader" | "editor") {
                    let primary = current
                        .book_id
                        .as_deref()
                        .context("Reader/Editor chat lost its primary book")?;
                    ensure!(
                        scope.contains(primary),
                        "Reader/Editor chat scope must contain its primary book"
                    );
                }
                let scope_json =
                    serde_json::to_string(&scope).context("failed to serialize chat scope")?;
                let now = unix_timestamp()?;
                ensure!(
                    db::chat_threads::update_context(
                        &conn,
                        &thread_id,
                        title.trim(),
                        &scope_json,
                        &current.window_kind,
                        now,
                    )? == 1,
                    "chat thread does not exist"
                );
                thread_from_db(
                    db::chat_threads::get(&conn, &thread_id)?
                        .context("updated chat thread disappeared")?,
                )
            })
            .await
            .context("chat thread update worker stopped")?
    }

    /// Appends one complete message. The row and all citations become visible
    /// together through `db::transactions::insert_chat_message`.
    pub async fn append_message(
        &self,
        thread_id: &str,
        draft: NewChatMessage,
    ) -> Result<StoredChatMessage> {
        validate_id("thread ID", thread_id)?;
        validate_message(&draft)?;
        let db_path = self.db_path.clone();
        let thread_id = thread_id.to_string();
        let writes = Arc::clone(&self.writes);
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let _write = writes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("chat write lock is poisoned"))?;
                let mut conn = db::open_conn(&db_path)?;
                let thread_row = db::chat_threads::get(&conn, &thread_id)?
                    .context("chat thread does not exist")?;
                let thread = thread_from_db(thread_row)?;
                if let Some(parent_id) = draft.parent_id.as_deref() {
                    let parent = db::chat_messages::get(&conn, parent_id)?
                        .context("parent chat message does not exist")?;
                    ensure!(
                        parent.thread_id == thread_id,
                        "parent chat message belongs to another thread"
                    );
                }
                for citation in &draft.citations {
                    validate_current_citation(&conn, citation, &thread.scope)?;
                }

                let ordinal = db::chat_messages::list_for_thread(&conn, &thread_id)?.len();
                let now = unix_timestamp()?;
                let message_id = next_id("chat-message")?;
                let message = db::chat_messages::ChatMessage {
                    id: message_id.clone(),
                    thread_id: thread_id.clone(),
                    parent_id: draft.parent_id.clone(),
                    ordinal,
                    role: role_name(draft.role).to_string(),
                    content: draft.content.clone(),
                    model: normalized_optional(draft.model.as_deref()),
                    created_at: now,
                };
                let mut stored_citations = Vec::with_capacity(draft.citations.len());
                let mut citation_rows = Vec::with_capacity(draft.citations.len());
                for (ordinal, citation) in draft.citations.into_iter().enumerate() {
                    let id = next_id("chat-citation")?;
                    citation_rows.push(db::chat_citations::ChatCitation {
                        id: id.clone(),
                        message_id: message_id.clone(),
                        content_unit_id: citation.content_unit_id.clone(),
                        search_chunk_id: citation.search_chunk_id.clone(),
                        ordinal,
                        quote: citation.quote.clone(),
                        document_revision: citation.document_revision.get(),
                        unit_revision: citation.unit_revision.get(),
                        locator_json: serde_json::to_string(&citation.locator)
                            .context("failed to serialize citation locator")?,
                        created_at: now,
                    });
                    stored_citations.push(ChatCitation {
                        id,
                        content_unit_id: citation.content_unit_id,
                        search_chunk_id: citation.search_chunk_id,
                        document_revision: citation.document_revision,
                        unit_revision: citation.unit_revision,
                        quote: citation.quote,
                        locator: citation.locator,
                        created_at: now,
                    });
                }
                db::transactions::insert_chat_message(&mut conn, &message, &citation_rows, now)?;
                Ok(StoredChatMessage {
                    id: message_id,
                    thread_id,
                    parent_id: message.parent_id,
                    ordinal,
                    role: draft.role,
                    content: message.content,
                    model: message.model,
                    created_at: now,
                    citations: stored_citations,
                })
            })
            .await
            .context("chat message worker stopped")?
    }

    pub async fn session(&self, thread_id: &str) -> Result<Option<ChatSession>> {
        validate_id("thread ID", thread_id)?;
        let db_path = self.db_path.clone();
        let thread_id = thread_id.to_string();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let Some(thread) = db::chat_threads::get(&conn, &thread_id)? else {
                    return Ok(None);
                };
                let thread = thread_from_db(thread)?;
                let mut messages = Vec::new();
                for message in db::chat_messages::list_for_thread(&conn, &thread_id)? {
                    let citations = db::chat_citations::list_for_message(&conn, &message.id)?
                        .into_iter()
                        .map(citation_from_db)
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .filter(|citation| {
                            thread.scope.contains(&citation.locator.book_id)
                                && citation
                                    .content_unit_id
                                    .as_deref()
                                    .is_none_or(|unit_id| unit_id == citation.locator.unit_id)
                        })
                        .collect();
                    messages.push(message_from_db(message, citations)?);
                }
                Ok(Some(ChatSession { thread, messages }))
            })
            .await
            .context("chat session restore worker stopped")?
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<bool> {
        validate_id("thread ID", thread_id)?;
        let db_path = self.db_path.clone();
        let thread_id = thread_id.to_string();
        let writes = Arc::clone(&self.writes);
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let _write = writes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("chat write lock is poisoned"))?;
                Ok(db::chat_threads::delete(&db::open_conn(&db_path)?, &thread_id)? == 1)
            })
            .await
            .context("chat thread delete worker stopped")?
    }
}

fn thread_to_db(thread: &ChatThread) -> Result<db::chat_threads::ChatThread> {
    Ok(db::chat_threads::ChatThread {
        id: thread.id.clone(),
        book_id: thread.primary_book_id.clone(),
        title: thread.title.clone(),
        scope_json: serde_json::to_string(&thread.scope)
            .context("failed to serialize chat scope")?,
        window_kind: thread.window_kind.as_str().to_string(),
        created_at: thread.created_at,
        updated_at: thread.updated_at,
    })
}

fn thread_from_db(row: db::chat_threads::ChatThread) -> Result<ChatThread> {
    let scope = serde_json::from_str::<ChatScope>(&row.scope_json)
        .context("stored chat scope is invalid")?
        .normalized()?;
    let window_kind = ChatWindowKind::parse(&row.window_kind)?;
    let thread = ChatThread {
        id: row.id,
        primary_book_id: row.book_id,
        title: row.title,
        scope,
        window_kind,
        created_at: row.created_at,
        updated_at: row.updated_at,
    };
    NewChatThread {
        primary_book_id: thread.primary_book_id.clone(),
        title: thread.title.clone(),
        scope: thread.scope.clone(),
        window_kind: thread.window_kind,
    }
    .validate()?;
    Ok(thread)
}

fn citation_from_db(row: db::chat_citations::ChatCitation) -> Result<ChatCitation> {
    let locator = serde_json::from_str::<DocumentLocator>(&row.locator_json)
        .context("stored citation locator is invalid")?;
    locator
        .validate()
        .context("stored citation locator is invalid")?;
    Ok(ChatCitation {
        id: row.id,
        content_unit_id: row.content_unit_id,
        search_chunk_id: row.search_chunk_id,
        document_revision: Revision::new(row.document_revision),
        unit_revision: Revision::new(row.unit_revision),
        quote: row.quote,
        locator,
        created_at: row.created_at,
    })
}

fn message_from_db(
    row: db::chat_messages::ChatMessage,
    citations: Vec<ChatCitation>,
) -> Result<StoredChatMessage> {
    Ok(StoredChatMessage {
        id: row.id,
        thread_id: row.thread_id,
        parent_id: row.parent_id,
        ordinal: row.ordinal,
        role: parse_role(&row.role)?,
        content: row.content,
        model: row.model,
        created_at: row.created_at,
        citations,
    })
}

fn validate_message(message: &NewChatMessage) -> Result<()> {
    ensure!(
        !message.content.trim().is_empty(),
        "chat message cannot be empty"
    );
    ensure!(
        message.content.len() <= MAX_MESSAGE_BYTES,
        "chat message exceeds {MAX_MESSAGE_BYTES} bytes"
    );
    if let Some(parent_id) = message.parent_id.as_deref() {
        validate_id("parent message ID", parent_id)?;
    }
    if let Some(model) = message.model.as_deref() {
        ensure!(
            !model.trim().is_empty() && !model.chars().any(char::is_control),
            "chat model is invalid"
        );
    }
    ensure!(
        message.citations.len() <= MAX_CITATIONS,
        "chat message exceeds {MAX_CITATIONS} citations"
    );
    Ok(())
}

fn validate_citation(citation: &NewChatCitation, scope: &ChatScope) -> Result<()> {
    citation
        .locator
        .validate()
        .context("citation locator is invalid")?;
    ensure!(
        scope.contains(&citation.locator.book_id),
        "citation is outside the persisted chat scope"
    );
    ensure!(
        !matches!(
            citation.locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        ),
        "preview-only Office rendered pages cannot be persisted as citations"
    );
    ensure!(
        !citation.quote.trim().is_empty(),
        "citation quote cannot be empty"
    );
    ensure!(
        citation.quote.len() <= MAX_QUOTE_BYTES,
        "citation quote exceeds {MAX_QUOTE_BYTES} bytes"
    );
    if let Some(content_unit_id) = citation.content_unit_id.as_deref() {
        validate_id("citation content unit ID", content_unit_id)?;
        ensure!(
            content_unit_id == citation.locator.unit_id,
            "citation content unit does not match its locator"
        );
    }
    if let Some(search_chunk_id) = citation.search_chunk_id.as_deref() {
        validate_id("citation search chunk ID", search_chunk_id)?;
    }
    Ok(())
}

fn validate_current_citation(
    conn: &rusqlite::Connection,
    citation: &NewChatCitation,
    scope: &ChatScope,
) -> Result<()> {
    validate_citation(citation, scope)?;
    let book = db::books::get(conn, &citation.locator.book_id)?
        .context("citation book no longer exists")?;
    ensure!(
        book.revision == citation.document_revision.get(),
        "citation document revision is stale"
    );
    let source = db::book_sources::get_revision(conn, &book.id, book.revision)?
        .context("citation document source no longer exists")?;
    let unit = db::content_units::get(conn, &citation.locator.unit_id)?
        .context("citation content unit no longer exists")?;
    ensure!(
        unit.book_id == book.id && unit.source_id == source.id,
        "citation content unit is outside the current document revision"
    );
    ensure!(
        unit.revision == citation.unit_revision.get(),
        "citation content unit revision is stale"
    );
    if let Some(chunk_id) = citation.search_chunk_id.as_deref() {
        let chunk = db::search_chunks::get(conn, chunk_id)?
            .context("citation search passage no longer exists")?;
        ensure!(
            chunk.book_id == book.id
                && chunk.source_id == source.id
                && chunk.content_unit_id == unit.id,
            "citation search passage does not match its current book and content unit"
        );
        let chunk_locator = serde_json::from_str::<DocumentLocator>(&chunk.locator_json)
            .context("citation search passage locator is invalid")?;
        ensure!(
            chunk_locator == citation.locator,
            "citation locator does not match its search passage"
        );
    }
    Ok(())
}

fn role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn parse_role(value: &str) -> Result<ChatRole> {
    match value {
        "system" => Ok(ChatRole::System),
        "user" => Ok(ChatRole::User),
        "assistant" => Ok(ChatRole::Assistant),
        "tool" => Ok(ChatRole::Tool),
        _ => bail!("unknown chat role: {value}"),
    }
}

fn validate_id(label: &str, value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.trim() == value
            && value.len() <= 512
            && !value.chars().any(char::is_control),
        "{label} is invalid"
    );
    Ok(())
}

fn validate_title(title: &str) -> Result<()> {
    ensure!(!title.trim().is_empty(), "chat title cannot be empty");
    ensure!(
        title.chars().count() <= MAX_TITLE_CHARS && !title.chars().any(char::is_control),
        "chat title is invalid"
    );
    Ok(())
}

fn normalized_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_secs())
}

fn next_id(namespace: &str) -> Result<String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(deterministic_id(
        namespace,
        format!("{}:{}:{}", elapsed.as_nanos(), std::process::id(), sequence),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{books, content_units, search_chunks},
        document::{BlockDocument, SourceLocator},
    };

    fn repository() -> (tempfile::TempDir, IoRuntime, ChatRepository) {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        db::open_or_recreate(&db_path).unwrap();
        let runtime = IoRuntime::new(1).unwrap();
        let repository = ChatRepository::new(db_path, runtime.clone());
        (temp, runtime, repository)
    }

    fn insert_book_graph(db_path: &Path) {
        let conn = db::open_conn(db_path).unwrap();
        let source_key = format!("blake3/00/{}", "0".repeat(64));
        db::blobs::insert(
            &conn,
            &db::blobs::BlobRecord {
                object_key: source_key.clone(),
                media_type: "application/epub+zip".to_string(),
                byte_len: 1,
                hash: "0".repeat(64),
                created_at: 1,
            },
        )
        .unwrap();
        books::insert(
            &conn,
            &books::BookRecord {
                id: "book-1".to_string(),
                title: "Book".to_string(),
                author: "Author".to_string(),
                language: None,
                description: None,
                format: "epub".to_string(),
                revision: 1,
                source_object_key: source_key.clone(),
                cover_asset_id: None,
                cover_object_key: None,
                cover_mime: None,
                added_at: 1,
                updated_at: 1,
                group_id: None,
                last_spine: 0,
            },
        )
        .unwrap();
        db::book_sources::insert(
            &conn,
            &db::book_sources::BookSource {
                id: "source-1".to_string(),
                book_id: "book-1".to_string(),
                revision: 1,
                format: "epub".to_string(),
                source_kind: "original".to_string(),
                object_key: source_key,
                source_name: None,
                created_at: 1,
            },
        )
        .unwrap();
        content_units::insert(
            &conn,
            &content_units::ContentUnit {
                id: "unit-1".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                parent_id: None,
                ordinal: 0,
                kind: "chapter".to_string(),
                source_kind: "markdown".to_string(),
                href: Some("chapter-1.md".to_string()),
                source_locator_json: serde_json::to_string(&SourceLocator::Created).unwrap(),
                title: Some("Chapter".to_string()),
                media_type: Some("text/markdown".to_string()),
                source_text: Some("text".to_string()),
                block_json: serde_json::to_string(&BlockDocument::default()).unwrap(),
                revision: 1,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();
        search_chunks::insert(
            &conn,
            &search_chunks::SearchChunk {
                id: "chunk-1".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                content_unit_id: "unit-1".to_string(),
                ordinal: 0,
                heading: "Chapter".to_string(),
                body: "quoted text".to_string(),
                token_count: 2,
                locator_json: serde_json::to_string(&DocumentLocator::unit("book-1", "unit-1"))
                    .unwrap(),
                content_hash: blake3::hash(b"quoted text").to_hex().to_string(),
                created_at: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn persists_and_restores_an_independent_window_session() {
        let (_temp, runtime, repository) = repository();
        insert_book_graph(repository.database_path());
        runtime.block_on(async {
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: Some("book-1".to_string()),
                    title: "Reading notes".to_string(),
                    scope: ChatScope::new(["book-1"]).unwrap(),
                    window_kind: ChatWindowKind::Reader,
                })
                .await
                .unwrap();
            let user = repository
                .append_message(
                    &thread.id,
                    NewChatMessage::text(ChatRole::User, "What is this chapter about?"),
                )
                .await
                .unwrap();
            let answer = repository
                .append_message(
                    &thread.id,
                    NewChatMessage {
                        parent_id: Some(user.id.clone()),
                        role: ChatRole::Assistant,
                        content: "An answer".to_string(),
                        model: Some("chat-model".to_string()),
                        citations: vec![NewChatCitation {
                            content_unit_id: Some("unit-1".to_string()),
                            search_chunk_id: Some("chunk-1".to_string()),
                            document_revision: Revision::new(1),
                            unit_revision: Revision::new(1),
                            quote: "quoted text".to_string(),
                            locator: DocumentLocator::unit("book-1", "unit-1"),
                        }],
                    },
                )
                .await
                .unwrap();

            assert_eq!(answer.ordinal, 1);
            let restored = repository.session(&thread.id).await.unwrap().unwrap();
            assert_eq!(restored.thread.id, thread.id);
            assert_eq!(restored.thread.scope, thread.scope);
            assert_eq!(restored.messages.len(), 2);
            assert_eq!(restored.messages[1].citations.len(), 1);
            assert_eq!(restored.messages[1].citations[0].quote, "quoted text");
            assert_eq!(
                restored.messages[1].citations[0].document_revision,
                Revision::new(1)
            );
            assert_eq!(
                restored.messages[1].citations[0].unit_revision,
                Revision::new(1)
            );
            let listed = repository
                .list_threads(ChatWindowKind::Reader, Some("book-1"))
                .await
                .unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].id, thread.id);
            assert_eq!(listed[0].scope, thread.scope);
            assert_eq!(listed[0].window_kind, thread.window_kind);
            assert!(
                listed[0].updated_at >= thread.updated_at,
                "appending messages must not move the thread timestamp backwards"
            );
        });
    }

    #[test]
    fn delete_thread_cascades_messages_and_citations_and_is_idempotent() {
        let (_temp, runtime, repository) = repository();
        insert_book_graph(repository.database_path());
        runtime.block_on(async {
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: Some("book-1".to_string()),
                    title: "Delete me".to_string(),
                    scope: ChatScope::new(["book-1"]).unwrap(),
                    window_kind: ChatWindowKind::Reader,
                })
                .await
                .unwrap();
            let user = repository
                .append_message(
                    &thread.id,
                    NewChatMessage::text(ChatRole::User, "Delete this conversation"),
                )
                .await
                .unwrap();
            let answer = repository
                .append_message(
                    &thread.id,
                    NewChatMessage {
                        parent_id: Some(user.id.clone()),
                        role: ChatRole::Assistant,
                        content: "Deleted answer".to_string(),
                        model: Some("chat-model".to_string()),
                        citations: vec![NewChatCitation {
                            content_unit_id: Some("unit-1".to_string()),
                            search_chunk_id: Some("chunk-1".to_string()),
                            document_revision: Revision::new(1),
                            unit_revision: Revision::new(1),
                            quote: "quoted text".to_string(),
                            locator: DocumentLocator::unit("book-1", "unit-1"),
                        }],
                    },
                )
                .await
                .unwrap();
            let citation_id = answer.citations[0].id.clone();

            assert!(repository.delete_thread(&thread.id).await.unwrap());
            assert!(repository.thread(&thread.id).await.unwrap().is_none());
            assert!(repository.session(&thread.id).await.unwrap().is_none());

            let conn = db::open_conn(repository.database_path()).unwrap();
            assert!(db::chat_messages::get(&conn, &user.id).unwrap().is_none());
            assert!(db::chat_messages::get(&conn, &answer.id).unwrap().is_none());
            assert!(
                db::chat_citations::get(&conn, &citation_id)
                    .unwrap()
                    .is_none()
            );
            drop(conn);

            assert!(!repository.delete_thread(&thread.id).await.unwrap());
        });
    }

    #[test]
    fn message_and_citations_roll_back_together() {
        let (_temp, runtime, repository) = repository();
        insert_book_graph(repository.database_path());
        runtime.block_on(async {
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: Some("book-1".to_string()),
                    title: "Atomic".to_string(),
                    scope: ChatScope::new(["book-1"]).unwrap(),
                    window_kind: ChatWindowKind::Editor,
                })
                .await
                .unwrap();
            let result = repository
                .append_message(
                    &thread.id,
                    NewChatMessage {
                        parent_id: None,
                        role: ChatRole::Assistant,
                        content: "Will roll back".to_string(),
                        model: None,
                        citations: vec![NewChatCitation {
                            content_unit_id: Some("unit-1".to_string()),
                            search_chunk_id: Some("missing-chunk".to_string()),
                            document_revision: Revision::new(1),
                            unit_revision: Revision::new(1),
                            quote: "quote".to_string(),
                            locator: DocumentLocator::unit("book-1", "unit-1"),
                        }],
                    },
                )
                .await;
            assert!(result.is_err());
            assert!(
                repository
                    .session(&thread.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .messages
                    .is_empty()
            );
        });
    }

    #[test]
    fn reader_scope_cannot_drop_current_book_or_store_foreign_citation() {
        let (_temp, runtime, repository) = repository();
        insert_book_graph(repository.database_path());
        runtime.block_on(async {
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: Some("book-1".to_string()),
                    title: "Scoped".to_string(),
                    scope: ChatScope::new(["book-1"]).unwrap(),
                    window_kind: ChatWindowKind::Reader,
                })
                .await
                .unwrap();
            assert!(
                repository
                    .update_thread(&thread.id, "Scoped", ChatScope::default())
                    .await
                    .is_err()
            );
            assert!(
                repository
                    .append_message(
                        &thread.id,
                        NewChatMessage {
                            parent_id: None,
                            role: ChatRole::Assistant,
                            content: "No".to_string(),
                            model: None,
                            citations: vec![NewChatCitation {
                                content_unit_id: None,
                                search_chunk_id: None,
                                document_revision: Revision::new(1),
                                unit_revision: Revision::new(1),
                                quote: "foreign".to_string(),
                                locator: DocumentLocator::unit("book-2", "unit-x"),
                            }],
                        },
                    )
                    .await
                    .is_err()
            );
        });
    }
}
