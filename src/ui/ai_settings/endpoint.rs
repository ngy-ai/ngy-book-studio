//! Endpoint 设置窗口：连接、密钥与远程授权。
//!
//! 这里只管 Endpoint 本身；模型绑定与模型名在模型设置窗口里选。保存时以已保存的
//! `ProviderSettings` 为底，只覆盖 Endpoint 列表与各 Endpoint 的密钥。

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_component::{Selectable as _, checkbox::Checkbox};
use ngy_book_studio::ai::{DEFAULT_AI_REQUEST_TIMEOUT_SECS, normalize_provider_base_url};
use ngy_book_studio::services::{
    ApiKeyUpdate, DEFAULT_ENDPOINT_ID, EndpointSettings, ProviderSettings,
};

pub(crate) struct EndpointSettingsWindow {
    pub(crate) services: Arc<AppServices>,
    pub(crate) endpoints: Vec<EndpointDraft>,
    pub(crate) selected_endpoint_id: String,
    /// 已保存设置里各模型绑定的 Endpoint。删除时用它挡住仍被模型使用的连接；
    /// 每次删除前重新读一遍已保存设置，避免用旧快照放走正在被使用的 Endpoint。
    pub(crate) chat_endpoint_id: String,
    pub(crate) translation_endpoint_id: String,
    pub(crate) embedding_endpoint_id: String,
    pub(crate) vision_endpoint_id: String,
    pub(crate) state: SettingsWindowState,
}

