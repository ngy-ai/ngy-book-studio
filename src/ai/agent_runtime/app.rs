//! Construction and configuration of the runtime.

pub(super) use super::*;

impl AgentRuntime {
    pub fn new(
        provider: Arc<dyn OpenAiCompatibleProvider>,
        search: Arc<dyn SearchBackend>,
        books: Arc<dyn BookBackend>,
        chat_model: impl Into<String>,
        limits: AgentLimits,
    ) -> Result<Self> {
        let chat_model = chat_model.into();
        ensure!(!chat_model.trim().is_empty(), "chat model is required");
        limits.validate().map_err(anyhow::Error::new)?;
        Ok(Self {
            provider,
            search,
            books,
            web: None,
            chat_model,
            chat_generation: ChatGenerationSettings::default(),
            limits,
        })
    }

    pub fn with_chat_generation(mut self, settings: ChatGenerationSettings) -> Result<Self> {
        settings.validate()?;
        self.chat_generation = settings;
        Ok(self)
    }

    /// Attaches the host-performed web search used only when the authorized
    /// books cannot ground an answer. Passing `None` keeps web retrieval off.
    pub fn with_web_search(mut self, web: Option<Arc<dyn WebSearchBackend>>) -> Self {
        self.web = web;
        self
    }

    pub fn for_database(
        provider: Arc<dyn OpenAiCompatibleProvider>,
        search: Arc<dyn SearchBackend>,
        db_path: impl Into<PathBuf>,
        chat_model: impl Into<String>,
        web_search: Option<Arc<dyn WebSearchBackend>>,
    ) -> Result<Self> {
        let mut runtime = Self::new(
            provider,
            search,
            Arc::new(SqliteBookBackend::new(db_path)),
            chat_model,
            AgentLimits::default(),
        )?;
        if let Some(web) = web_search {
            runtime = runtime.with_web_search(Some(web));
        }
        Ok(runtime)
    }

    pub async fn answer(
        &self,
        question: AgentQuestion,
        events: Option<mpsc::UnboundedSender<AgentRunEvent>>,
        cancellation: AgentCancellation,
    ) -> Result<AgentAnswer> {
        let span = tracing::info_span!(target: "ngy_ai", "ai_run",
            trace_id = cancellation.trace_id(), model = %safe_label(&self.chat_model));
        async {
            let started = Instant::now();
            let mut stage = "prepare";
            tracing::info!(target: "ngy_ai",
                question_bytes = question.question.len(), history_messages = question.history.len(),
                scope_books = question.allowed_book_ids.len(), snapshots = question.snapshots.len(),
                max_tool_rounds = self.limits.max_tool_rounds, max_context_bytes = self.limits.max_context_bytes,
                "AI run started");
            let result = self.answer_inner(question, events, &cancellation, &mut stage).await;
            match &result {
                Ok(answer) => tracing::info!(target: "ngy_ai", elapsed_ms = started.elapsed().as_millis() as u64,
                    answer_bytes = answer.markdown.len(), citations = answer.citations.len(),
                    source_status = ?answer.source_status, "AI run completed"),
                Err(error) if error.is::<AgentRequestCancelled>() => tracing::info!(target: "ngy_ai", stage,
                    elapsed_ms = started.elapsed().as_millis() as u64, "AI run cancelled"),
                Err(error) => tracing::warn!(target: "ngy_ai", stage,
                    elapsed_ms = started.elapsed().as_millis() as u64, error_kind = error_kind(error),
                    "AI run failed"),
            }
            result
        }.instrument(span).await
    }
}
