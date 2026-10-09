//! Submitting a question, cancelling it and closing the session.

use super::*;

impl AiSidebarController {
    pub(crate) fn submit<Owner: 'static>(
        &mut self,
        request: AiQuestionRequest,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        let AiQuestionRequest {
            request_id,
            question,
            book_ids,
            book_titles,
            reference_hints,
            reference,
            truncate_from,
        } = request;
        let reference_hints = merge_editor_reference(reference_hints, reference);
        let services = Arc::clone(&self.services);
        let conversation = self.conversation.clone();
        let prepared = match conversation.prepare_request(request_id) {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(target: "ngy_ai", request_id, window_kind = ?self.window_kind, stage = "prepare_request", error_kind = error_kind(&error), "AI request preparation failed");
                let error = friendly_agent_error(&format!("{error:#}"));
                sidebar.update(cx, |sidebar, cx| {
                    sidebar.fail_answer(request_id, error, cx);
                });
                return;
            }
        };
        let cancellation = prepared.cancellation();
        let trace_id = cancellation.trace_id();
        let started = Instant::now();
        let ui_span = tracing::info_span!(
            target: "ngy_ai",
            "ai_ui_request",
            trace_id,
            request_id,
            window_kind = ?self.window_kind,
            allowed_books = book_ids.len(),
            references = reference_hints.len(),
            question_bytes = question.len()
        );
        let worker_span = ui_span.clone();
        let runtime = services.runtime();
        let live_scope = book_ids.clone();
        let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ui_tx, ui_rx) = async_channel::unbounded();

        let worker = async move {
            tracing::debug!(target: "ngy_ai", stage = "freeze_references", "AI reference preparation started");
            let freeze_started = Instant::now();
            let snapshots = match freeze_references(
                Arc::clone(&services),
                reference_hints,
                &book_ids,
                cancellation,
            )
            .await
            {
                Ok(snapshots) => {
                    tracing::debug!(target: "ngy_ai", stage = "freeze_references", snapshots = snapshots.len(), elapsed_ms = freeze_started.elapsed().as_millis() as u64, "AI reference preparation completed");
                    snapshots
                }
                Err(error) => {
                    if error.is::<AgentRequestCancelled>() {
                        tracing::info!(target: "ngy_ai", stage = "freeze_references", error_kind = "cancelled", elapsed_ms = freeze_started.elapsed().as_millis() as u64, "AI reference preparation cancelled");
                    } else {
                        tracing::warn!(target: "ngy_ai", stage = "freeze_references", error_kind = error_kind(&error), elapsed_ms = freeze_started.elapsed().as_millis() as u64, "AI reference preparation failed");
                    }
                    drop(prepared);
                    let _ = ui_tx
                        .send(UiAgentMessage::Completed(Box::new(Err(format!(
                            "{error:#}"
                        )))))
                        .await;
                    return;
                }
            };
            let cancel_conversation = conversation.clone();
            let session_conversation = conversation.clone();
            let mut answer = Box::pin(conversation.ask_prepared(
                ConversationQuestion {
                    request_id,
                    question,
                    allowed_book_ids: book_ids,
                    book_titles,
                    snapshots,
                    truncate_from,
                },
                Some(agent_tx),
                prepared,
            ));
            let mut agent_events_open = true;
            let result = loop {
                tokio::select! {
                    biased;
                    event = agent_rx.recv(), if agent_events_open => {
                        let message = match event {
                            Some(AgentRunEvent::AnswerDelta(delta)) => Some(UiAgentMessage::Delta(delta)),
                            Some(AgentRunEvent::AnswerReset) => Some(UiAgentMessage::Reset),
                            Some(AgentRunEvent::AnswerCommitted) => Some(UiAgentMessage::Committed),
                            Some(AgentRunEvent::ToolStarted { .. } | AgentRunEvent::ToolFinished { .. }) => None,
                            None => {
                                agent_events_open = false;
                                None
                            }
                        };
                        if let Some(message) = message
                            && ui_tx.send(message).await.is_err()
                        {
                                tracing::debug!(target: "ngy_ai", stage = "deliver_events", "AI UI receiver closed; cancelling request");
                                cancel_conversation.cancel(request_id);
                                return;
                        }
                    }
                    result = &mut answer => {
                        break result;
                    }
                }
            };
            // The completed future still owns PreparedAgentRequest. Drop it
            // before notifying the UI so a pending scope reconciliation never
            // observes a request that has already finished or been cancelled.
            drop(answer);
            let result = match result {
                Ok(answer) => {
                    // Preserve AgentRuntime's explicit verified-source result before any UI
                    // source conversion or current-document validation can discard a link.
                    let source_status = answer.answer.source_status;
                    let sources = answer
                        .answer
                        .citations
                        .iter()
                        .cloned()
                        .filter_map(source_link_from_agent_citation)
                        .collect();
                    let sources = match validate_live_sources(
                        Arc::clone(&services),
                        live_scope.clone(),
                        sources,
                    )
                    .await
                    {
                        Ok(sources) => sources,
                        Err(error) => {
                            tracing::warn!(
                                target: "ngy_ai",
                                stage = "validate_live_sources",
                                error_kind = error_kind(&error),
                                "cannot decorate the already-persisted AI answer with live sources"
                            );
                            Vec::new()
                        }
                    };
                    Ok(UiConversationAnswer {
                        answer,
                        sources,
                        source_status,
                    })
                }
                Err(error) => {
                    if error.is::<AgentRequestCancelled>() {
                        tracing::debug!(target: "ngy_ai", stage = "conversation_result", error_kind = "cancelled", elapsed_ms = started.elapsed().as_millis() as u64, "AI conversation cancellation returned to the UI");
                    } else {
                        tracing::warn!(target: "ngy_ai", stage = "conversation_result", error_kind = error_kind(&error), elapsed_ms = started.elapsed().as_millis() as u64, "AI conversation returned an error to the UI");
                    }
                    Err(format!("{error:#}"))
                }
            };
            let active_thread_id = session_conversation.selected_thread_id().await;
            match session_conversation.list_sessions(&live_scope).await {
                Ok(threads) => {
                    let _ = ui_tx
                        .send(UiAgentMessage::Sessions {
                            active_thread_id,
                            threads: thread_options(threads),
                        })
                        .await;
                }
                Err(error) => {
                    tracing::warn!(target: "ngy_ai", stage = "refresh_sessions", error_kind = error_kind(&error), "cannot refresh AI conversation list");
                }
            }
            let succeeded = result.is_ok();
            let delivered = ui_tx
                .send(UiAgentMessage::Completed(Box::new(result)))
                .await
                .is_ok();
            tracing::debug!(target: "ngy_ai", stage = "deliver_completion", succeeded, delivered, elapsed_ms = started.elapsed().as_millis() as u64, "AI conversation completion sent to UI");
        };
        runtime.spawn(worker.instrument(worker_span));

        self.answer_task = Some(cx.spawn(async move |_, cx| {
            let update = async move {
                let mut received_delta = false;
                let mut answer_committed = false;
                let mut delta_bytes = 0usize;
                while let Ok(message) = ui_rx.recv().await {
                    match message {
                        UiAgentMessage::Delta(delta) => {
                            delta_bytes = delta_bytes.saturating_add(delta.len());
                            received_delta = true;
                            let _ = sidebar.update(cx, |sidebar, cx| {
                                sidebar.append_answer_delta(request_id, &delta, cx);
                            });
                        }
                        UiAgentMessage::Reset => {
                            tracing::debug!(target: "ngy_ai", stage = "reset_provisional_answer", "AI provisional answer reset");
                            received_delta = false;
                            answer_committed = false;
                            let _ = sidebar.update(cx, |sidebar, cx| {
                                sidebar.reset_answer(request_id, cx);
                            });
                        }
                        UiAgentMessage::Committed => {
                            answer_committed = true;
                            tracing::debug!(target: "ngy_ai", stage = "answer_source_commit", "AI source validation commit received");
                        }
                        UiAgentMessage::Sessions {
                            active_thread_id,
                            threads,
                        } => {
                            let _ = sidebar.update(cx, |sidebar, cx| {
                                sidebar.refresh_sessions(active_thread_id, threads, cx);
                            });
                        }
                        UiAgentMessage::Completed(result) => {
                            let succeeded = result.is_ok();
                            let applied = sidebar.update(cx, |sidebar, cx| match *result {
                                Ok(answer) => {
                                    if !answer_committed {
                                        sidebar.reset_answer(request_id, cx);
                                        received_delta = false;
                                    }
                                    if !received_delta {
                                        sidebar.append_answer_delta(
                                            request_id,
                                            &answer.answer.answer.markdown,
                                            cx,
                                        );
                                    }
                                    sidebar.finish_persisted_answer(
                                        request_id,
                                        answer.answer.stored_message.content,
                                        answer.sources,
                                        answer.source_status,
                                        Some(answer.answer.user_message_id),
                                        Some(answer.answer.stored_message.id),
                                        cx,
                                    )
                                }
                                Err(error) => {
                                    sidebar.fail_answer(request_id, friendly_agent_error(&error), cx)
                                }
                            });
                            if applied {
                                tracing::debug!(target: "ngy_ai", stage = "ui_completion", succeeded, delta_bytes, elapsed_ms = started.elapsed().as_millis() as u64, "AI result applied to sidebar");
                            } else {
                                tracing::debug!(target: "ngy_ai", stage = "ui_completion", succeeded, "AI stale completion ignored by sidebar");
                            }
                            return;
                        }
                    }
                }
                tracing::debug!(target: "ngy_ai", stage = "ui_events_closed", delta_bytes, elapsed_ms = started.elapsed().as_millis() as u64, "AI UI event channel closed");
            };
            update.instrument(ui_span).await
        }));
    }

    pub(crate) fn cancel(&mut self, request_id: u64) {
        self.conversation.cancel(request_id);
    }

    pub(crate) fn close(&mut self) {
        self.conversation.cancel_all();
        self.session_generation.fetch_add(1, Ordering::SeqCst);
        self.answer_task.take();
        self.session_task.take();
    }
}