impl EndpointSettingsWindow {
    pub(crate) fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let endpoints = settings
            .endpoints()
            .into_iter()
            .map(|endpoint| new_endpoint_draft(endpoint, window, cx))
            .collect();
        Self {
            services,
            endpoints,
            selected_endpoint_id: DEFAULT_ENDPOINT_ID.to_string(),
            chat_endpoint_id: settings.endpoint_routing.chat_endpoint_id,
            translation_endpoint_id: settings.endpoint_routing.translation_endpoint_id,
            embedding_endpoint_id: settings.endpoint_routing.embedding_endpoint_id,
            vision_endpoint_id: settings.endpoint_routing.vision_endpoint_id,
            state: SettingsWindowState::new(window),
        }
    }

    /// 以已保存设置为底，只覆盖本窗口负责的字段。
    pub(crate) fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        let mut endpoints = self
            .endpoints
            .iter()
            .map(|endpoint| endpoint.entered(cx))
            .collect::<Result<Vec<_>>>()?;
        let default_index = endpoints
            .iter()
            .position(|endpoint| endpoint.id == DEFAULT_ENDPOINT_ID)
            .context("缺少默认 Endpoint")?;
        let default_endpoint = endpoints.remove(default_index);
        settings.base_url = default_endpoint.base_url;
        settings.remote_content_confirmed = default_endpoint.remote_content_confirmed;
        settings.allow_insecure_remote_http = default_endpoint.allow_insecure_remote_http;
        settings.confirmed_remote_endpoint = default_endpoint.confirmed_remote_endpoint;
        settings.request_timeout_secs = default_endpoint.request_timeout_secs;
        settings.endpoint_routing.default_endpoint_name = default_endpoint.name;
        settings.endpoint_routing.additional_endpoints = endpoints;
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn selected_endpoint(&self) -> &EndpointDraft {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.id == self.selected_endpoint_id)
            .expect("the selected endpoint remains in the draft registry")
    }

    pub(crate) fn select_endpoint(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.busy() || !self.endpoints.iter().any(|endpoint| endpoint.id == id) {
            return;
        }
        if self.selected_endpoint_id != id {
            // Keep drafts and input history while removing focus from hidden fields.
            window.blur();
        }
        self.selected_endpoint_id = id.to_string();
        self.state.notice = None;
        cx.notify();
    }

    pub(crate) fn add_endpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let mut bytes = [0_u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            self.state.report("无法生成 Endpoint 标识，请重试。", true);
            cx.notify();
            return;
        }
        let id = format!(
            "endpoint-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let endpoint = EndpointSettings {
            id: id.clone(),
            name: format!("Endpoint {}", self.endpoints.len() + 1),
            base_url: String::new(),
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: String::new(),
            request_timeout_secs: DEFAULT_AI_REQUEST_TIMEOUT_SECS,
        };
        self.endpoints
            .push(new_endpoint_draft(endpoint, window, cx));
        self.select_endpoint(&id, window, cx);
    }

    /// 这个 Endpoint 是否仍被某个模型使用。
    ///
    /// 取「已保存设置」与「窗口打开时的快照」的并集：前者承接模型设置窗口里刚保存的
    /// 绑定，后者兜住已保存设置读不出来、以及两个窗口同时打开时本窗口还没来得及看到新
    /// 绑定的情况。真正的兜底仍是保存时的 `ProviderSettings::validate`（它会在绑定已经
    /// 不存在时报错），这里只是尽量提前把话说明白。
    pub(crate) fn endpoint_is_bound(&self, id: &str) -> bool {
        let saved_bound = self.services.provider_settings().is_ok_and(|settings| {
            [
                settings.endpoint_routing.chat_endpoint_id,
                settings.endpoint_routing.translation_endpoint_id,
                settings.endpoint_routing.embedding_endpoint_id,
                settings.endpoint_routing.vision_endpoint_id,
            ]
            .into_iter()
            .any(|bound| bound == id)
        });
        saved_bound
            || [
                &self.chat_endpoint_id,
                &self.translation_endpoint_id,
                &self.embedding_endpoint_id,
                &self.vision_endpoint_id,
            ]
            .into_iter()
            .any(|bound| bound == id)
    }

    pub(crate) fn delete_selected_endpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() || self.selected_endpoint_id == DEFAULT_ENDPOINT_ID {
            return;
        }
        if self.endpoint_is_bound(&self.selected_endpoint_id) {
            self.state.report(
                "这个 Endpoint 仍被模型使用，请先在「模型设置」窗口中重新选择对应 Endpoint。",
                true,
            );
            cx.notify();
            return;
        }
        self.endpoints
            .retain(|endpoint| endpoint.id != self.selected_endpoint_id);
        self.select_endpoint(DEFAULT_ENDPOINT_ID, window, cx);
    }

    #[inline(never)]
    pub(crate) fn render_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .v_flex()
            .gap_5()
            .child(self.render_endpoint_card(cx))
            .child(self.render_api_key_card(cx))
            .into_any_element()
    }

    #[inline(never)]
    pub(crate) fn render_endpoint_card(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let endpoint = self.selected_endpoint();
        let busy = self.state.busy();
        let remote_view = cx.entity();
        let insecure_view = cx.entity();
        let endpoint_for_confirmation = endpoint.base_url_input.clone();
        let remote_endpoint_id = endpoint.id.clone();
        let insecure_endpoint_id = endpoint.id.clone();
        let detection = render_model_detection(self, endpoint, None, cx);

        panel(
            theme,
            vec![
                self.render_endpoint_list(cx),
                render_input_field(
                    theme,
                    busy,
                    "Endpoint 名称",
                    "用于区分连接；各模型按此名称选择 Endpoint。",
                    &endpoint.name_input,
                ),
                render_input_field(
                    theme,
                    busy,
                    "Endpoint",
                    "默认使用本机 Ollama 的 http://127.0.0.1:11434/v1/；不会静默切换到云端。",
                    &endpoint.base_url_input,
                ),
                Checkbox::new("ai-confirm-remote")
                    .checked(endpoint.remote_content_confirmed)
                    .disabled(busy)
                    .label("确认允许把图书内容发送到远程 endpoint")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        remote_view.update(cx, |this, cx| {
                            let Some(endpoint) = this
                                .endpoints
                                .iter_mut()
                                .find(|endpoint| endpoint.id == remote_endpoint_id)
                            else {
                                return;
                            };
                            if checked {
                                let entered = endpoint_for_confirmation.read(cx).value();
                                match normalize_provider_base_url(entered.trim()) {
                                    Ok(url) => {
                                        endpoint.remote_content_confirmed = true;
                                        endpoint.confirmed_remote_endpoint = url.to_string();
                                    }
                                    Err(error) => {
                                        endpoint.remote_content_confirmed = false;
                                        endpoint.confirmed_remote_endpoint.clear();
                                        this.state.report(
                                            format!("Endpoint 无效，无法确认：{error:#}"),
                                            true,
                                        );
                                    }
                                }
                            } else {
                                endpoint.remote_content_confirmed = false;
                                endpoint.allow_insecure_remote_http = false;
                                endpoint.confirmed_remote_endpoint.clear();
                            }
                            cx.notify();
                        });
                    })
                    .into_any_element(),
                Checkbox::new("ai-allow-insecure-http")
                    .checked(endpoint.allow_insecure_remote_http)
                    .disabled(busy || !endpoint.remote_content_confirmed)
                    .label("额外允许非 HTTPS 的远程 endpoint（内容可能被窃听）")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        insecure_view.update(cx, |this, cx| {
                            if let Some(endpoint) = this
                                .endpoints
                                .iter_mut()
                                .find(|endpoint| endpoint.id == insecure_endpoint_id)
                            {
                                endpoint.allow_insecure_remote_http = checked;
                            }
                            cx.notify();
                        });
                    })
                    .into_any_element(),
                render_input_field(
                    theme,
                    busy,
                    "请求超时（秒）",
                    "可设置 1–600 秒，默认 120 秒。非流式请求按整段调用计时；流式回答按“多久没有新数据”计时，只要模型持续输出就不会被中途中断。",
                    &endpoint.request_timeout_input,
                ),
                detection.unwrap_or_else(|| div().into_any_element()),
            ],
        )
    }

    #[inline(never)]
    pub(crate) fn render_endpoint_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let busy = self.state.busy();
        div()
            .v_flex()
            .gap_2()
            .child(div().h_flex().flex_wrap().gap_2().children(
                self.endpoints.iter().enumerate().map(|(index, endpoint)| {
                    let id = endpoint.id.clone();
                    Button::new(("ai-endpoint-choice", index))
                        .label(endpoint.name_input.read(cx).value())
                        .selected(self.selected_endpoint_id == endpoint.id)
                        .disabled(busy)
                        .debug_selector(move || format!("ai-endpoint-choice-{index}"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_endpoint(&id, window, cx)
                        }))
                }),
            ))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        Button::new("ai-add-endpoint")
                            .label("新增 Endpoint")
                            .disabled(busy)
                            .debug_selector(|| "ai-add-endpoint".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_endpoint(window, cx)
                            })),
                    )
                    .child(
                        Button::new("ai-delete-endpoint")
                            .label("删除 Endpoint")
                            .disabled(busy || self.selected_endpoint_id == DEFAULT_ENDPOINT_ID)
                            .debug_selector(|| "ai-delete-endpoint".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delete_selected_endpoint(window, cx)
                            })),
                    ),
            )
            .child(card_note(
                theme,
                "每个 Endpoint 独立保存连接、密钥和远程授权。模型使用中的 Endpoint 需先在「模型设置」窗口里重新分配才能删除。",
            ))
            .into_any_element()
    }

    #[inline(never)]
    pub(crate) fn render_api_key_card(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let endpoint = self.selected_endpoint();
        let endpoint_id = endpoint.id.clone();
        let delete_view = cx.entity();
        let busy = self.state.busy();

        panel(
            theme,
            vec![
                card_title(theme, "API key"),
                Input::new(&endpoint.api_key_input)
                    .mask_toggle()
                    .disabled(busy || endpoint.delete_api_key)
                    .into_any_element(),
                card_note(
                    theme,
                    "现有密钥不会回填明文。留空表示保持；输入新值表示替换。密钥只存入 Windows Credential Manager。",
                ),
                Checkbox::new("ai-delete-api-key")
                    .checked(endpoint.delete_api_key)
                    .disabled(busy)
                    .label("删除这个 endpoint 已保存的 API key")
                    .on_click(move |checked, window, cx| {
                        let checked = *checked;
                        let endpoint_id = endpoint_id.clone();
                        delete_view.update(cx, |this, cx| {
                            set_delete_endpoint_key(this, &endpoint_id, checked, window, cx);
                        });
                    })
                    .into_any_element(),
            ],
        )
    }

    /// 底栏上的「检测模型」：检测当前选中的 Endpoint，结果可用于模型设置窗口。
    pub(crate) fn render_footer_actions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let detecting = self.state.operation == PendingOperation::Detecting;
        Button::new("ai-endpoint-detect-models")
            .debug_selector(|| "ai-endpoint-detect-models".into())
            .outline()
            .icon(IconName::Search)
            .label(if detecting {
                "正在检测…"
            } else {
                "检测模型"
            })
            .disabled(self.state.busy())
            .on_click(cx.listener(|this, _, window, cx| {
                let endpoint_id = this.selected_endpoint_id.clone();
                detect_models(this, &endpoint_id, window, cx);
            }))
            .into_any_element()
    }
}

