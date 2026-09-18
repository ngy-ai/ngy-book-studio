//! Construction and the persisted session lifecycle.

use super::*;

impl AgentConversation {
    pub fn new(
        services: Arc<AppServices>,
        window_kind: ChatWindowKind,
        primary_book_id: Option<String>,
    ) -> Result<Self> {
        match window_kind {
            ChatWindowKind::Library => ensure!(
                primary_book_id.is_none(),
                "library conversation cannot have a primary book"
            ),
            ChatWindowKind::Reader | ChatWindowKind::Editor => ensure!(
                primary_book_id
                    .as_ref()
                    .is_some_and(|id| !id.trim().is_empty()),
                "Reader/Editor conversation requires a primary book"
            ),
        }
        let lease = Arc::new(ConversationLease::new(services.database_path()));
        Ok(Self {
            services,
            window_kind,
            primary_book_id,
            selection: Arc::new(AsyncMutex::new(ConversationSelection::Uninitialized)),
            lease,
            active: Arc::new(Mutex::new(HashMap::new())),
            session_transition: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Restores the most recently used conversation for this exact window
    /// kind/current book whose persisted scope remains inside the current
    /// host-authorized scope. A missing session is normal before the first
    /// submit.
    pub async fn restore(&self, allowed_book_ids: &[String]) -> Result<Option<ChatSession>> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        match selection.clone() {
            ConversationSelection::Fresh => return Ok(None),
            ConversationSelection::Selected(thread_id) => {
                if let Some(session) = repository.session(&thread_id).await? {
                    self.validate_session_context(&session.thread)?;
                    if self.thread_scope_is_authorized(&session.thread, &host_scope) {
                        return Ok(Some(session));
                    }
                }
                self.lease.clear_claim()?;
                *selection = ConversationSelection::Uninitialized;
            }
            ConversationSelection::Uninitialized => {}
        }
        let session = self.claim_recent_session(&repository, &host_scope).await?;
        *selection = session
            .as_ref()
            .map_or(ConversationSelection::Fresh, |session| {
                ConversationSelection::Selected(session.thread.id.clone())
            });
        Ok(session)
    }

    /// Lists persisted sessions that belong to this window kind/current book
    /// and whose scope is fully contained by the current host authorization.
    pub async fn list_sessions(&self, allowed_book_ids: &[String]) -> Result<Vec<ChatThread>> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let threads = self
            .services
            .chat()
            .list_threads(self.window_kind, self.primary_book_id.as_deref())
            .await?;
        let mut authorized = Vec::with_capacity(threads.len());
        for thread in threads {
            self.validate_session_context(&thread)?;
            if self.thread_scope_is_authorized(&thread, &host_scope) {
                authorized.push(thread);
            }
        }
        Ok(authorized)
    }

    /// Selects one persisted session without releasing the current session if
    /// another live window already owns the requested thread.
    pub async fn select_session(
        &self,
        thread_id: &str,
        allowed_book_ids: &[String],
    ) -> Result<ChatSession> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        let mut reservation = self
            .lease
            .reserve_claim(thread_id)?
            .context("conversation is already open in another window")?;
        let session = repository
            .session(thread_id)
            .await?
            .context("conversation does not exist")?;
        self.validate_session_context(&session.thread)?;
        ensure!(
            self.thread_scope_is_authorized(&session.thread, &host_scope),
            "conversation scope exceeds the current host-authorized scope"
        );
        reservation.promote_to_selection(&self.lease)?;
        *selection = ConversationSelection::Selected(session.thread.id.clone());
        Ok(session)
    }

    /// Starts a clean conversation. The database thread is created lazily on
    /// first submit so repeated clicks do not leave empty persisted sessions.
    pub async fn start_new_session(&self) -> Result<()> {
        let _transition = self.begin_session_transition()?;
        let mut selection = self.selection.lock().await;
        self.lease.clear_claim()?;
        *selection = ConversationSelection::Fresh;
        Ok(())
    }

    /// Permanently deletes one authorized persisted session.
    ///
    /// Deleting the selected session releases its window claim and leaves this
    /// conversation in the same explicit fresh state as
    /// [`Self::start_new_session`]. Deleting another session preserves and
    /// returns the currently selected session so callers can keep displaying
    /// its transcript. A temporary claim closes the race between checking an
    /// inactive session and deleting it; sessions open in another window are
    /// rejected.
    pub async fn delete_session(
        &self,
        thread_id: &str,
        allowed_book_ids: &[String],
    ) -> Result<Option<ChatSession>> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        let _reservation = self
            .lease
            .reserve_claim(thread_id)?
            .context("conversation is already open in another window")?;
        let target = repository
            .session(thread_id)
            .await?
            .context("conversation does not exist")?;
        self.validate_session_context(&target.thread)?;
        ensure!(
            self.thread_scope_is_authorized(&target.thread, &host_scope),
            "conversation scope exceeds the current host-authorized scope"
        );
        let deleted_selected = matches!(
            &*selection,
            ConversationSelection::Selected(active) if active == &target.thread.id
        );

        // Resolve every fallible read and authorization decision before the
        // irreversible DELETE. The selected conversation cannot change while
        // the transition/selection guards are held, and another window cannot
        // own it because this lease retains its active claim.
        let (reset_to_fresh, preserved_session) = if deleted_selected {
            (true, None)
        } else if let ConversationSelection::Selected(active_thread_id) = selection.clone() {
            match repository.session(&active_thread_id).await? {
                Some(active_session) => {
                    self.validate_session_context(&active_session.thread)?;
                    if self.thread_scope_is_authorized(&active_session.thread, &host_scope) {
                        (false, Some(active_session))
                    } else {
                        (true, None)
                    }
                }
                None => (true, None),
            }
        } else {
            (false, None)
        };

        ensure!(
            repository.delete_thread(&target.thread.id).await?,
            "conversation no longer exists"
        );
        Ok(finalize_committed_delete(
            &mut selection,
            reset_to_fresh,
            preserved_session,
            || self.lease.clear_claim(),
        ))
    }

    /// Drops the selected conversation when its persisted authority exceeds
    /// the host's current authority. The session transition guard keeps this
    /// decision mutually exclusive with active requests and other session
    /// changes, while the selection lock makes releasing the claim and
    /// switching to a fresh transcript one state transition.
    pub async fn reconcile_scope(&self, allowed_book_ids: &[String]) -> Result<bool> {
        let host_scope = self.host_scope(allowed_book_ids)?;
        let _transition = self.begin_session_transition()?;
        let repository = self.services.chat();
        let mut selection = self.selection.lock().await;
        let ConversationSelection::Selected(thread_id) = selection.clone() else {
            return Ok(false);
        };
        let session = repository.session(&thread_id).await?;
        if let Some(session) = session {
            self.validate_session_context(&session.thread)?;
            if self.thread_scope_is_authorized(&session.thread, &host_scope) {
                return Ok(false);
            }
        }

        self.lease.clear_claim()?;
        *selection = ConversationSelection::Fresh;
        Ok(true)
    }
}
