//! The commands the sidebar widgets raise.

use super::*;

impl AiSidebar {
    /// Called before the owner releases a Reader/Editor WebView or removes the
    /// library window. The returned ID lets a future adapter cancel its task;
    /// stale callbacks are rejected by the state machine either way.
    pub(crate) fn cancel_for_window_close(&mut self, cx: &mut Context<Self>) -> Option<u64> {
        // 宿主窗口要没了：朗读会话不能再继续（它占着音频输出，也持有本
        // view 的登记项）。
        self.stop_read_aloud(cx);
        let request_id = self.conversation.cancel();
        if let Some(request_id) = request_id {
            cx.emit(AiSidebarEvent::Cancel { request_id });
            self.fail_selection_explanation_request(request_id, "本次 AI 解释已取消。".into(), cx);
            cx.notify();
            Some(request_id)
        } else {
            None
        }
    }

    pub(super) fn on_input_event(
        &mut self,
        _input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            event,
            InputEvent::PressEnter {
                secondary: true,
                ..
            }
        ) {
            self.submit(window, cx);
        }
    }

    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync {
            return;
        }
        let question = self.input.read(cx).value().to_string();
        let Some(request) = self.conversation.begin(&question) else {
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(AiSidebarEvent::Submit(request));
        cx.notify();
    }

    /// Starts editing one user message: its text moves back into the input
    /// box, the message and everything after it leave the transcript, and the
    /// stored thread rewinds to that message once the edit is submitted.
    pub(super) fn edit_message(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.busy || self.pending_scope_sync {
            return;
        }
        let Some(text) = self.conversation.begin_edit(index) else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Regenerates one answer: the question that produced it is asked again
    /// and the stored thread rewinds to that question, so the replacement
    /// answer takes the old turn's place.
    pub(super) fn regenerate_message(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync {
            return;
        }
        let Some(question) = self.conversation.begin_regenerate(index) else {
            return;
        };
        let Some(request) = self.conversation.begin(&question) else {
            return;
        };
        cx.emit(AiSidebarEvent::Submit(request));
        cx.notify();
    }

    /// Deletes one question and its answer.
    ///
    /// A question with a recorded stored ID is removed through the controller
    /// and the sidebar waits for the reloaded transcript: a storage failure
    /// must leave the displayed conversation untouched instead of showing a
    /// deletion that never reached the database. A question that never reached
    /// persistence (a failed or cancelled request) only leaves the local
    /// transcript.
    pub(super) fn delete_turn(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync {
            return;
        }
        let Some(deletion) = self.conversation.turn_deletion(index) else {
            return;
        };
        match deletion {
            TurnDeletion::LocalOnly => {
                if self.conversation.discard_turn(index) {
                    cx.notify();
                }
            }
            TurnDeletion::Stored(user_db_id) => {
                if !self.sessions.begin() {
                    return;
                }
                cx.emit(AiSidebarEvent::DeleteTurn { user_db_id });
                cx.notify();
            }
        }
    }

    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.conversation.cancel() else {
            return;
        };
        cx.emit(AiSidebarEvent::Cancel { request_id });
        self.fail_selection_explanation_request(request_id, "本次 AI 解释已取消。".into(), cx);
        cx.notify();
    }

    pub(super) fn toggle_session_picker(&mut self, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync || self.conversation.is_sending() {
            return;
        }
        self.sessions.open = !self.sessions.open;
        self.sessions.error = None;
        self.sessions.pending_delete = None;
        cx.notify();
    }

    pub(super) fn new_session(&mut self, cx: &mut Context<Self>) {
        if !self.begin_session_operation(cx) {
            return;
        }
        // 消息列表即将整体替换：正在朗读的旧消息马上就不存在了。
        self.stop_read_aloud(cx);
        cx.emit(AiSidebarEvent::NewSession);
    }

    pub(super) fn switch_session(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.sessions.active_thread_id.as_deref() == Some(thread_id) {
            self.sessions.open = false;
            cx.notify();
            return;
        }
        if !self.begin_session_operation(cx) {
            return;
        }
        // 同上：切会话会换掉整个消息列表。
        self.stop_read_aloud(cx);
        cx.emit(AiSidebarEvent::SwitchSession {
            thread_id: thread_id.to_string(),
        });
    }

    pub(super) fn confirm_delete_session(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.sessions.busy || self.pending_scope_sync || self.conversation.is_sending() {
            return;
        }
        let allowed = self
            .conversation
            .scope
            .book_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let Some(thread) =
            self.sessions.threads.iter().find(|thread| {
                thread.id == thread_id && thread_scope_is_authorized(thread, &allowed)
            })
        else {
            self.sessions.error = Some("该会话已不存在或不在当前图书范围内。".to_string());
            cx.notify();
            return;
        };
        self.sessions.pending_delete = Some(PendingSessionDeletion {
            thread_id: thread.id.clone(),
            title: thread.title.clone(),
        });
        self.sessions.error = None;
        cx.notify();
    }

    pub(super) fn cancel_delete_session(&mut self, cx: &mut Context<Self>) {
        if self.sessions.pending_delete.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn delete_session(&mut self, thread_id: &str, cx: &mut Context<Self>) -> bool {
        if !self.begin_session_operation(cx) {
            return false;
        }
        cx.emit(AiSidebarEvent::DeleteSession {
            thread_id: thread_id.to_string(),
        });
        true
    }

    pub(super) fn toggle_reference(&mut self, reference: &AiReferenceHint, cx: &mut Context<Self>) {
        if self.conversation.toggle_reference(reference) {
            cx.notify();
        }
    }

    pub(super) fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.layout.toggle();
        cx.notify();
    }

    pub(super) fn add_book(&mut self, book_id: &str, cx: &mut Context<Self>) {
        if self.conversation.scope.add_book(book_id) {
            self.begin_scope_sync(cx);
        }
    }

    pub(super) fn remove_book(&mut self, book_id: &str, cx: &mut Context<Self>) {
        if self.conversation.scope.remove_book(book_id) {
            self.conversation.prune_references_to_scope();
            self.begin_scope_sync(cx);
        }
    }
}
