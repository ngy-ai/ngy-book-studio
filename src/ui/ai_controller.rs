//! GPUI adapter for persistent read-only AI conversations.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context as _, Result, bail};
use gpui::{Context, Entity, Task};
use moye_epub_editor::{
    agent::{AgentAnswerSourceStatus, AgentCitation, SelectionSnapshot},
    agent_chat::{AgentConversation, ConversationQuestion},
    agent_runtime::{AgentCancellation, AgentRequestCancelled, AgentRunEvent},
    ai::ChatRole,
    ai_diagnostics::error_kind,
    chat::{ChatCitation, ChatSession, ChatThread, ChatWindowKind},
    document::{BookDocument, Revision, SourceLocator},
    services::AppServices,
};
use tracing::Instrument as _;

use super::ai_sidebar::{
    AiQuestionRequest, AiReferenceHint, AiRestoredMessage, AiRestoredRole, AiSidebar, AiSourceLink,
    AiThreadOption, MAX_SELECTED_REFERENCES,
};

const MAX_REFERENCE_TOTAL_BYTES: usize = 96 * 1024;

enum UiAgentMessage {
    Delta(String),
    Reset,
    Committed,
    Sessions {
        active_thread_id: Option<String>,
        threads: Vec<AiThreadOption>,
    },
    Completed(Box<std::result::Result<UiConversationAnswer, String>>),
}

struct UiConversationAnswer {
    answer: moye_epub_editor::agent_chat::ConversationAnswer,
    sources: Vec<AiSourceLink>,
    source_status: AgentAnswerSourceStatus,
}

struct UiSessionState {
    active_thread_id: Option<String>,
    threads: Vec<AiThreadOption>,
    messages: Vec<AiRestoredMessage>,
}

pub(super) struct AiSidebarController {
    services: Arc<AppServices>,
    conversation: AgentConversation,
    window_kind: ChatWindowKind,
    answer_task: Option<Task<()>>,
    session_task: Option<Task<()>>,
    session_generation: Arc<AtomicU64>,
}

impl AiSidebarController {
    pub(super) fn new(
        services: Arc<AppServices>,
        window_kind: ChatWindowKind,
        primary_book_id: Option<String>,
    ) -> Result<Self> {
        let conversation =
            AgentConversation::new(Arc::clone(&services), window_kind, primary_book_id)?;
        Ok(Self {
            services,
            conversation,
            window_kind,
            answer_task: None,
            session_task: None,
            session_generation: Arc::new(AtomicU64::new(0)),
        })
    }

