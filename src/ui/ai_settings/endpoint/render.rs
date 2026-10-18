//! The panel and its cards of the Endpoint settings window.

use super::*;

impl EndpointSettingsWindow {
    #[inline(never)]
    pub(crate) fn render_panel(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        div()
            .v_flex()
            .gap_5()
            .child(self.render_endpoint_card(cx))
            .child(self.render_api_key_card(cx))
            .into_any_element()
    }

    #[inline(never)]
    pub(crate) fn render_endpoint_card(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
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
                render_input_field(
                    theme,
                    busy,
                    "每分钟请求数上限（RPM）",
                    "留空或 0＝自动：本机地址不限，远程地址按 10 节流。服务商按 Endpoint 计量，所以同一个地址上的问答、翻译、向量、视觉与模型检测共用这一份额度：额度用满时请求在本地等待，问答与模型检测优先，不会硬撞服务商的限制。想固定上限就填数字（最多 10000），急着跑完可以调大；服务商返回 429 时整个 Endpoint 一起退避。",
                    &endpoint.requests_per_minute_input,
                ),
                render_input_field(
                    theme,
                    busy,
                    "每请求文本块数",
                    "留空或 0＝自动：本机地址一次一块（弱模型更容易把多个块合并成一条，打包会让一处坏形状牵连更多块），远程地址一次最多 8 块，省下往返与计费。填 1 表示关掉打包（一块一请求），2–64 是固定上限；整本翻译会按输出预算、块数与请求体 256 KB 三个上限自己决定一批装几块，装不下就少装。打包只改变一次请求带走几块：译文、缓存与进度仍按块，可以随时改，不会让已翻好的部分失效。",
                    &endpoint.blocks_per_request_input,
                ),
                detection.unwrap_or_else(|| div().into_any_element()),
            ],
        )
    }

    #[inline(never)]
    pub(crate) fn render_endpoint_list(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
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
    pub(crate) fn render_api_key_card(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
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
    pub(crate) fn render_footer_actions(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
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
