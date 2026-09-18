//! The provider calls: completing a turn, starting a stream and the web
//! search fallback.

pub(super) use super::*;

impl AgentRuntime {
    /// Runs a host-performed web search and re-asks the model using only the
    /// retrieved pages as sources.
    ///
    /// Web search is deliberately **not** a model-facing tool: the host decides
    /// when the authorized books cannot ground an answer, so the read-only
    /// agent keeps exactly three tools and the model can never trigger network
    /// egress by itself. Returns `Ok(None)` when no usable result was found, so
    /// the caller keeps the ungrounded answer the books produced.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn web_fallback(
        &self,
        web: &dyn WebSearchBackend,
        question: &str,
        messages: &[ChatMessage],
        request_context: &mut RequestContext,
        citations: &mut CitationRegistry,
        publisher: &mut AnswerEventPublisher,
        cancellation: &AgentCancellation,
    ) -> Result<Option<AgentAnswer>> {
        if cancellation.is_cancelled() {
            bail!(AgentRequestCancelled);
        }
        let results = match web
            .web_search(WebSearchRequest {
                query: question.to_string(),
                limit: WEB_SEARCH_RESULT_LIMIT,
            })
            .await
        {
            Ok(results) => results,
            Err(error) => {
                // A misconfigured or unreachable engine must not destroy the
                // answer the books already produced.
                tracing::warn!(target: "ngy_ai", error_kind = error_kind(&error), "AI web search fallback failed");
                return Ok(None);
            }
        };
        if results.is_empty() {
            return Ok(None);
        }
        tracing::debug!(target: "ngy_ai", result_count = results.len(), "AI web search completed");

        let mut context = String::from(WEB_SEARCH_INSTRUCTIONS);
        for (index, result) in results.iter().enumerate() {
            let marker = format!("web:{index}");
            citations
                .record_citation_for_marker(
                    marker.clone(),
                    AgentCitation::web(
                        web_citation_id(&result.url),
                        result.title.clone(),
                        result.url.clone(),
                        result.snippet.clone(),
                    ),
                )
                .map_err(anyhow::Error::new)?;
            context.push_str(&format!(
                "\n\n[{marker}] {title}\n{url}\n{snippet}",
                title = result.title,
                url = result.url,
                snippet = result.snippet,
            ));
        }

        // Withdraw the provisional book-only answer before streaming the
        // grounded replacement.
        publisher.reset();

        let mut messages = messages.to_vec();
        messages.push(ChatMessage::text(ChatRole::User, context));
        let mut projection = StreamingAnswerProjection::default();
        let content = self
            .complete(
                messages,
                request_context,
                citations,
                &mut projection,
                publisher,
                cancellation,
            )
            .await?;

        let (markdown, citation_ids, no_source) = extract_citation_markers(&content)?;
        ensure!(
            !no_source || citation_ids.is_empty(),
            "AI answer cannot combine source citations with the no-source marker"
        );
        ensure!(
            !markdown.trim().is_empty(),
            "AI endpoint returned an empty answer after removing source markers"
        );
        let answer = citations
            .finish(markdown, &citation_ids)
            .map_err(anyhow::Error::new)?;
        let remainder = projection.finish(&answer.markdown)?;
        publisher.delta(remainder);
        publisher.commit();
        Ok(Some(answer))
    }

    /// Streams a single tool-free completion and returns the raw model content.
    pub(super) async fn complete(
        &self,
        messages: Vec<ChatMessage>,
        context: &mut RequestContext,
        citations: &mut CitationRegistry,
        projection: &mut StreamingAnswerProjection,
        publisher: &mut AnswerEventPublisher,
        cancellation: &AgentCancellation,
    ) -> Result<String> {
        let mut request = ChatRequest {
            model: self.chat_model.clone(),
            messages,
            tools: Vec::new(),
            temperature: self.chat_generation.temperature,
            top_p: self.chat_generation.top_p,
            presence_penalty: self.chat_generation.presence_penalty,
            frequency_penalty: self.chat_generation.frequency_penalty,
            max_tokens: Some(context.max_output_tokens),
            reasoning_effort: Some(ReasoningEffort::None),
        };
        let mut stream = self
            .start_stream(&mut request, context, cancellation)
            .await?;
        retain_request_citations(citations, &request.messages)?;
        let mut accumulator = AgentStreamAccumulator::new(
            MAX_ANSWER_BYTES,
            MAX_TOOL_CALLS_PER_TURN,
            MAX_TOOL_ARGUMENT_BYTES,
        )
        .map_err(anyhow::Error::new)?;
        loop {
            let item = tokio::select! {
                item = stream.next() => item,
                _ = cancellation.cancelled() => bail!(AgentRequestCancelled),
            };
            let Some(item) = item else { break };
            let event = item.context("AI response stream failed")?;
            accumulator.push(&event).map_err(anyhow::Error::new)?;
            if let Some(delta) = event.content_delta.as_deref() {
                let visible = projection.push(delta);
                publisher.delta(visible);
            }
            if event.done {
                break;
            }
        }
        let turn = accumulator.finish().map_err(anyhow::Error::new)?;
        ensure!(
            turn.completed || turn.finish_reason.is_some(),
            "AI response stream ended before a completion marker"
        );
        ensure!(
            turn.tool_calls.is_empty(),
            "AI endpoint returned tool calls when no tools were offered"
        );
        Ok(turn.content)
    }

    /// Only a rejected stream start may be retried. Once SSE begins, normal
    /// cancellation/reset/commit handling remains authoritative.
    pub(super) async fn start_stream(
        &self,
        request: &mut ChatRequest,
        context: &mut RequestContext,
        cancellation: &AgentCancellation,
    ) -> Result<ChatEventStream> {
        let mut context_retries = 0;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let started = Instant::now();
            tracing::debug!(target: "ngy_ai", round = context.round, attempt, context_retries,
                tool_arguments_retried = context.tool_arguments_retried,
                message_count = request.messages.len(), tools = request.tools.len(),
                history_messages = context.history_messages, max_tokens = ?request.max_tokens,
                "AI stream start attempt");
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => bail!(AgentRequestCancelled),
                result = self.provider.chat_stream(request.clone()) => result,
            };
            match result {
                Ok(stream) => {
                    tracing::debug!(target: "ngy_ai", round = context.round, attempt, elapsed_ms = started.elapsed().as_millis() as u64,
                        "AI stream start accepted");
                    return Ok(stream);
                }
                Err(error) => {
                    tracing::warn!(target: "ngy_ai", round = context.round, attempt, context_retries,
                        elapsed_ms = started.elapsed().as_millis() as u64, error_kind = error_kind(&error),
                        "AI stream start rejected");
                    if let Some(incomplete) = error.downcast_ref::<IncompleteToolArguments>() {
                        if context.tool_arguments_retried
                            || !request
                                .tools
                                .iter()
                                .any(|tool| tool.function.name == incomplete.tool_name)
                        {
                            return Err(error);
                        }
                        let Some(ChatMessage {
                            role: ChatRole::System,
                            content: Some(MessageContent::Text(policy)),
                            ..
                        }) = request.messages.first_mut()
                        else {
                            return Err(error);
                        };
                        // Use only host-authored guidance. Never inject the
                        // endpoint error or guessed arguments into the prompt.
                        policy.push_str(TOOL_ARGUMENT_RECOVERY_GUIDANCE);
                        request.temperature = Some(0.0);
                        context.tool_arguments_retried = true;
                        tracing::info!(target: "ngy_ai",
                            round = context.round, attempt, tool = tool_label(&incomplete.tool_name),
                            "retrying AI stream start once after incomplete tool arguments"
                        );
                        continue;
                    }
                    let Some(exceeded) = error.downcast_ref::<ContextWindowExceeded>() else {
                        return Err(error).context("failed to start AI response stream");
                    };
                    if context_retries == MAX_CONTEXT_RETRIES
                        || !reduce_request_history(request, context, exceeded, context_retries)
                    {
                        return Err(error);
                    }
                    context_retries += 1;
                    tracing::info!(target: "ngy_ai",
                        round = context.round, attempt,
                        retry = context_retries,
                        history_messages = context.history_messages,
                        max_output_tokens = context.max_output_tokens,
                        "retrying AI request after endpoint context rejection"
                    );
                }
            }
        }
    }
}
