//! The answer loop: context retries, tool rounds and answer projection.

pub(super) use super::*;

impl AgentRuntime {
    pub(super) async fn answer_inner(
        &self,
        question: AgentQuestion,
        events: Option<mpsc::UnboundedSender<AgentRunEvent>>,
        cancellation: &AgentCancellation,
        stage: &mut &'static str,
    ) -> Result<AgentAnswer> {
        mark_stage(stage, "scope_and_snapshots");
        if cancellation.is_cancelled() {
            bail!(AgentRequestCancelled);
        }
        validate_history(&question.history)?;
        let scope = AllowedBookScope::new(question.allowed_book_ids)
            .map_err(anyhow::Error::new)
            .context("invalid host-authorized book scope")?;
        let mut agent = ReadOnlyAgent::new(
            Arc::clone(&self.search),
            Arc::clone(&self.books),
            scope,
            self.limits.clone(),
        )
        .map_err(anyhow::Error::new)?;
        let mut citations = CitationRegistry::default();
        for snapshot in question.snapshots {
            agent
                .attach_snapshot(snapshot)
                .map_err(anyhow::Error::new)?;
        }
        for (index, snapshot) in agent.snapshots().iter().enumerate() {
            // Frozen editor text is a source only after the host resolved its
            // book/unit/locator and revisions against the shared library. The
            // model sees only a short turn-local alias; the registry resolves
            // it back to this stable host-issued citation after validation.
            let citation = snapshot.citation().map_err(anyhow::Error::new)?;
            citations
                .record_citation_for_marker(agent.selection_model_marker(index), citation)
                .map_err(anyhow::Error::new)?;
        }

        let question_messages = agent
            .question_messages(&question.question, &question.book_titles)
            .map_err(anyhow::Error::new)?;
        let mut messages = Vec::with_capacity(question.history.len() + 2);
        let mut context = RequestContext {
            round: 0,
            history_messages: question.history.len(),
            max_output_tokens: self.chat_generation.max_output_tokens,
            tool_arguments_retried: false,
        };
        messages.push(question_messages[0].clone());
        messages.extend(question.history);
        messages.push(question_messages[1].clone());

        let has_authorized_books = agent.scope().ids().len() > 0;
        // One tool-free final response is allowed after all tool calls have
        // been spent. An empty host scope is tool-free from the first turn.
        for round in 0..=self.limits.max_tool_rounds {
            context.round = round + 1;
            mark_stage(stage, "stream_start");
            let round_started = Instant::now();
            let remaining_tool_calls = agent.remaining_tool_calls();
            let tools_offered = has_authorized_books && remaining_tool_calls > 0;
            if has_authorized_books && remaining_tool_calls == 0 {
                if let Some(MessageContent::Text(policy)) = messages[0].content.as_mut() {
                    policy.push_str(TOOL_BUDGET_FINAL_GUIDANCE);
                }
                tracing::info!(target: "ngy_ai", round = context.round,
                    tool_calls_used = agent.rounds_used(), remaining_tool_calls,
                    "AI tool budget exhausted; requesting final answer");
            }
            let mut request = ChatRequest {
                model: self.chat_model.clone(),
                messages: messages.clone(),
                tools: if tools_offered {
                    agent_tool_definitions()
                } else {
                    Vec::new()
                },
                temperature: self.chat_generation.temperature,
                top_p: self.chat_generation.top_p,
                presence_penalty: self.chat_generation.presence_penalty,
                frequency_penalty: self.chat_generation.frequency_penalty,
                max_tokens: Some(context.max_output_tokens),
                // Agent answers need the final `content`, not a hidden reasoning
                // trace. Small thinking models can otherwise spend the entire
                // output budget before emitting any answer text.
                reasoning_effort: Some(ReasoningEffort::None),
            };
            let mut stream = self
                .start_stream(&mut request, &mut context, cancellation)
                .await?;
            mark_stage(stage, "request_citations");
            // Carry accepted history reduction into later tool rounds. The
            // persisted conversation itself is never edited or deleted.
            messages = request.messages;
            retain_request_citations(&mut citations, &messages)?;

            let mut accumulator = AgentStreamAccumulator::new(
                MAX_ANSWER_BYTES,
                MAX_TOOL_CALLS_PER_TURN,
                MAX_TOOL_ARGUMENT_BYTES,
            )
            .map_err(anyhow::Error::new)?;
            let mut projection = StreamingAnswerProjection::default();
            let mut publisher = AnswerEventPublisher::new(events.as_ref());
            let mut tool_turn = false;
            let mut stream_events = 0usize;
            mark_stage(stage, "stream_receive");
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    _ = cancellation.cancelled() => bail!(AgentRequestCancelled),
                };
                let Some(item) = item else { break };
                let event = item.context("AI response stream failed")?;
                stream_events += 1;
                if !tool_turn && !event.tool_call_deltas.is_empty() {
                    tool_turn = true;
                    publisher.reset();
                }
                mark_stage(stage, "stream_accumulate");
                accumulator.push(&event).map_err(anyhow::Error::new)?;
                mark_stage(stage, "stream_receive");
                if !tool_turn && let Some(delta) = event.content_delta.as_deref() {
                    let visible = projection.push(delta);
                    publisher.delta(visible);
                }
                if event.done {
                    break;
                }
            }
            mark_stage(stage, "stream_finish");
            let turn = accumulator.finish().map_err(anyhow::Error::new)?;
            tracing::debug!(target: "ngy_ai", round = context.round, stream_events, elapsed_ms = round_started.elapsed().as_millis() as u64,
                finish_reason = ?turn.finish_reason.as_deref().map(finish_reason_label),
                completed = turn.completed, answer_bytes = turn.content.len(), tool_calls = turn.tool_calls.len(),
                usage = ?turn.usage, "AI tool round received");
            ensure!(
                turn.completed || turn.finish_reason.is_some(),
                "AI response stream ended before a completion marker"
            );