    pub(super) fn restore<Owner: 'static>(
        &mut self,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        if !sidebar.update(cx, |sidebar, cx| sidebar.begin_session_operation(cx)) {
            return;
        }
        let generation = self.session_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current_generation = Arc::clone(&self.session_generation);
        let conversation = self.conversation.clone();
        let services = Arc::clone(&self.services);
        let allowed_book_ids = sidebar.read(cx).authorized_book_ids();
        let join = self.services.runtime().spawn(async move {
            let session = conversation
                .restore(&allowed_book_ids)
                .await
                .map_err(|error| format!("{error:#}"))?;
            Ok::<_, String>(
                load_ui_session_state(services, &conversation, session, &allowed_book_ids).await,
            )
        });
        self.session_task = Some(cx.spawn(async move |_, cx| {
            let outcome = match join.await {
                Ok(outcome) => outcome,
                Err(error) => Err(format!("AI 会话恢复任务已停止：{error}")),
            };
            if current_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = sidebar.update(cx, |sidebar, cx| match outcome {
                Ok(session) => {
                    sidebar.apply_session(
                        session.active_thread_id,
                        session.threads,
                        session.messages,
                        cx,
                    );
                }
                Err(error) => {
                    sidebar.fail_session_operation(format!("无法恢复 AI 会话：{error}"), cx)
                }
            });
        }));
    }

    pub(super) fn new_session<Owner: 'static>(
        &mut self,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        self.create_session(sidebar, None, cx);
    }

    /// Wait for the backend to leave the old conversation before publishing
    /// the fixed question. The owned hint survives later Reader selection
    /// changes, while the ordinary Submit handler still owns cancellation.
    pub(super) fn explain_selection<Owner: 'static>(
        &mut self,
        reference: AiReferenceHint,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) -> bool {
        let allowed_book_ids = sidebar.read(cx).authorized_book_ids();
        let validation = validate_explanation_reference(&reference, &allowed_book_ids);
        if let Err(error) = validation {
            sidebar.update(cx, |sidebar, cx| {
                sidebar.show_explanation_error(format!("无法解释所选文本：{error:#}"), cx);
            });
            return false;
        }
        if !sidebar.update(cx, |sidebar, cx| sidebar.begin_selection_explanation(cx)) {
            return false;
        }
        self.create_session(sidebar, Some(reference), cx);
        true
    }

    fn create_session<Owner: 'static>(
        &mut self,
        sidebar: Entity<AiSidebar>,
        explanation: Option<AiReferenceHint>,
        cx: &mut Context<Owner>,
    ) {
        let generation = self.session_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current_generation = Arc::clone(&self.session_generation);
        let conversation = self.conversation.clone();
        let services = Arc::clone(&self.services);
        let allowed_book_ids = sidebar.read(cx).authorized_book_ids();
        let join = self.services.runtime().spawn(start_new_ui_session(
            services,
            conversation,
            allowed_book_ids,
        ));
        self.session_task = Some(cx.spawn(async move |_, cx| {
            let outcome = match join.await {
                Ok(outcome) => outcome,
                Err(error) => Err(format!("新建 AI 会话任务已停止：{error}")),
            };
            if current_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = sidebar.update(cx, |sidebar, cx| match outcome {
                Ok(session) => {
                    if let Some(reference) = explanation {
                        sidebar.complete_selection_explanation(session.threads, reference, cx);
                    } else {
                        sidebar.apply_session(None, session.threads, session.messages, cx);
                    }
                }
                Err(error) => {
                    sidebar.fail_session_operation(format!("无法新建 AI 会话：{error}"), cx)
                }
            });
        }));
    }

    pub(super) fn switch_session<Owner: 'static>(
        &mut self,
        thread_id: String,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        let generation = self.session_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current_generation = Arc::clone(&self.session_generation);
        let conversation = self.conversation.clone();
        let services = Arc::clone(&self.services);
        let allowed_book_ids = sidebar.read(cx).authorized_book_ids();
        let join = self.services.runtime().spawn(async move {
            let session = conversation
                .select_session(&thread_id, &allowed_book_ids)
                .await
                .map_err(|error| format!("{error:#}"))?;
            Ok::<_, String>(
                load_ui_session_state(services, &conversation, Some(session), &allowed_book_ids)
                    .await,
            )
        });
        self.session_task = Some(cx.spawn(async move |_, cx| {
            let outcome = match join.await {
                Ok(outcome) => outcome,
                Err(error) => Err(format!("切换 AI 会话任务已停止：{error}")),
            };
            if current_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = sidebar.update(cx, |sidebar, cx| match outcome {
                Ok(session) => {
                    sidebar.apply_session(
                        session.active_thread_id,
                        session.threads,
                        session.messages,
                        cx,
                    );
                }
                Err(error) => {
                    sidebar.fail_session_operation(format!("无法切换 AI 会话：{error}"), cx)
                }
            });
        }));
    }

    pub(super) fn delete_session<Owner: 'static>(
        &mut self,
        thread_id: String,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        let generation = self.session_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current_generation = Arc::clone(&self.session_generation);
        let conversation = self.conversation.clone();
        let services = Arc::clone(&self.services);
        let allowed_book_ids = sidebar.read(cx).authorized_book_ids();
        let join = self.services.runtime().spawn(async move {
            let session = conversation
                .delete_session(&thread_id, &allowed_book_ids)
                .await
                .map_err(|error| format!("{error:#}"))?;
            Ok::<_, String>(
                load_ui_session_state(services, &conversation, session, &allowed_book_ids).await,
            )
        });
        self.session_task = Some(cx.spawn(async move |_, cx| {
            let outcome = match join.await {
                Ok(outcome) => outcome,
                Err(error) => Err(format!("删除 AI 会话任务已停止：{error}")),
            };
            if current_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = sidebar.update(cx, |sidebar, cx| match outcome {
                Ok(session) => {
                    sidebar.apply_session(
                        session.active_thread_id,
                        session.threads,
                        session.messages,
                        cx,
                    );
                }
                Err(error) => {
                    sidebar.fail_session_operation(format!("无法删除 AI 会话：{error}"), cx)
                }
            });
        }));
    }

    pub(super) fn reconcile_scope<Owner: 'static>(
        &mut self,
        allowed_book_ids: Vec<String>,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        let generation = self.session_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current_generation = Arc::clone(&self.session_generation);
        let conversation = self.conversation.clone();
        let join = self.services.runtime().spawn(async move {
            let reset = conversation
                .reconcile_scope(&allowed_book_ids)
                .await
                .map_err(|error| format!("{error:#}"))?;
            let active_thread_id = conversation.selected_thread_id().await;
            let threads = conversation
                .list_sessions(&allowed_book_ids)
                .await
                .map(thread_options)
                .map_err(|error| format!("{error:#}"));
            Ok::<_, String>((reset, active_thread_id, threads))
        });
        self.session_task = Some(cx.spawn(async move |_, cx| {
            let outcome = match join.await {
                Ok(outcome) => outcome,
                Err(error) => Err(format!("AI 会话范围同步任务已停止：{error}")),
            };
            if current_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = sidebar.update(cx, |sidebar, cx| match outcome {
                Ok((reset, active_thread_id, Ok(threads))) => {
                    if reset {
                        sidebar.apply_session(None, threads, Vec::new(), cx);
                    } else {
                        sidebar.refresh_sessions(active_thread_id, threads, cx);
                    }
                }
                Ok((true, _, Err(error))) => {
                    tracing::warn!(target: "moye_ai", stage = "list_sessions_after_scope_reset", error_kind = error_kind(&anyhow::Error::msg(error)), "cannot list sessions after resetting AI scope");
                    sidebar.apply_session(None, Vec::new(), Vec::new(), cx);
                }
                Ok((false, _, Err(error))) => sidebar
                    .fail_session_operation(format!("无法刷新当前范围的 AI 会话：{error}"), cx),
                Err(error) => {
                    sidebar.fail_session_operation(format!("无法同步 AI 会话范围：{error}"), cx)
                }
            });
        }));
    }

    pub(super) fn submit<Owner: 'static>(
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
        } = request;
        let reference_hints = merge_editor_reference(reference_hints, reference);
        let services = Arc::clone(&self.services);
        let conversation = self.conversation.clone();
        let prepared = match conversation.prepare_request(request_id) {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(target: "moye_ai", request_id, window_kind = ?self.window_kind, stage = "prepare_request", error_kind = error_kind(&error), "AI request preparation failed");
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
            target: "moye_ai",
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
            tracing::debug!(target: "moye_ai", stage = "freeze_references", "AI reference preparation started");
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
                    tracing::debug!(target: "moye_ai", stage = "freeze_references", snapshots = snapshots.len(), elapsed_ms = freeze_started.elapsed().as_millis() as u64, "AI reference preparation completed");
                    snapshots
                }
                Err(error) => {
                    if error.is::<AgentRequestCancelled>() {
                        tracing::info!(target: "moye_ai", stage = "freeze_references", error_kind = "cancelled", elapsed_ms = freeze_started.elapsed().as_millis() as u64, "AI reference preparation cancelled");
                    } else {
                        tracing::warn!(target: "moye_ai", stage = "freeze_references", error_kind = error_kind(&error), elapsed_ms = freeze_started.elapsed().as_millis() as u64, "AI reference preparation failed");
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
                                tracing::debug!(target: "moye_ai", stage = "deliver_events", "AI UI receiver closed; cancelling request");
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
                                target: "moye_ai",
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
                        tracing::debug!(target: "moye_ai", stage = "conversation_result", error_kind = "cancelled", elapsed_ms = started.elapsed().as_millis() as u64, "AI conversation cancellation returned to the UI");
                    } else {
                        tracing::warn!(target: "moye_ai", stage = "conversation_result", error_kind = error_kind(&error), elapsed_ms = started.elapsed().as_millis() as u64, "AI conversation returned an error to the UI");
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
                    tracing::warn!(target: "moye_ai", stage = "refresh_sessions", error_kind = error_kind(&error), "cannot refresh AI conversation list");
                }
            }
            let succeeded = result.is_ok();
            let delivered = ui_tx
                .send(UiAgentMessage::Completed(Box::new(result)))
                .await
                .is_ok();
            tracing::debug!(target: "moye_ai", stage = "deliver_completion", succeeded, delivered, elapsed_ms = started.elapsed().as_millis() as u64, "AI conversation completion sent to UI");
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
                            tracing::debug!(target: "moye_ai", stage = "reset_provisional_answer", "AI provisional answer reset");
                            received_delta = false;
                            answer_committed = false;
                            let _ = sidebar.update(cx, |sidebar, cx| {
                                sidebar.reset_answer(request_id, cx);
                            });
                        }
                        UiAgentMessage::Committed => {
                            answer_committed = true;
                            tracing::debug!(target: "moye_ai", stage = "answer_source_commit", "AI source validation commit received");
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
                                    sidebar.finish_answer(
                                        request_id,
                                        answer.sources,
                                        answer.source_status,
                                        cx,
                                    )
                                }
                                Err(error) => {
                                    sidebar.fail_answer(request_id, friendly_agent_error(&error), cx)
                                }
                            });
                            match applied {
                                Ok(true) => tracing::debug!(target: "moye_ai", stage = "ui_completion", succeeded, delta_bytes, elapsed_ms = started.elapsed().as_millis() as u64, "AI result applied to sidebar"),
                                Ok(false) => tracing::debug!(target: "moye_ai", stage = "ui_completion", succeeded, "AI stale completion ignored by sidebar"),
                                Err(_) => tracing::debug!(target: "moye_ai", stage = "ui_completion", succeeded, "AI completion dropped because sidebar closed"),
                            }
                            return;
                        }
                    }
                }
                tracing::debug!(target: "moye_ai", stage = "ui_events_closed", delta_bytes, elapsed_ms = started.elapsed().as_millis() as u64, "AI UI event channel closed");
            };
            update.instrument(ui_span).await
        }));
    }

    pub(super) fn cancel(&mut self, request_id: u64) {
        self.conversation.cancel(request_id);
    }

    pub(super) fn close(&mut self) {
        self.conversation.cancel_all();
        self.session_generation.fetch_add(1, Ordering::SeqCst);
        self.answer_task.take();
        self.session_task.take();
    }
}

