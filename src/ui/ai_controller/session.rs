//! Deleting a session and reconciling the scope with the library.

use super::*;

impl AiSidebarController {
    pub(crate) fn delete_session<Owner: 'static>(
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

    pub(crate) fn reconcile_scope<Owner: 'static>(
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
                    tracing::warn!(target: "ngy_ai", stage = "list_sessions_after_scope_reset", error_kind = error_kind(&anyhow::Error::msg(error)), "cannot list sessions after resetting AI scope");
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
}
