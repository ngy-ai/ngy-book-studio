//! The chat repository: threads, messages and citations.

use super::*;

/// Async repository backed by one SQLite connection per operation.
///
/// The mutex serializes ordinal allocation across cloned handles in this
/// process. SQLite's unique index remains the final consistency guard.
#[derive(Clone, Debug)]
pub struct ChatRepository {
    pub(crate) db_path: PathBuf,
    pub(crate) runtime: IoRuntime,
    pub(crate) writes: Arc<Mutex<()>>,
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
                        locator_json: if citation.source_kind.is_web() {
                            None
                        } else {
                            Some(
                                serde_json::to_string(&citation.locator)
                                    .context("failed to serialize citation locator")?,
                            )
                        },
                        source_kind: match citation.source_kind {
                            AgentCitationSourceKind::Web => "web".to_string(),
                            AgentCitationSourceKind::Book => "book".to_string(),
                        },

                        url: citation.url.clone(),
                        source_title: citation.source_title.clone(),
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
                        source_kind: citation.source_kind,
                        url: citation.url.clone(),
                        source_title: citation.source_title.clone(),
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
                            citation.source_kind.is_web()
                                || (thread.scope.contains(&citation.locator.book_id)
                                    && citation
                                        .content_unit_id
                                        .as_deref()
                                        .is_none_or(|unit_id| unit_id == citation.locator.unit_id))
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