async fn start_new_ui_session(
    services: Arc<AppServices>,
    conversation: AgentConversation,
    allowed_book_ids: Vec<String>,
) -> std::result::Result<UiSessionState, String> {
    conversation
        .start_new_session()
        .await
        .map_err(|error| format!("{error:#}"))?;
    Ok(load_ui_session_state(services, &conversation, None, &allowed_book_ids).await)
}

fn merge_editor_reference(
    mut references: Vec<AiReferenceHint>,
    editor_reference: Option<AiReferenceHint>,
) -> Vec<AiReferenceHint> {
    let Some(editor_reference) = editor_reference else {
        return references;
    };
    if let Some(reference) = references.iter_mut().find(|reference| {
        reference.book_id == editor_reference.book_id
            && reference.unit_id == editor_reference.unit_id
    }) {
        *reference = editor_reference;
    }
    references
}

fn validate_reference_request(
    references: &[AiReferenceHint],
    allowed_book_ids: &[String],
) -> Result<()> {
    if references.len() > MAX_SELECTED_REFERENCES {
        bail!("一次最多引用 {MAX_SELECTED_REFERENCES} 个章节或页面");
    }
    let allowed = allowed_book_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut frozen_bytes = 0usize;
    for reference in references {
        if !allowed.contains(reference.book_id.as_str()) {
            bail!("引用的图书已不在当前窗口授权范围内");
        }
        validate_reference_locator(reference)?;
        let locator_json = reference
            .locator
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("无法序列化引用定位信息")?;
        if !seen.insert((
            reference.book_id.as_str(),
            reference.unit_id.as_str(),
            locator_json,
        )) {
            bail!("引用列表包含重复的章节或页面");
        }
        if let Some(text) = reference.frozen_text.as_ref() {
            frozen_bytes = checked_reference_bytes(frozen_bytes, text.len())?;
        }
    }
    Ok(())
}

fn validate_reference_locator(reference: &AiReferenceHint) -> Result<()> {
    let Some(locator) = reference.locator.as_ref() else {
        return Ok(());
    };
    locator.validate().context("引用定位信息无效")?;
    if locator.book_id != reference.book_id || locator.unit_id != reference.unit_id {
        bail!("引用定位与图书或内容单元不一致");
    }
    if matches!(
        locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    ) {
        bail!("Office 增强预览页是可重建派生数据，不能作为 AI 引用定位");
    }
    Ok(())
}

fn checked_reference_bytes(current: usize, additional: usize) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .context("引用内容大小溢出")?;
    if total > MAX_REFERENCE_TOTAL_BYTES {
        bail!(
            "所选引用内容总大小超过 {} KiB",
            MAX_REFERENCE_TOTAL_BYTES / 1024
        );
    }
    Ok(total)
}

fn validate_explanation_reference(
    reference: &AiReferenceHint,
    allowed_book_ids: &[String],
) -> Result<()> {
    validate_reference_request(std::slice::from_ref(reference), allowed_book_ids)?;
    // Reader hints deliberately carry no editor revision. The conversation
    // authorizes their stable locator against current persisted metadata;
    // explicit revisions remain available for Editor snapshot barriers.
    let snapshot = capture_frozen_reference(reference)?.context("请先选择需要解释的文本")?;
    validate_snapshot_payload(&[snapshot])
}

fn capture_frozen_reference(reference: &AiReferenceHint) -> Result<Option<SelectionSnapshot>> {
    let Some(text) = reference.frozen_text.as_ref() else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let snapshot = SelectionSnapshot::capture(
        reference.book_id.clone(),
        reference.unit_id.clone(),
        reference.revision.unwrap_or(0),
        text.clone(),
    )
    .map_err(anyhow::Error::new)?;
    let snapshot = match reference.locator.clone() {
        Some(locator) => snapshot.with_locator(locator).map_err(anyhow::Error::new)?,
        None => snapshot,
    };
    Ok(Some(snapshot))
}

fn validate_snapshot_payload(snapshots: &[SelectionSnapshot]) -> Result<()> {
    if snapshots.len() > MAX_SELECTED_REFERENCES {
        bail!("一次最多引用 {MAX_SELECTED_REFERENCES} 个章节或页面");
    }
    let mut text_bytes = 0usize;
    for snapshot in snapshots {
        text_bytes = checked_reference_bytes(text_bytes, snapshot.text.len())?;
    }
    let encoded_bytes = serde_json::to_vec(snapshots)
        .context("无法计算引用快照大小")?
        .len();
    if encoded_bytes > MAX_REFERENCE_TOTAL_BYTES {
        bail!(
            "所选引用快照总大小超过 {} KiB",
            MAX_REFERENCE_TOTAL_BYTES / 1024
        );
    }
    Ok(())
}