impl SettingsWindowView for EndpointSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::Endpoint
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().endpoint
    }

    fn state(&self) -> &SettingsWindowState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SettingsWindowState {
        &mut self.state
    }

    fn services(&self) -> &Arc<AppServices> {
        &self.services
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let settings = match self.entered_settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.state
                    .report(format!("Endpoint 设置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        let key_updates: BTreeMap<_, _> = self
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.id.clone(), endpoint.api_key_update(cx)))
            .collect();
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            key_updates,
            ApiKeyUpdate::Keep,
            None,
            window,
            cx,
        );
    }
}

impl EndpointBackedWindow for EndpointSettingsWindow {
    fn endpoints(&self) -> &[EndpointDraft] {
        &self.endpoints
    }

    fn endpoints_mut(&mut self) -> &mut Vec<EndpointDraft> {
        &mut self.endpoints
    }
}

impl Render for EndpointSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        let footer_extra = self.render_footer_actions(cx);
        settings_frame(self, body, Some(footer_extra), cx)
    }
}

#[cfg(test)]
mod tests {
    use super::endpoint_draft::{apply_detected_models, canonical_endpoint, detection_matches};
    use super::tests_support::*;
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    fn open_window(
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        Entity<EndpointSettingsWindow>,
        &mut VisualTestContext,
    ) {
        open_settings_view::<EndpointSettingsWindow>(cx, EndpointSettingsWindow::new)
    }

