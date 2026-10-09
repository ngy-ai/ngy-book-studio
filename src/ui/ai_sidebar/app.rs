//! Construction, width and scope handling, reference hints and the
//! selection-explanation flow.

use super::*;

impl AiSidebar {
    pub(crate) fn new(
        scope: AiSidebarScope,
        services: Arc<AppServices>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("向这些图书提问…（Ctrl+Enter 发送）")
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.observe_window_bounds(window, |this, window, cx| {
                if this.layout.resize(window.viewport_size().width) {
                    cx.notify();
                }
            }),
        ];
        let endpoint_status = AiEndpointStatus::current(&services);
        let endpoint_services = Arc::clone(&services);
        let endpoint_refresh_task = cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(AI_ENDPOINT_REFRESH_INTERVAL)
                    .await;
                let endpoint_status = AiEndpointStatus::current(&endpoint_services);
                if view
                    .update(cx, |this, cx| {
                        if this.endpoint_status != endpoint_status {
                            this.endpoint_status = endpoint_status;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            conversation: ConversationState::new(scope),
            sessions: SessionPickerState::default(),
            selection_explanation_request: None,
            pending_scope_sync: false,
            layout: ResponsiveSidebar::new(window.viewport_size().width),
            expanded_width: px(AI_SIDEBAR_WIDTH),
            endpoint_status,
            input,
            _endpoint_refresh_task: endpoint_refresh_task,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_expanded_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        let width = clamp_ai_sidebar_width(width);
        if self.expanded_width != width {
            self.expanded_width = width;
            cx.notify();
        }
    }

    pub(crate) fn expanded_width(&self) -> Pixels {
        self.expanded_width
    }

    pub(crate) fn is_collapsed(&self) -> bool {
        self.layout.collapsed
    }

    pub(crate) fn set_scope(&mut self, scope: AiSidebarScope, cx: &mut Context<Self>) {
        if self.conversation.set_scope(scope) {
            self.begin_scope_sync(cx);
        }
    }

    pub(crate) fn authorized_book_ids(&self) -> Vec<String> {
        self.conversation.scope.book_ids()
    }

    /// 宿主与测试观察用：当前显示的消息文本序列。
    #[allow(dead_code)]
    pub(crate) fn transcript_contents(&self) -> Vec<String> {
        self.conversation
            .messages
            .iter()
            .map(|message| message.content.clone())
            .collect()
    }

    /// 宿主与测试观察用：最近一次会话操作的错误提示。
    #[allow(dead_code)]
    pub(crate) fn session_error(&self) -> Option<&str> {
        self.sessions.error.as_deref()
    }

    pub(crate) fn set_reference_hint(
        &mut self,
        reference: Option<AiReferenceHint>,
        cx: &mut Context<Self>,
    ) {
        let references = reference.into_iter().collect::<Vec<_>>();
        if self.conversation.reference_hints != references {
            self.conversation.set_reference_hints(references);
            cx.notify();
        }
    }

    pub(crate) fn set_reference_hints(
        &mut self,
        references: Vec<AiReferenceHint>,
        cx: &mut Context<Self>,
    ) {
        if self.conversation.reference_hints != references {
            self.conversation.set_reference_hints(references);
            cx.notify();
        }
    }

    pub(crate) fn begin_session_operation(&mut self, cx: &mut Context<Self>) -> bool {
        if self.conversation.is_sending() || self.pending_scope_sync || !self.sessions.begin() {
            return false;
        }
        cx.notify();
        true
    }

    pub(crate) fn show_explanation_error(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.layout.collapsed = false;
        self.layout.manual_override = true;
        // A rejected context-menu action must not clear another operation's
        // busy flag or cancel the answer currently visible in the sidebar.
        self.sessions.error = Some(error.into());
        cx.notify();
    }

    pub(crate) fn begin_selection_explanation(&mut self, cx: &mut Context<Self>) -> bool {
        self.layout.collapsed = false;
        self.layout.manual_override = true;
        if !self.begin_session_operation(cx) {
            self.show_explanation_error("AI 正在处理当前请求，请完成或取消后再使用 AI 解释。", cx);
            return false;
        }
        true
    }

    pub(crate) fn complete_selection_explanation(
        &mut self,
        threads: Vec<AiThreadOption>,
        reference: AiReferenceHint,
        cx: &mut Context<Self>,
    ) {
        if !self.apply_session(None, threads, Vec::new(), cx)
            || self.sessions.busy
            || self.pending_scope_sync
        {
            self.reject_selection_explanation(
                "AI 图书范围正在更新，请更新完成后再次使用 AI 解释。",
                cx,
            );
            return;
        }
        let Some(request) = self.conversation.begin_selection_explanation(reference) else {
            self.reject_selection_explanation(
                "所选文本已不在当前 AI 范围内，请重新选择后再试。",
                cx,
            );
            return;
        };
        self.selection_explanation_request = Some(request.request_id);
        cx.emit(AiExplanationSubmitted {
            request_id: request.request_id,
        });
        cx.emit(AiSidebarEvent::Submit(request));
        cx.notify();
    }

    pub(super) fn reject_selection_explanation(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let error = error.into();
        self.show_explanation_error(error.clone(), cx);
        cx.emit(AiExplanationFailed {
            request_id: None,
            error,
        });
    }

    pub(crate) fn fail_selection_explanation_session(
        &mut self,
        error: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let error = error.into();
        self.fail_session_operation(error.clone(), cx);
        cx.emit(AiExplanationFailed {
            request_id: None,
            error,
        });
    }

    pub(super) fn fail_selection_explanation_request(
        &mut self,
        request_id: u64,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if self.selection_explanation_request == Some(request_id) {
            self.selection_explanation_request = None;
            cx.emit(AiExplanationFailed {
                request_id: Some(request_id),
                error,
            });
        }
    }

    pub(super) fn begin_scope_sync(&mut self, cx: &mut Context<Self>) {
        if self.conversation.is_sending() || self.sessions.busy {
            self.pending_scope_sync = true;
            cx.notify();
            return;
        }

        let book_ids = self.conversation.scope.book_ids();
        let allowed = book_ids.iter().cloned().collect::<HashSet<_>>();
        let active_is_unauthorized =
            self.sessions
                .active_thread_id
                .as_deref()
                .is_some_and(|active| {
                    self.sessions
                        .threads
                        .iter()
                        .find(|thread| thread.id == active)
                        .is_none_or(|thread| !thread_scope_is_authorized(thread, &allowed))
                });
        if active_is_unauthorized {
            self.sessions.active_thread_id = None;
            self.conversation.messages.clear();
            self.conversation.included_references.clear();
        }

        self.pending_scope_sync = false;
        if self.sessions.begin() {
            cx.emit(AiSidebarEvent::ScopeChanged { book_ids });
        }
        cx.notify();
    }

    pub(super) fn resume_pending_scope_sync(&mut self, cx: &mut Context<Self>) {
        if self.pending_scope_sync && !self.conversation.is_sending() && !self.sessions.busy {
            self.begin_scope_sync(cx);
        }
    }
}