async fn freeze_references(
    services: Arc<AppServices>,
    references: Vec<AiReferenceHint>,
    allowed_book_ids: &[String],
    cancellation: AgentCancellation,
) -> Result<Vec<SelectionSnapshot>> {
    ensure_reference_freeze_active(&cancellation)?;
    validate_reference_request(&references, allowed_book_ids)?;
    if references.is_empty() {
        return Ok(Vec::new());
    }

    let mut slots = vec![None; references.len()];
    let mut persisted = Vec::new();
    let mut frozen_bytes = 0usize;
    for (index, reference) in references.into_iter().enumerate() {
        ensure_reference_freeze_active(&cancellation)?;
        if reference.frozen_text.is_some() {
            if let Some(snapshot) = capture_frozen_reference(&reference)? {
                frozen_bytes = checked_reference_bytes(frozen_bytes, snapshot.text.len())?;
                slots[index] = Some(snapshot);
            }
        } else {
            persisted.push((index, reference));
        }
    }

    if !persisted.is_empty() {
        ensure_reference_freeze_active(&cancellation)?;
        let read_cancellation = cancellation.clone();
        let resolved = services
            .spawn_library_read(move |library| {
                ensure_reference_freeze_active(&read_cancellation)?;
                let mut resolved = Vec::with_capacity(persisted.len());
                let mut total_bytes = frozen_bytes;
                for (index, reference) in persisted {
                    ensure_reference_freeze_active(&read_cancellation)?;
                    let document = library.document(&reference.book_id)?;
                    let unit = document
                        .units
                        .iter()
                        .find(|unit| unit.id == reference.unit_id)
                        .context("引用的章节或页面已不存在")?;
                    if let Some(unit_index) = reference.unit_index {
                        let indexed = document
                            .units
                            .get(unit_index)
                            .context("引用的内容单元序号已失效")?;
                        if indexed.id != unit.id {
                            bail!("引用的内容单元 ID 与序号不一致");
                        }
                    }
                    validate_persisted_reference(&document, unit, &reference)?;
                    let text = unit.plain_text();
                    total_bytes = checked_reference_bytes(total_bytes, text.len())?;
                    let snapshot = SelectionSnapshot::capture(
                        reference.book_id,
                        unit.id.clone(),
                        unit.revision.get(),
                        text,
                    )
                    .map_err(anyhow::Error::new)?;
                    let snapshot = match reference.locator {
                        Some(locator) => {
                            snapshot.with_locator(locator).map_err(anyhow::Error::new)?
                        }
                        None => snapshot,
                    };
                    resolved.push((index, snapshot));
                }
                ensure_reference_freeze_active(&read_cancellation)?;
                Ok(resolved)
            })
            .await
            .context("引用快照任务已停止")??;
        ensure_reference_freeze_active(&cancellation)?;
        for (index, snapshot) in resolved {
            slots[index] = Some(snapshot);
        }
    }

    ensure_reference_freeze_active(&cancellation)?;
    let snapshots = slots.into_iter().flatten().collect::<Vec<_>>();
    validate_snapshot_payload(&snapshots)?;
    ensure_reference_freeze_active(&cancellation)?;
    Ok(snapshots)
}

fn ensure_reference_freeze_active(cancellation: &AgentCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        bail!(AgentRequestCancelled);
    }
    Ok(())
}

fn validate_persisted_reference(
    document: &BookDocument,
    unit: &moye_epub_editor::document::ContentUnit,
    reference: &AiReferenceHint,
) -> Result<()> {
    if let Some(revision) = reference.revision
        && revision != unit.revision.get()
    {
        bail!("引用的内容单元版本已失效");
    }
    if let Some(locator) = reference.locator.as_ref() {
        document
            .validate_locator(locator)
            .context("引用定位已失效或超出内容范围")?;
        if let Some(source) = locator.source.as_ref()
            && unit.source_locator.as_ref() != Some(source)
        {
            bail!("引用的原始来源位置与当前内容单元不匹配");
        }
    }
    Ok(())
}

fn thread_options(threads: Vec<ChatThread>) -> Vec<AiThreadOption> {
    threads
        .into_iter()
        .map(|thread| AiThreadOption::new(thread.id, thread.title, thread.scope.book_ids))
        .collect()
}

async fn load_ui_session_state(
    services: Arc<AppServices>,
    conversation: &AgentConversation,
    session: Option<ChatSession>,
    allowed_book_ids: &[String],
) -> UiSessionState {
    let active_thread_id = session
        .as_ref()
        .map(|session| session.thread.id.clone())
        .or(conversation.selected_thread_id().await);
    let fallback_thread = session.as_ref().map(|session| session.thread.clone());
    let messages = match session {
        Some(session) => match validated_restored_messages(services, session.clone()).await {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(
                    target: "moye_ai",
                    stage = "validate_restored_citations",
                    error_kind = error_kind(&error),
                    "cannot validate restored AI citations; restoring message text without sources"
                );
                let mut messages = restored_messages(session);
                for message in &mut messages {
                    message.sources.clear();
                }
                messages
            }
        },
        None => Vec::new(),
    };
    let threads = match conversation.list_sessions(allowed_book_ids).await {
        Ok(threads) => thread_options(threads),
        Err(error) => {
            tracing::warn!(target: "moye_ai", stage = "list_selected_sessions", error_kind = error_kind(&error), "cannot list AI sessions after selecting a session");
            fallback_thread
                .into_iter()
                .map(|thread| AiThreadOption::new(thread.id, thread.title, thread.scope.book_ids))
                .collect()
        }
    };
    UiSessionState {
        active_thread_id,
        threads,
        messages,
    }
}

fn restored_messages(session: ChatSession) -> Vec<AiRestoredMessage> {
    session
        .messages
        .into_iter()
        .filter_map(|message| {
            let role = match message.role {
                ChatRole::User => AiRestoredRole::User,
                ChatRole::Assistant => AiRestoredRole::Assistant,
                ChatRole::System | ChatRole::Tool => return None,
            };
            // Persisted citation-row presence is the durable grounding fact. Record it
            // before malformed, stale, or unauthorized links are filtered for display.
            let source_status = match role {
                AiRestoredRole::User => None,
                AiRestoredRole::Assistant => Some(if message.citations.is_empty() {
                    AgentAnswerSourceStatus::NoVerifiedSources
                } else {
                    AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
                }),
            };
            Some(AiRestoredMessage {
                role,
                content: message.content,
                sources: message
                    .citations
                    .into_iter()
                    .filter_map(source_link_from_chat_citation)
                    .collect(),
                source_status,
            })
        })
        .collect()
}

