//! The answer loop and the thread/history assembly behind it.

use super::*;

impl AgentConversation {
    pub(super) async fn ask_inner(
        &self,
        request: ConversationQuestion,
        events: Option<mpsc::UnboundedSender<AgentRunEvent>>,
        cancellation: AgentCancellation,
        ai_snapshot: AiRequestSnapshot,
    ) -> Result<ConversationAnswer> {
        let scope = conversation_stage("authorize_scope", async {
            ensure_request_not_cancelled(&cancellation)?;
            let scope = ChatScope::new(request.allowed_book_ids.iter().cloned())?;
            match self.window_kind {
                ChatWindowKind::Library => {}
                ChatWindowKind::Reader | ChatWindowKind::Editor => {
                    let primary = self
                        .primary_book_id
                        .as_deref()
                        .context("conversation primary book is missing")?;
                    ensure!(
                        scope.contains(primary),
                        "host scope does not contain the required current book"
                    );
                }
            }
            Ok(scope)
        })
        .await?;

        // Resolve frozen editor text against the single shared library before
        // it reaches the provider. This is the trust transition that supplies
        // current titles/revisions and issues a host citation ID; raw snapshot
        // JSON alone is never a persistable source.
        let snapshot_scope = scope.clone();
        let snapshot_cancellation = cancellation.clone();
        let snapshots = conversation_stage("authorize_snapshots", async {
            self.services
                .spawn_library_read(move |library| {
                    ensure_request_not_cancelled(&snapshot_cancellation)?;
                    authorize_selection_snapshots(library, &snapshot_scope, request.snapshots)
                })
                .await
                .context("frozen selection authorization worker stopped")?
        })
        .await?;
        ensure_request_not_cancelled(&cancellation)?;

        let repository = self.services.chat();
        let (thread_id, mut previous) = conversation_stage(
            "load_thread_history",
            self.thread_and_history(&repository, &request.question, scope.clone()),
        )
        .await?;
        ensure_request_not_cancelled(&cancellation)?;
        if let Some(anchor) = request.truncate_from.as_deref() {
            // Editing a question or regenerating an answer rewinds the stored
            // thread first: the replacement turn then extends the same prefix
            // instead of duplicating the one it replaces. The anchor comes
            // from the UI's own persisted-ID bookkeeping; an anchor that no
            // longer exists (another window rewrote the thread) is skipped
            // rather than guessed at.
            let cutoff = conversation_stage("rewind_answer_branch", async {
                repository
                    .truncate_from(&thread_id, anchor)
                    .await
                    .context("failed to rewind the replaced answer branch")
            })
            .await?;
            match cutoff {
                Some(cutoff) => {
                    previous.retain(|message| message.ordinal < cutoff);
                    tracing::debug!(
                        target: "ngy_ai",
                        stage = "rewind_answer_branch",
                        history_messages = previous.len(),
                        "AI conversation history rewound for the replacement question"
                    );
                }
                None => {
                    tracing::warn!(
                        target: "ngy_ai",
                        stage = "rewind_answer_branch",
                        error_kind = "stale_anchor",
                        "replaced answer anchor is gone; keeping the stored history"
                    );
                }
            }
        }
        tracing::debug!(target: "ngy_ai", history_messages = previous.len(), "AI conversation history loaded");
        ensure_request_not_cancelled(&cancellation)?;
        let mut user_draft = NewChatMessage::text(ChatRole::User, request.question.clone());
        user_draft.parent_id = previous.last().map(|message| message.id.clone());
        ensure_request_not_cancelled(&cancellation)?;
        let user_message = conversation_stage("persist_user_question", async {
            repository
                .append_message(&thread_id, user_draft)
                .await
                .context("failed to persist the user question")
        })
        .await?;
        ensure_request_not_cancelled(&cancellation)?;

        let (settings, runtime) = conversation_stage("configure_runtime", async {
            let settings = ai_snapshot.settings;
            let search_backend: Arc<dyn SearchBackend> = ai_snapshot.search;
            let web_search = self.services.web_search_backend()?;
            let runtime = AgentRuntime::for_database(
                ai_snapshot.provider,
                search_backend,
                self.services.database_path(),
                settings.chat_model.clone(),
                web_search,
            )?
            .with_chat_generation(settings.chat_generation.clone())?;
            Ok((settings, runtime))
        })
        .await?;
        ensure_request_not_cancelled(&cancellation)?;
        let answer = conversation_stage(
            "run_agent",
            runtime.answer(
                AgentQuestion {
                    question: request.question,
                    allowed_book_ids: scope.book_ids,
                    book_titles: request.book_titles,
                    history: history_messages(&previous),
                    snapshots,
                },
                events,
                cancellation.clone(),
            ),
        )
        .await?;
        ensure_request_not_cancelled(&cancellation)?;

        let citations = answer
            .citations
            .iter()
            .map(|citation| NewChatCitation {
                content_unit_id: if citation.is_web() {
                    None
                } else {
                    Some(citation.unit_id.clone())
                },
                search_chunk_id: citation
                    .citation_id
                    .strip_prefix("passage:")
                    .map(str::to_string),
                document_revision: citation.document_revision,
                unit_revision: citation.unit_revision,
                quote: citation.quote.clone(),
                locator: citation.locator.clone(),
                source_kind: citation.source_kind,
                url: citation.url.clone(),
                source_title: citation.source_title.clone(),
            })
            .collect();
        tracing::debug!(target: "ngy_ai", answer_bytes = answer.markdown.len(), citations = answer.citations.len(), "AI answer ready for persistence");
        let stored_message = conversation_stage("persist_answer_citations", async {
            repository
                .append_message(
                    &thread_id,
                    NewChatMessage {
                        parent_id: Some(user_message.id.clone()),
                        role: ChatRole::Assistant,
                        content: answer.markdown.clone(),
                        model: Some(settings.chat_model),
                        citations,
                    },
                )
                .await
                .context("failed to persist the AI answer")
        })
        .await?;

        Ok(ConversationAnswer {
            thread_id,
            answer,
            stored_message,
            user_message_id: user_message.id,
        })
    }