            if turn.tool_calls.is_empty() {
                mark_stage(stage, "answer_validation");
                ensure!(
                    !turn.content.trim().is_empty(),
                    "AI endpoint returned an empty answer"
                );
                let (markdown, citation_ids, no_source) = extract_citation_markers(&turn.content)?;
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
                // The books could not ground this answer. Before letting the
                // model answer unaided, let the host try the internet. The
                // provisional text above is withdrawn if the fallback grounds
                // a new answer.
                if answer.source_status == AgentAnswerSourceStatus::NoVerifiedSources
                    && let Some(web) = self.web.clone()
                {
                    mark_stage(stage, "web_fallback");
                    if let Some(web_answer) = self
                        .web_fallback(
                            web.as_ref(),
                            &question.question,
                            &messages,
                            &mut context,
                            &mut citations,
                            &mut publisher,
                            cancellation,
                        )
                        .await?
                    {
                        return Ok(web_answer);
                    }
                }
                mark_stage(stage, "answer_projection");
                // The projection withheld possible cross-chunk protocol
                // markers. Publish only the validated remainder, then commit
                // all preceding provisional deltas atomically.
                let remainder = projection.finish(&answer.markdown)?;
                publisher.delta(remainder);
                publisher.commit();
                return Ok(answer);
            }

            ensure!(
                tools_offered,
                "AI endpoint returned tool calls when no tools were offered"
            );

            let assistant = ChatMessage {
                role: ChatRole::Assistant,
                content: (!turn.content.is_empty())
                    .then(|| MessageContent::Text(turn.content.clone())),
                name: None,
                tool_call_id: None,
                tool_calls: turn.tool_calls.clone(),
            };
            messages.push(assistant);
            for (index, call) in turn.tool_calls.iter().enumerate() {
                if cancellation.is_cancelled() {
                    bail!(AgentRequestCancelled);
                }
                if agent.remaining_tool_calls() == 0 {
                    messages.push(ChatMessage {
                        role: ChatRole::Tool,
                        content: Some(TOOL_BUDGET_EXHAUSTED_RESULT.into()),
                        name: Some(call.function.name.clone()),
                        tool_call_id: Some(call.id.clone()),
                        tool_calls: Vec::new(),
                    });
                    tracing::debug!(target: "ngy_ai", round = context.round,
                        tool_index = index, tool = tool_label(&call.function.name),
                        tool_calls_used = agent.rounds_used(),
                        "AI tool skipped because the call budget is exhausted");
                    continue;
                }
                mark_stage(stage, "tool_execution");
                let tool_started = Instant::now();
                tracing::debug!(target: "ngy_ai", round = context.round, tool_index = index, tool = tool_label(&call.function.name),
                    tool_calls_used = agent.rounds_used(), remaining_tool_calls = agent.remaining_tool_calls(),
                    arguments = ?ToolArgumentsSummary::new(&call.function.arguments), "AI tool started");
                if let Some(sender) = events.as_ref() {
                    let _ = sender.send(AgentRunEvent::ToolStarted {
                        name: call.function.name.clone(),
                    });
                }
                let execution = tokio::select! {
                    result = agent.execute_tool(call) => result.map_err(anyhow::Error::new),
                    _ = cancellation.cancelled() => bail!(AgentRequestCancelled),
                };
                let execution = execution.inspect_err(|error| {
                    tracing::warn!(target: "ngy_ai", round = context.round, tool_index = index, tool = tool_label(&call.function.name),
                        elapsed_ms = tool_started.elapsed().as_millis() as u64, error_kind = error_kind(error),
                        arguments = ?ToolArgumentsSummary::new(&call.function.arguments), "AI tool failed");
                })?;
                mark_stage(stage, "tool_citations");
                citations.record(&execution).map_err(anyhow::Error::new)?;
                tracing::debug!(target: "ngy_ai", round = context.round, tool_index = index, tool = tool_label(&execution.name),
                    elapsed_ms = tool_started.elapsed().as_millis() as u64, result_bytes = execution.content.len(),
                    citations = execution.citations.len(), "AI tool completed");
                if let Some(sender) = events.as_ref() {
                    let _ = sender.send(AgentRunEvent::ToolFinished {
                        name: execution.name.clone(),
                        citation_count: execution.citations.len(),
                    });
                }
                messages.push(execution.as_chat_message());
            }
        }
        bail!("AI agent exceeded its tool-round limit")
    }
}