async fn validated_restored_messages(
    services: Arc<AppServices>,
    session: ChatSession,
) -> Result<Vec<AiRestoredMessage>> {
    let allowed_book_ids = session.thread.scope.book_ids.clone();
    let messages = restored_messages(session);
    services
        .spawn_library_read(move |library| {
            let allowed = allowed_book_ids
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut messages = messages;
            for message in &mut messages {
                message.sources.retain_mut(|source| {
                    if !allowed.contains(source.book_id.as_str()) {
                        return false;
                    }
                    let document = library.document(&source.book_id).ok();
                    source.mark_restored_status(document.as_ref());
                    true
                });
            }
            Ok(messages)
        })
        .await
        .context("恢复引用状态的任务已停止")?
}

async fn validate_live_sources(
    services: Arc<AppServices>,
    allowed_book_ids: Vec<String>,
    sources: Vec<AiSourceLink>,
) -> Result<Vec<AiSourceLink>> {
    services
        .spawn_library_read(move |library| {
            let allowed = allowed_book_ids
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut validated = Vec::with_capacity(sources.len());
            for mut source in sources {
                if !allowed.contains(source.book_id.as_str()) {
                    continue;
                }
                let document = library.document(&source.book_id).ok();
                source.mark_current_status(document.as_ref());
                validated.push(source);
            }
            Ok(validated)
        })
        .await
        .context("校验引用状态的任务已停止")?
}

fn source_link_from_agent_citation(citation: AgentCitation) -> Option<AiSourceLink> {
    if citation.is_web() {
        return Some(AiSourceLink {
            citation_id: citation.citation_id,
            book_id: String::new(),
            unit_id: String::new(),
            unit_index: None,
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            locator: None,
            label: citation
                .source_title
                .clone()
                .unwrap_or_else(|| citation.url.clone().unwrap_or_default()),
            quote: Some(citation.quote),
            selection_snapshot: false,
            stale: false,
            url: citation.url.clone(),
        });
    }
    if citation.locator.book_id != citation.book_id
        || citation.locator.unit_id != citation.unit_id
        || citation.locator.validate().is_err()
        || matches!(
            citation.locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        )
    {
        tracing::warn!(
            target: "moye_ai",
            stage = "convert_agent_citation",
            error_kind = "invalid_locator",
            "discarding AI citation with an invalid or mismatched locator"
        );
        return None;
    }
    let selection_snapshot = citation.citation_id.starts_with("selection:");
    Some(AiSourceLink {
        citation_id: citation.citation_id,
        book_id: citation.book_id,
        unit_id: citation.unit_id,
        unit_index: None,
        document_revision: citation.document_revision,
        unit_revision: citation.unit_revision,
        locator: Some(citation.locator),
        label: format!("{} · {}", citation.book_title, citation.unit_title),
        quote: Some(citation.quote),
        selection_snapshot,
        stale: false,
        url: None,
    })
}

fn source_link_from_chat_citation(citation: ChatCitation) -> Option<AiSourceLink> {
    if citation.source_kind.is_web() {
        return Some(AiSourceLink {
            citation_id: citation.id,
            book_id: String::new(),
            unit_id: String::new(),
            unit_index: None,
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            locator: None,
            label: citation
                .source_title
                .clone()
                .unwrap_or_else(|| citation.url.clone().unwrap_or_default()),
            quote: Some(citation.quote),
            selection_snapshot: false,
            stale: false,
            url: citation.url.clone(),
        });
    }
    if citation.locator.validate().is_err()
        || matches!(
            citation.locator.source.as_ref(),
            Some(SourceLocator::OfficeRenderedPage { .. })
        )
        || citation
            .content_unit_id
            .as_deref()
            .is_some_and(|unit_id| unit_id != citation.locator.unit_id)
    {
        tracing::warn!(
            target: "moye_ai",
            stage = "convert_stored_citation",
            error_kind = "invalid_locator",
            "discarding persisted AI citation with an invalid or mismatched locator"
        );
        return None;
    }
    let selection_snapshot = citation.search_chunk_id.is_none();
    Some(AiSourceLink {
        citation_id: citation.id,
        book_id: citation.locator.book_id.clone(),
        unit_id: citation.locator.unit_id.clone(),
        unit_index: None,
        document_revision: citation.document_revision,
        unit_revision: citation.unit_revision,
        locator: Some(citation.locator),
        label: "已保存的来源".to_string(),
        quote: Some(citation.quote),
        selection_snapshot,
        stale: false,
        url: None,
    })
}