    #[gpui::test]
    fn endpoint_switches_preserve_drafts_and_role_bindings_block_deletion(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        visual.simulate_resize(size(px(1080.), px(1400.)));
        redraw(visual);
        let default_key = settings.read_with(visual, |view, _| {
            view.selected_endpoint().api_key_input.clone()
        });
        edit_input(visual, &default_key, "fixture-default-key");
        let add = visual.debug_bounds("ai-add-endpoint").unwrap();
        visual.simulate_click(add.center(), Modifiers::none());
        redraw(visual);
        assert_hidden_input_does_not_receive_text(visual, &default_key, "fixture-default-key");
        let (extra_id, extra_url) = settings.read_with(visual, |view, cx| {
            let endpoint = view.selected_endpoint();
            assert_ne!(endpoint.id, DEFAULT_ENDPOINT_ID);
            assert!(!endpoint.remote_content_confirmed);
            assert!(!endpoint.allow_insecure_remote_http);
            assert_eq!(endpoint.api_key_update(cx), ApiKeyUpdate::Keep);
            (endpoint.id.clone(), endpoint.base_url_input.clone())
        });
        edit_input(visual, &extra_url, "http://127.0.0.1:18082/v1/");
        let extra_key = settings.read_with(visual, |view, _| {
            view.selected_endpoint().api_key_input.clone()
        });
        edit_input(visual, &extra_key, "fixture-extra-key");

        // 模型仍绑定这个 Endpoint 时删除应被拒绝，草稿保持原样。
        settings.update(visual, |view, cx| {
            view.embedding_endpoint_id = extra_id.clone();
            cx.notify();
        });
        redraw(visual);

        let delete = visual.debug_bounds("ai-delete-endpoint").unwrap();
        visual.simulate_click(delete.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(view.endpoints.len(), 2);
            assert!(view.state.notice().unwrap().error);
            assert_eq!(
                view.selected_endpoint().api_key_update(cx),
                ApiKeyUpdate::Set("fixture-extra-key".into())
            );
        });

        let default_choice = visual.debug_bounds("ai-endpoint-choice-0").unwrap();
        visual.simulate_click(default_choice.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, cx| {
            assert_eq!(
                view.selected_endpoint().api_key_input.entity_id(),
                default_key.entity_id()
            );
            assert_eq!(
                view.selected_endpoint().api_key_update(cx),
                ApiKeyUpdate::Set("fixture-default-key".into())
            );
            assert_eq!(
                view.endpoints[1].api_key_input.entity_id(),
                extra_key.entity_id()
            );
        });

