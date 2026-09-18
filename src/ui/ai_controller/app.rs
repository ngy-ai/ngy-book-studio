//! Construction, restoring a session and the session switch commands.

use super::*;

impl AiSidebarController {
    pub(crate) fn new(
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

    pub(crate) fn restore<Owner: 'static>(
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

    pub(crate) fn new_session<Owner: 'static>(
        &mut self,
        sidebar: Entity<AiSidebar>,
        cx: &mut Context<Owner>,
    ) {
        self.create_session(sidebar, None, cx);
    }

    /// Wait for the backend to leave the old conversation before publishing
    /// the fixed question. The owned hint survives later Reader selection
    /// changes, while the ordinary Submit handler still owns cancellation.
    pub(crate) fn explain_selection<Owner: 'static>(
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

    pub(crate) fn create_session<Owner: 'static>(
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
                    let error = format!("无法新建 AI 会话：{error}");
                    if explanation.is_some() {
                        sidebar.fail_selection_explanation_session(error, cx);
                    } else {
                        sidebar.fail_session_operation(error, cx);
                    }
                }
            });
        }));
    }

    pub(crate) fn switch_session<Owner: 'static>(
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
}