fn friendly_agent_error(error: &str) -> String {
    if error.contains("connection refused") || error.contains("failed to connect") {
        "无法连接本地 Ollama。请确认 Ollama 已启动，并在 AI 设置中检查端点和模型；本应用不会代为启动服务。".to_string()
    } else if error.contains("cancelled") {
        "本次回答已取消。".to_string()
    } else if error.contains("unknown or unserved citation") {
        "模型引用了一个本次检索并未提供的来源，因此该回答未被采用。请重新提问；小模型更容易出现这种情况，可在 AI 设置中换用更强的对话模型。".to_string()
    } else {
        format!("AI 回答失败：{error}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::ai_sidebar::{AiBookOption, AiSidebarEvent, AiSidebarScope};
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use gpui_component::Root;
    use moye_epub_editor::document::{
        Block, BlockDocument, BookDocument, ContentUnit, ContentUnitKind, DocumentLocator,
        Revision, SourceKind,
    };
    use moye_epub_editor::{
        agent::AgentCitationSourceKind,
        chat::{ChatScope, ChatThread, NewChatMessage, NewChatThread, StoredChatMessage},
        library::LibraryStore,
    };
    use std::{cell::RefCell, rc::Rc, time::Duration};

    fn reference(book_id: &str, unit_id: &str, index: usize) -> AiReferenceHint {
        AiReferenceHint::chapter(book_id, unit_id, index, format!("Unit {index}"))
    }

    #[test]
    fn explains_local_ollama_connection_failure() {
        assert!(friendly_agent_error("connection refused").contains("Ollama"));
    }

    #[test]
    fn selection_explanation_rejects_missing_selection_and_untrusted_scope() {
        let allowed = vec!["book-a".to_string()];
        let mut selected = reference("book-a", "unit-1", 0);
        assert!(validate_explanation_reference(&selected, &allowed).is_err());
        selected.frozen_text = Some("  \n".to_string());
        assert!(validate_explanation_reference(&selected, &allowed).is_err());
        selected.frozen_text = Some("selected words\n精确选区".to_string());
        assert_eq!(
            selected.revision, None,
            "Reader uses host-authorized metadata"
        );
        assert!(validate_explanation_reference(&selected, &allowed).is_ok());
        assert!(validate_explanation_reference(&selected, &["other".to_string()]).is_err());
        selected.frozen_text = Some("x".repeat(MAX_REFERENCE_TOTAL_BYTES + 1));
        assert!(validate_explanation_reference(&selected, &allowed).is_err());
    }

    #[gpui::test]
    fn reader_selection_explanation_emits_once_after_fresh_session_callback(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
        let book = library.create_book("Reader fixture", "Author").unwrap();
        let document = library.document(&book.id).unwrap();
        let mut selected = reference(&book.id, &document.units[0].id, 0);
        selected.locator = Some(DocumentLocator::unit(&book.id, &document.units[0].id));
        selected.frozen_text = Some("original menu selection".to_string());
        assert_eq!(selected.revision, None);
        drop(library);
        let services = Arc::new(AppServices::open(temp.path()).unwrap());
        let runtime = services.runtime();
        let controller = AiSidebarController::new(
            Arc::clone(&services),
            ChatWindowKind::Reader,
            Some(book.id.clone()),
        )
        .unwrap();
        let backend = controller.conversation.clone();
        let thread = runtime.block_on(async {
            let repository = services.chat();
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: Some(book.id.clone()),
                    title: "Previous question".to_string(),
                    scope: ChatScope::new([book.id.clone()]).unwrap(),
                    window_kind: ChatWindowKind::Reader,
                })
                .await
                .unwrap();
            backend
                .select_session(&thread.id, std::slice::from_ref(&book.id))
                .await
                .unwrap();
            thread
        });
        cx.update(gpui_component::init);
        let mut sidebar = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                AiSidebar::new(
                    AiSidebarScope::book(AiBookOption::new(&book.id, "Reader fixture"), Vec::new()),
                    Arc::clone(&services),
                    window,
                    cx,
                )
            });
            sidebar = Some(view.clone());
            Root::new(view, window, cx)
        });
        let sidebar = sidebar.unwrap();
        sidebar.update(visual, |sidebar, cx| {
            sidebar.apply_session(
                Some(thread.id.clone()),
                vec![AiThreadOption::new(
                    &thread.id,
                    &thread.title,
                    vec![book.id.clone()],
                )],
                vec![AiRestoredMessage {
                    role: AiRestoredRole::User,
                    content: "Previous question".to_string(),
                    sources: Vec::new(),
                    source_status: None,
                }],
                cx,
            );
        });
        let controller = visual.new(|_| controller);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&requests);
        let _subscription = sidebar.update(visual, |_, cx| {
            cx.subscribe(&sidebar, move |_, _, event, _| {
                if let AiSidebarEvent::Submit(request) = event {
                    recorded.borrow_mut().push(request.clone());
                }
            })
        });
        controller.update(visual, |controller, cx| {
            assert!(controller.explain_selection(selected.clone(), sidebar.clone(), cx));
            assert!(!controller.explain_selection(selected.clone(), sidebar.clone(), cx));
            assert!(
                requests.borrow().is_empty(),
                "submission waits for backend reset"
            );
            let mut changed = selected.clone();
            changed.frozen_text = Some("later live selection".to_string());
            sidebar.update(cx, |sidebar, cx| {
                sidebar.set_reference_hints(vec![changed], cx)
            });
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while requests.borrow().is_empty() && Instant::now() < deadline {
            visual.run_until_parked();
            // The product uses an independent Tokio runtime, outside GPUI's
            // deterministic executor. Give that real worker a bounded turn.
            std::thread::sleep(Duration::from_millis(1));
        }
        let emitted = requests.borrow();
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].question, "详细解释一下");
        assert_eq!(emitted[0].reference_hints, vec![selected]);
        assert_eq!(runtime.block_on(backend.selected_thread_id()), None);
        assert!(!sidebar.read_with(visual, |sidebar, _| sidebar.is_collapsed()));
        controller.update(visual, |controller, _| controller.close());
    }

    #[test]
    fn fresh_ui_session_waits_for_backend_reset_and_keeps_old_persisted_history() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let conversation =
            AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None)
                .expect("create conversation");
        runtime.block_on(async {
            let repository = services.chat();
            let thread = repository
                .create_thread(NewChatThread {
                    primary_book_id: None,
                    title: "Previous question".to_string(),
                    scope: ChatScope::default(),
                    window_kind: ChatWindowKind::Library,
                })
                .await
                .unwrap();
            repository
                .append_message(
                    &thread.id,
                    NewChatMessage::text(ChatRole::User, "Previous question"),
                )
                .await
                .unwrap();
            conversation.select_session(&thread.id, &[]).await.unwrap();
            let prepared = conversation.prepare_request(41).unwrap();
            assert!(
                start_new_ui_session(Arc::clone(&services), conversation.clone(), Vec::new())
                    .await
                    .is_err()
            );
            assert_eq!(
                conversation.selected_thread_id().await,
                Some(thread.id.clone())
            );
            assert!(!prepared.cancellation().is_cancelled());
            drop(prepared);

            let state =
                start_new_ui_session(Arc::clone(&services), conversation.clone(), Vec::new())
                    .await
                    .unwrap();
            assert_eq!(state.active_thread_id, None);
            assert!(state.messages.is_empty());
            assert_eq!(state.threads.len(), 1);
            assert_eq!(state.threads[0].id, thread.id);
            assert_eq!(conversation.selected_thread_id().await, None);
            assert!(conversation.restore(&[]).await.unwrap().is_none());
            let saved = repository.session(&thread.id).await.unwrap().unwrap();
            assert_eq!(saved.messages.len(), 1);
            assert_eq!(saved.messages[0].content, "Previous question");
        });
    }

    #[test]
    fn editor_frozen_text_replaces_only_the_matching_selected_reference() {
        let first = reference("book-a", "unit-1", 0);
        let second = reference("book-a", "unit-2", 1);
        let mut frozen = first.clone();
        frozen.revision = Some(27);
        frozen.frozen_text = Some("未保存的正文\nexact bytes".to_string());

        let merged = merge_editor_reference(vec![first, second.clone()], Some(frozen.clone()));

        assert_eq!(merged, vec![frozen.clone(), second]);
        let snapshot = capture_frozen_reference(&merged[0]).unwrap().unwrap();
        assert_eq!(snapshot.text, "未保存的正文\nexact bytes");
        assert_eq!(snapshot.revision, 27);
        assert!(snapshot.validate().is_ok());

        let outside = reference("book-secret", "unit-x", 0);
        assert_eq!(
            merge_editor_reference(merged.clone(), Some(outside)),
            merged
        );
    }

    #[test]
    fn controller_rejects_out_of_scope_duplicate_and_too_many_references() {
        let allowed = vec!["book-a".to_string()];
        assert!(validate_reference_request(&[reference("book-a", "unit-1", 0)], &allowed).is_ok());
        assert!(
            validate_reference_request(&[reference("book-secret", "unit-1", 0)], &allowed)
                .unwrap_err()
                .to_string()
                .contains("授权范围")
        );
        let duplicate = reference("book-a", "unit-1", 0);
        assert!(
            validate_reference_request(&[duplicate.clone(), duplicate], &allowed)
                .unwrap_err()
                .to_string()
                .contains("重复")
        );

        let too_many = (0..=MAX_SELECTED_REFERENCES)
            .map(|index| reference("book-a", &format!("unit-{index}"), index))
            .collect::<Vec<_>>();
        assert!(
            validate_reference_request(&too_many, &allowed)
                .unwrap_err()
                .to_string()
                .contains("最多引用")
        );
    }

    #[test]
    fn controller_rejects_preview_only_and_mismatched_locators() {
        let allowed = vec!["book-a".to_string()];
        let mut preview_only = reference("book-a", "unit-1", 0);
        preview_only.locator = Some(
            DocumentLocator::unit("book-a", "unit-1")
                .with_source(SourceLocator::office_rendered_page(1)),
        );
        assert!(
            validate_reference_request(&[preview_only], &allowed)
                .unwrap_err()
                .to_string()
                .contains("派生数据")
        );

        let mut source_locator = reference("book-a", "unit-1", 0);
        source_locator.locator =
            Some(DocumentLocator::unit("book-a", "unit-1").with_source(SourceLocator::pdf_page(1)));
        assert!(validate_reference_request(&[source_locator], &allowed).is_ok());

        let mut mismatched = reference("book-a", "unit-1", 0);
        mismatched.locator = Some(DocumentLocator::unit("book-a", "unit-2"));
        assert!(
            validate_reference_request(&[mismatched], &allowed)
                .unwrap_err()
                .to_string()
                .contains("不一致")
        );
    }

    #[test]
    fn live_and_persisted_citations_keep_only_valid_exact_locators() {
        let locator =
            DocumentLocator::unit("book-a", "unit-1").with_source(SourceLocator::pdf_page(2));
        let live = source_link_from_agent_citation(AgentCitation {
            citation_id: "passage:p-1".to_string(),
            book_id: "book-a".to_string(),
            book_title: "Book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_title: "Unit".to_string(),
            document_revision: Revision::new(1),
            unit_revision: Revision::new(1),
            quote: "evidence".to_string(),
            locator: locator.clone(),
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
        })
        .expect("valid live citation");
        assert_eq!(live.locator.as_ref(), Some(&locator));
        assert!(!live.selection_snapshot);

        let live_selection = source_link_from_agent_citation(AgentCitation {
            citation_id: "selection:host-issued".to_string(),
            book_id: "book-a".to_string(),
            book_title: "Book".to_string(),
            unit_id: "unit-1".to_string(),
            unit_title: "Unit".to_string(),
            document_revision: Revision::new(1),
            unit_revision: Revision::new(1),
            quote: "unsaved selection".to_string(),
            locator: locator.clone(),
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
        })
        .expect("valid live selection citation");
        assert!(live_selection.selection_snapshot);

        let restored = source_link_from_chat_citation(ChatCitation {
            id: "citation-1".to_string(),
            content_unit_id: Some("unit-1".to_string()),
            search_chunk_id: Some("p-1".to_string()),
            document_revision: Revision::new(1),
            unit_revision: Revision::new(1),
            quote: "evidence".to_string(),
            locator: locator.clone(),
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
            created_at: 1,
        })
        .expect("valid persisted citation");
        assert_eq!(restored.locator.as_ref(), Some(&locator));
        assert!(!restored.selection_snapshot);

        assert!(
            source_link_from_agent_citation(AgentCitation {
                citation_id: "passage:forged".to_string(),
                book_id: "book-a".to_string(),
                book_title: "Book".to_string(),
                unit_id: "unit-2".to_string(),
                unit_title: "Other".to_string(),
                document_revision: Revision::new(1),
                unit_revision: Revision::new(1),
                quote: "forged".to_string(),
                locator,
                source_kind: AgentCitationSourceKind::Book,
                url: None,
                source_title: None,
            })
            .is_none()
        );
    }

    #[test]
    fn web_citations_keep_a_url_instead_of_a_book_locator() {
        let link = source_link_from_chat_citation(ChatCitation {
            id: "web-citation".to_string(),
            content_unit_id: None,
            search_chunk_id: None,
            document_revision: Revision::new(0),
            unit_revision: Revision::new(0),
            quote: "verified snippet".to_string(),
            locator: DocumentLocator::unit("", ""),
            created_at: 1,
            source_kind: AgentCitationSourceKind::Web,
            url: Some("https://example.test/docs".to_string()),
            source_title: Some("Docs".to_string()),
        })
        .expect("web citation must survive restoration");
        assert_eq!(link.url.as_deref(), Some("https://example.test/docs"));
        assert_eq!(link.label, "Docs");
        assert!(link.locator.is_none());
        assert!(link.book_id.is_empty());
    }

    #[test]
    fn restored_source_status_uses_rows_before_ui_source_filtering() {
        let thread_id = "thread-source-status".to_string();
        let cited = StoredChatMessage {
            id: "assistant-cited".to_string(),
            thread_id: thread_id.clone(),
            parent_id: None,
            ordinal: 0,
            role: ChatRole::Assistant,
            content: "grounded answer".to_string(),
            model: Some("mock".to_string()),
            created_at: 1,
            citations: vec![ChatCitation {
                id: "citation-invalid-for-ui".to_string(),
                content_unit_id: Some("unit-a".to_string()),
                search_chunk_id: Some("chunk-a".to_string()),
                document_revision: Revision::new(1),
                unit_revision: Revision::new(1),
                quote: "evidence".to_string(),
                locator: DocumentLocator::unit("book-a", "different-unit"),
                created_at: 1,
                source_kind: AgentCitationSourceKind::Book,
                url: None,
                source_title: None,
            }],
        };
        let uncited = StoredChatMessage {
            id: "assistant-uncited".to_string(),
            thread_id: thread_id.clone(),
            parent_id: Some(cited.id.clone()),
            ordinal: 1,
            role: ChatRole::Assistant,
            content: "general answer".to_string(),
            model: Some("mock".to_string()),
            created_at: 2,
            citations: Vec::new(),
        };
        let restored = restored_messages(ChatSession {
            thread: ChatThread {
                id: thread_id,
                primary_book_id: None,
                title: "Question".to_string(),
                scope: ChatScope::default(),
                window_kind: ChatWindowKind::Library,
                created_at: 1,
                updated_at: 2,
            },
            messages: vec![cited, uncited],
        });

        assert!(restored[0].sources.is_empty());
        assert_eq!(
            restored[0].source_status,
            Some(AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources),
            "a persisted citation row must keep the answer grounded even when its UI link is rejected"
        );
        assert_eq!(
            restored[1].source_status,
            Some(AgentAnswerSourceStatus::NoVerifiedSources)
        );
    }

    #[test]
    fn persisted_reference_checks_current_revision_and_text_bounds() {
        let unit = ContentUnit::new(
            "unit-1",
            ContentUnitKind::Chapter,
            "One",
            SourceKind::Markdown,
            "body",
            BlockDocument::new(vec![Block::paragraph("block-1", "body")]),
        );
        let mut document = BookDocument::created("book-a", "Book");
        document.units.push(unit);
        let unit = &document.units[0];

        let mut reference = reference("book-a", "unit-1", 0);
        reference.revision = Some(unit.revision.get());
        reference.locator = Some(DocumentLocator::text("book-a", "unit-1", "block-1", 0, 4));
        assert!(validate_persisted_reference(&document, unit, &reference).is_ok());

        reference.revision = Some(unit.revision.get() + 1);
        assert!(validate_persisted_reference(&document, unit, &reference).is_err());
        reference.revision = Some(unit.revision.get());
        reference.locator = Some(DocumentLocator::text("book-a", "unit-1", "block-1", 0, 99));
        assert!(validate_persisted_reference(&document, unit, &reference).is_err());

        reference.locator =
            Some(DocumentLocator::unit("book-a", "unit-1").with_source(SourceLocator::pdf_page(1)));
        assert!(
            validate_persisted_reference(&document, unit, &reference)
                .unwrap_err()
                .to_string()
                .contains("原始来源位置")
        );
    }

    #[test]
    fn combined_snapshot_payload_has_a_hard_total_byte_limit() {
        let within = vec![
            SelectionSnapshot::capture("book-a", "unit-1", 1, "a".repeat(1024)).unwrap(),
            SelectionSnapshot::capture("book-a", "unit-2", 2, "b".repeat(1024)).unwrap(),
        ];
        assert!(validate_snapshot_payload(&within).is_ok());

        let oversized = vec![
            SelectionSnapshot::capture(
                "book-a",
                "unit-large",
                3,
                "x".repeat(MAX_REFERENCE_TOTAL_BYTES + 1),
            )
            .unwrap(),
        ];
        assert!(
            validate_snapshot_payload(&oversized)
                .unwrap_err()
                .to_string()
                .contains("总大小")
        );
    }

    #[test]
    fn reference_freeze_stops_when_the_prepared_request_is_cancelled() {
        let temp = tempfile::tempdir().expect("temp dir");
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();
        let cancellation = AgentCancellation::default();
        cancellation.cancel();

        let error = runtime
            .block_on(freeze_references(services, Vec::new(), &[], cancellation))
            .expect_err("cancelled freeze must stop before reading the library");
        assert!(error.to_string().contains("cancelled"));
        assert!(error.is::<AgentRequestCancelled>());
        assert_eq!(error_kind(&error), "cancelled");
    }

    #[test]
    fn restored_unsaved_selection_is_stale_when_current_ast_cannot_prove_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut library = LibraryStore::load_from(temp.path().to_path_buf()).expect("open library");
        let record = library
            .create_book("Selection restore", "Author")
            .expect("create book");
        let document = library.document(&record.id).expect("load document");
        let unit = document.units.first().expect("default chapter");
        let locator = match unit.source_locator.clone() {
            Some(source) => DocumentLocator::unit(&document.id, &unit.id).with_source(source),
            None => DocumentLocator::unit(&document.id, &unit.id),
        };
        let session = ChatSession {
            thread: ChatThread {
                id: "thread-selection".to_string(),
                primary_book_id: None,
                title: "Question".to_string(),
                scope: ChatScope::new([document.id.clone()]).unwrap(),
                window_kind: ChatWindowKind::Library,
                created_at: 1,
                updated_at: 1,
            },
            messages: vec![StoredChatMessage {
                id: "assistant-selection".to_string(),
                thread_id: "thread-selection".to_string(),
                parent_id: None,
                ordinal: 0,
                role: ChatRole::Assistant,
                content: "Answer".to_string(),
                model: Some("mock".to_string()),
                created_at: 1,
                citations: vec![ChatCitation {
                    id: "stored-selection".to_string(),
                    content_unit_id: Some(unit.id.clone()),
                    search_chunk_id: None,
                    document_revision: document.revision,
                    unit_revision: unit.revision,
                    quote: "unsaved text absent from the current AST".to_string(),
                    locator,
                    created_at: 1,
                    source_kind: AgentCitationSourceKind::Book,
                    url: None,
                    source_title: None,
                }],
            }],
        };
        drop(library);
        let services = Arc::new(AppServices::open(temp.path()).expect("open services"));
        let runtime = services.runtime();

        let restored = runtime
            .block_on(validated_restored_messages(services, session))
            .expect("restore messages");
        let source = &restored[0].sources[0];
        assert!(source.selection_snapshot);
        assert!(source.stale);
    }
}