    pub(super) async fn thread_and_history(
        &self,
        repository: &ChatRepository,
        question: &str,
        scope: ChatScope,
    ) -> Result<(String, Vec<StoredChatMessage>)> {
        let mut selection = self.selection.lock().await;
        let existing = match selection.clone() {
            ConversationSelection::Selected(thread_id) => {
                let session = repository.session(&thread_id).await?;
                if let Some(session) = session.as_ref() {
                    self.validate_session_context(&session.thread)?;
                } else {
                    self.lease.clear_claim()?;
                    *selection = ConversationSelection::Fresh;
                }
                session
            }
            ConversationSelection::Uninitialized => {
                let session = self.claim_recent_session(repository, &scope).await?;
                *selection = session
                    .as_ref()
                    .map_or(ConversationSelection::Fresh, |session| {
                        ConversationSelection::Selected(session.thread.id.clone())
                    });
                session
            }
            ConversationSelection::Fresh => None,
        };
        if let Some(session) = existing {
            if scope_excludes_prior_books(&session.thread.scope, &scope) {
                // Messages do not carry a per-message authority snapshot. If
                // the host removes a book, reusing this history would send
                // text derived from that now-unauthorized book back to the
                // provider even though subsequent tools are correctly scoped.
                // Preserve the old conversation as an auditable session and
                // start a clean one for the narrowed authority set.
                self.lease.clear_claim()?;
                *selection = ConversationSelection::Fresh;
            } else {
                let thread = repository
                    .update_thread(&session.thread.id, session.thread.title, scope)
                    .await?;
                *selection = ConversationSelection::Selected(thread.id.clone());
                return Ok((thread.id, session.messages));
            }
        }

        for _ in 0..CREATE_CLAIM_ATTEMPTS {
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: self.primary_book_id.clone(),
                    title: question_title(question),
                    scope: scope.clone(),
                    window_kind: self.window_kind,
                })
                .await?;
            if self.lease.switch_claim(&thread.id)? {
                *selection = ConversationSelection::Selected(thread.id.clone());
                return Ok((thread.id, Vec::new()));
            }
        }
        bail!("could not reserve an independent conversation for this window")
    }
}