        // 解除绑定后可以删除，选中项回到默认 Endpoint。
        visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.embedding_endpoint_id = DEFAULT_ENDPOINT_ID.to_string();
                view.select_endpoint(&extra_id, window, cx);
                view.delete_selected_endpoint(window, cx);
                assert_eq!(view.endpoints.len(), 1);
                assert_eq!(view.selected_endpoint_id, DEFAULT_ENDPOINT_ID);
            });
        });
    }

    #[gpui::test]
    fn endpoint_url_change_resets_only_its_authorization_key_and_detection(
        cx: &mut TestAppContext,
    ) {
        let (_directory, settings, visual) = open_window(cx);
        let (url_input, original_key, original_endpoint) =
            settings.read_with(visual, |view, cx| {
                let endpoint = view.selected_endpoint();
                (
                    endpoint.base_url_input.clone(),
                    endpoint.api_key_input.clone(),
                    endpoint.entered(cx).unwrap(),
                )
            });
        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                let endpoint = &mut view.endpoints[0];
                endpoint.remote_content_confirmed = true;
                endpoint.allow_insecure_remote_http = true;
                endpoint.confirmed_remote_endpoint =
                    canonical_endpoint(&original_endpoint.base_url).unwrap();
                endpoint.delete_api_key = true;
                endpoint.detected_models = Some(vec!["old-server-model".into()]);
                cx.notify();
            });
        });
        edit_input(visual, &url_input, "http://127.0.0.1:18083/v1/");
        settings.read_with(visual, |view, cx| {
            let endpoint = view.selected_endpoint();
            assert!(!endpoint.remote_content_confirmed);
            assert!(!endpoint.allow_insecure_remote_http);
            assert!(endpoint.confirmed_remote_endpoint.is_empty());
            assert!(!endpoint.delete_api_key);
            assert!(endpoint.detected_models.is_none());
            assert_ne!(endpoint.api_key_input.entity_id(), original_key.entity_id());
            assert_eq!(endpoint.api_key_update(cx), ApiKeyUpdate::Keep);
            assert!(!detection_matches(view, &original_endpoint, 0, cx));
        });
        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                let endpoint = view.selected_endpoint().entered(cx).unwrap();
                assert!(detection_matches(
                    view,
                    &endpoint,
                    view.selected_endpoint().discovery_generation,
                    cx
                ));
                apply_detected_models(view, &endpoint, vec![], false);
                assert_eq!(view.selected_endpoint().detected_models, Some(vec![]));
            });
        });
    }

    #[gpui::test]
    fn key_edits_and_key_deletion_invalidate_only_the_matching_endpoint(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        visual.simulate_resize(size(px(1080.), px(1600.)));
        let (key_input, endpoint, generation) = visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                let endpoint = view.selected_endpoint().entered(cx).unwrap();
                let other = EndpointSettings {
                    id: "second-endpoint".into(),
                    name: "另一个服务".into(),
                    base_url: "http://127.0.0.1:18084/v1/".into(),
                    ..endpoint.clone()
                };
                view.endpoints.push(new_endpoint_draft(other, window, cx));
                view.endpoints[0].detected_models = Some(vec!["account-a-model".into()]);
                view.endpoints[1].detected_models = Some(vec!["other-server-model".into()]);
                cx.notify();
                (
                    view.endpoints[0].api_key_input.clone(),
                    endpoint,
                    view.endpoints[0].discovery_generation,
                )
            })
        });
        redraw(visual);
        edit_input(visual, &key_input, "fixture-account-b-key");
        settings.read_with(visual, |view, cx| {
            assert!(view.endpoints[0].detected_models.is_none());
            assert_eq!(
                view.endpoints[0].api_key_input.entity_id(),
                key_input.entity_id()
            );
            assert!(!detection_matches(view, &endpoint, generation, cx));
            assert_eq!(
                view.endpoints[1].detected_models,
                Some(vec!["other-server-model".into()])
            );
        });
        let replacement_key = visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.endpoints[0].detected_models = Some(vec!["account-b-model".into()]);
                set_delete_endpoint_key(view, DEFAULT_ENDPOINT_ID, true, window, cx);
                assert!(view.endpoints[0].detected_models.is_none());
                view.endpoints[0].detected_models = Some(vec!["anonymous-model".into()]);
                set_delete_endpoint_key(view, DEFAULT_ENDPOINT_ID, false, window, cx);
                assert!(view.endpoints[0].detected_models.is_none());
                view.endpoints[0].detected_models = Some(vec!["cached-after-reset".into()]);
                view.endpoints[0].api_key_input.clone()
            })
        });
        redraw(visual);
        edit_input(visual, &replacement_key, "fixture-account-c-key");
        settings.read_with(visual, |view, _| {
            assert!(view.endpoints[0].detected_models.is_none())
        });
        // 检测前的校验失败不应清掉别的 Endpoint 的检测结果。
        visual.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.endpoints[0].detected_models = Some(vec!["cached-before-failure".into()]);
                view.endpoints[0]
                    .request_timeout_input
                    .update(cx, |input, cx| input.set_value("0", window, cx));
                detect_models(view, DEFAULT_ENDPOINT_ID, window, cx);
                assert_eq!(view.state.operation, PendingOperation::Idle);
                assert!(view.state.notice().unwrap().error);
                assert!(view.endpoints[0].detected_models.is_none());
                assert_eq!(
                    view.endpoints[1].detected_models,
                    Some(vec!["other-server-model".into()])
                );
            });
        });
    }
}
