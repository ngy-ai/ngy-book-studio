//! Committing the settings a window collected.

use super::*;

/// 提交一份设置，并在成功后关掉发起它的窗口。
///
/// 等待有上限（[`SAVE_WAIT_LIMIT`]）：保存要依次拿两把进程级门闩，被后台任务堵住时
/// 不会自己结束。超时只丢掉这次「等待」——写入仍由已经在跑的任务完成或回滚，所以结果
/// 是不确定的，提示必须如实说。
pub(crate) fn commit_settings<V: SettingsWindowView>(
    services: Arc<AppServices>,
    settings: ProviderSettings,
    key_updates: BTreeMap<String, ApiKeyUpdate>,
    web_key_update: ApiKeyUpdate,
    confirmation: Option<SaveConfirmation>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let success_notice = V::section().success_notice();
    cx.spawn_in(window, async move |view, cx| {
        if let Some(confirmation) = confirmation
            && !confirmation.answer.await
        {
            let _ = view.update(cx, |this, cx| {
                this.state_mut().operation = PendingOperation::Idle;
                this.state_mut().report(confirmation.cancelled, false);
                cx.notify();
            });
            return;
        }
        // AppServices moves credential and SQLite work to its dedicated runtime;
        // this UI future only awaits and applies the result.
        let save = Box::pin(services.configure_providers(settings, key_updates));
        let outcome =
            match futures_util::future::select(save, Box::pin(cx.background_executor().timer(SAVE_WAIT_LIMIT))).await
            {
                futures_util::future::Either::Left((outcome, _)) => Some(outcome),
                futures_util::future::Either::Right((_, _)) => None,
            };
        let outcome = outcome.map(|outcome| {
            outcome.and_then(|()| match &web_key_update {
                ApiKeyUpdate::Keep => Ok(()),
                ApiKeyUpdate::Set(value) => services.set_web_search_api_key(value),
                ApiKeyUpdate::Delete => services.delete_web_search_api_key(),
            })
        });
        let _ = view.update(cx, |this, cx| {
            let window_handle = this.state().window_handle;
            this.state_mut().operation = PendingOperation::Idle;
            let Some(outcome) = outcome else {
                this.state_mut().report(
                    format!(
                        "保存已等待 {} 秒仍未结束，仍在后台进行：设置可能已经保存，也可能仍被后台任务阻塞。\
                         可以直接关闭本窗口，稍后重新打开确认后再决定是否重存。",
                        SAVE_WAIT_LIMIT.as_secs()
                    ),
                    true,
                );
                // 正在退出软件时不能把退出流程留在这里等：这次等待的上限已经到了，
                // 窗口不再替它把关（写入仍在运行，进程退出可能打断它，后果与用户
                // 直接结束进程相同）。非退出时保留窗口，由用户决定关闭还是稍后重存。
                if application_is_exiting(cx) {
                    cx.spawn(async move |_entity, cx| {
                        let _ = window_handle.update(cx, |_, window, cx| {
                            remove_window_after_current_frame(window, cx, None);
                        });
                    })
                    .detach();
                }
                cx.notify();
                return;
            };
            match outcome {
                Ok(()) => {
                    this.state_mut().report(success_notice, false);
                    // Saved settings now reach documents that are already open; the
                    // write above is the source of truth.
                    this.apply_saved_preferences(cx);
                    cx.spawn(async move |_entity, cx| {
                        let _ = window_handle.update(cx, |_, window, cx| {
                            remove_window_after_current_frame(window, cx, None);
                        });
                    })
                    .detach();
                }
                Err(error) => {
                    cancel_application_exit(cx);
                    this.state_mut()
                        .report(format!("保存设置失败：{error:#}"), true);
                }
            }
            cx.notify();
        });
    })
    .detach();
}
