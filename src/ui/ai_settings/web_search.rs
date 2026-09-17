//! 联网搜索设置窗口：宿主在书中检索不到时可选的一次联网补充。
//!
//! 保存时以已保存的 `ProviderSettings` 为底，只覆盖联网搜索字段；搜索专用的 API key
//! 与 Endpoint 密钥一样只写进 Windows Credential Manager。

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_component::checkbox::Checkbox;
use ngy_book_studio::ai::web_search::{
    DEFAULT_WEB_SEARCH_TIMEOUT_SECS, MAX_WEB_SEARCH_RESULTS, MAX_WEB_SEARCH_TIMEOUT_SECS,
    MIN_WEB_SEARCH_TIMEOUT_SECS, normalize_web_endpoint,
};
use ngy_book_studio::services::{ApiKeyUpdate, ProviderSettings};

pub(super) struct WebSearchSettingsWindow {
    services: Arc<AppServices>,
    web_search_enabled: bool,
    web_search_url_input: Entity<InputState>,
    web_search_method_input: Entity<InputState>,
    web_search_body_input: Entity<InputState>,
    web_search_key_header_input: Entity<InputState>,
    web_search_api_key_input: Entity<InputState>,
    web_search_timeout_input: Entity<InputState>,
    web_search_max_results_input: Entity<InputState>,
    web_search_remote_confirmed: bool,
    web_search_allow_insecure_http: bool,
    web_search_confirmed_remote_endpoint: String,
    delete_web_search_api_key: bool,
    state: SettingsWindowState,
}

impl WebSearchSettingsWindow {
    pub(super) fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let web_search_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_url_template.clone())
                .placeholder("http://127.0.0.1:8080/search?q={query}&format=json")
        });
        let web_search_method_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_method.clone())
                .placeholder("GET")
                .validate(|value, _| matches!(value.to_ascii_uppercase().as_str(), "GET" | "POST"))
        });
        let web_search_body_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(
                    settings
                        .web_search_body_template
                        .clone()
                        .unwrap_or_default(),
                )
                .placeholder(r#"{"query":"{query}"}"#)
        });
        let web_search_key_header_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_key_header.clone().unwrap_or_default())
                .placeholder("Authorization")
        });
        let web_search_api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则保持现有密钥")
        });
        let web_search_timeout_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_timeout_secs.to_string())
                .placeholder(DEFAULT_WEB_SEARCH_TIMEOUT_SECS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let web_search_max_results_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.web_search_max_results.to_string())
                .placeholder(MAX_WEB_SEARCH_RESULTS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        Self {
            services,
            web_search_enabled: settings.web_search_enabled,
            web_search_url_input,
            web_search_method_input,
            web_search_body_input,
            web_search_key_header_input,
            web_search_api_key_input,
            web_search_timeout_input,
            web_search_max_results_input,
            web_search_remote_confirmed: settings.web_search_remote_confirmed,
            web_search_allow_insecure_http: settings.web_search_allow_insecure_http,
            web_search_confirmed_remote_endpoint: settings.web_search_confirmed_remote_endpoint,
            delete_web_search_api_key: false,
            state: SettingsWindowState::new(window),
        }
    }

    /// 以已保存设置为底，只覆盖联网搜索字段。
    fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        let body_template = {
            let raw = self.web_search_body_input.read(cx).value();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let key_header = {
            let raw = self.web_search_key_header_input.read(cx).value();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        settings.web_search_enabled = self.web_search_enabled;
        settings.web_search_url_template = self
            .web_search_url_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        settings.web_search_method = self
            .web_search_method_input
            .read(cx)
            .value()
            .trim()
            .to_ascii_uppercase();
        settings.web_search_body_template = body_template;
        settings.web_search_key_header = key_header;
        settings.web_search_remote_confirmed = self.web_search_remote_confirmed;
        settings.web_search_allow_insecure_http = self.web_search_allow_insecure_http;
        settings.web_search_confirmed_remote_endpoint =
            self.web_search_confirmed_remote_endpoint.clone();
        settings.web_search_timeout_secs =
            parse_web_search_timeout_secs(self.web_search_timeout_input.read(cx).value().as_ref())?;
        settings.web_search_max_results = parse_web_search_max_results(
            self.web_search_max_results_input.read(cx).value().as_ref(),
        )?;
        settings.validate()?;
        Ok(settings)
    }

    fn web_api_key_update(&self, cx: &App) -> ApiKeyUpdate {
        api_key_update_for_input(
            self.web_search_api_key_input.read(cx).value().as_ref(),
            self.delete_web_search_api_key,
        )
    }

    fn clear_entered_web_api_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.web_search_api_key_input.clone();
        input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    #[inline(never)]
    fn render_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let web_enable_view = cx.entity();
        let web_confirm_view = cx.entity();
        let web_insecure_view = cx.entity();
        let web_delete_view = cx.entity();
        let web_url_input = self.web_search_url_input.clone();
        let busy = self.state.busy();

        panel(
            theme,
            vec![
                div()
                    .h_flex()
                    .justify_between()
                    .child(
                        div()
                            .v_flex()
                            .gap_0p5()
                            .child(card_title(theme, "联网搜索（可选）"))
                            .child(card_note(
                                theme,
                                "书中检索不到时，宿主补充一次联网搜索作为回答依据；关闭则不联网。",
                            )),
                    )
                    .child(
                        Checkbox::new("ai-web-search-enabled")
                            .checked(self.web_search_enabled)
                            .disabled(busy)
                            .label("启用")
                            .debug_selector(|| "ai-web-search-enabled".into())
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                web_enable_view.update(cx, |this, cx| {
                                    this.web_search_enabled = checked;
                                    cx.notify();
                                });
                            })
                            .into_any_element(),
                    )
                    .into_any_element(),
                render_input_field(
                    theme,
                    busy,
                    "请求模板（URL）",
                    "必须包含 {query}；可配置 SearxNG、Brave、Tavily 等端点。",
                    &self.web_search_url_input,
                ),
                render_input_field(
                    theme,
                    busy,
                    "方法",
                    "GET 或 POST；POST 端点需填写下方的请求体模板。",
                    &self.web_search_method_input,
                ),
                render_input_field(
                    theme,
                    busy,
                    "请求体模板（可选）",
                    "POST 时使用，需包含 {query}；留空则用 GET。",
                    &self.web_search_body_input,
                ),
                render_input_field(
                    theme,
                    busy,
                    "API key 头（可选）",
                    "自定义携带密钥的请求头名；留空则用 Authorization: Bearer。",
                    &self.web_search_key_header_input,
                ),
                Input::new(&self.web_search_api_key_input)
                    .mask_toggle()
                    .disabled(busy || self.delete_web_search_api_key || !self.web_search_enabled)
                    .into_any_element(),
                card_note(
                    theme,
                    "联网搜索 API key 只存入 Windows Credential Manager；留空保持、输入替换。",
                ),
                Checkbox::new("ai-web-confirm-remote")
                    .checked(self.web_search_remote_confirmed)
                    .disabled(busy || !self.web_search_enabled)
                    .label("确认允许把检索词发送到远程搜索端点")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        web_confirm_view.update(cx, |this, cx| {
                            if checked {
                                let entered = web_url_input.read(cx).value();
                                match normalize_web_endpoint(entered.trim()) {
                                    Some(url) => {
                                        this.web_search_remote_confirmed = true;
                                        this.web_search_confirmed_remote_endpoint = url;
                                    }
                                    None => {
                                        this.web_search_remote_confirmed = false;
                                        this.web_search_confirmed_remote_endpoint.clear();
                                        this.state.report("联网搜索端点无效，无法确认。", true);
                                    }
                                }
                            } else {
                                this.web_search_remote_confirmed = false;
                                this.web_search_allow_insecure_http = false;
                                this.web_search_confirmed_remote_endpoint.clear();
                            }
                            cx.notify();
                        });
                    })
                    .into_any_element(),
                Checkbox::new("ai-web-allow-insecure-http")
                    .checked(self.web_search_allow_insecure_http)
                    .disabled(busy || !self.web_search_enabled || !self.web_search_remote_confirmed)
                    .label("额外允许非 HTTPS 的搜索端点（内容可能被窃听）")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        web_insecure_view.update(cx, |this, cx| {
                            this.web_search_allow_insecure_http = checked;
                            cx.notify();
                        });
                    })
                    .into_any_element(),
                render_input_field(
                    theme,
                    busy,
                    "超时（秒）",
                    "单次联网搜索请求的总超时。",
                    &self.web_search_timeout_input,
                ),
                render_input_field(
                    theme,
                    busy,
                    "结果条数",
                    "返回给模型的最大搜索结果数量。",
                    &self.web_search_max_results_input,
                ),
                Checkbox::new("ai-web-delete-api-key")
                    .checked(self.delete_web_search_api_key)
                    .disabled(busy || !self.web_search_enabled)
                    .label("删除已保存的联网搜索 API key")
                    .on_click(move |checked, window, cx| {
                        let checked = *checked;
                        web_delete_view.update(cx, |this, cx| {
                            this.delete_web_search_api_key = checked;
                            if checked {
                                this.clear_entered_web_api_key(window, cx);
                            }
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            ],
        )
    }
}

impl SettingsWindowView for WebSearchSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::WebSearch
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().web_search
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
                    .report(format!("联网搜索设置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        let web_key_update = self.web_api_key_update(cx);
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            BTreeMap::new(),
            web_key_update,
            None,
            window,
            cx,
        );
    }
}

impl Render for WebSearchSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        settings_frame(self, body, None, cx)
    }
}

fn parse_web_search_timeout_secs(value: &str) -> Result<u64> {
    let value = value.trim();
    let timeout = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "联网搜索超时必须是 {MIN_WEB_SEARCH_TIMEOUT_SECS} 到 {MAX_WEB_SEARCH_TIMEOUT_SECS} 之间的整数秒数"
        )
    })?;
    anyhow::ensure!(
        (MIN_WEB_SEARCH_TIMEOUT_SECS..=MAX_WEB_SEARCH_TIMEOUT_SECS).contains(&timeout),
        "联网搜索超时必须是 {MIN_WEB_SEARCH_TIMEOUT_SECS} 到 {MAX_WEB_SEARCH_TIMEOUT_SECS} 之间的整数秒数"
    );
    Ok(timeout)
}

fn parse_web_search_max_results(value: &str) -> Result<usize> {
    let value = value.trim();
    let count = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("联网搜索结果数必须是 1 到 {MAX_WEB_SEARCH_RESULTS} 之间的整数")
    })?;
    anyhow::ensure!(
        (1..=MAX_WEB_SEARCH_RESULTS).contains(&count),
        "联网搜索结果数必须是 1 到 {MAX_WEB_SEARCH_RESULTS} 之间的整数"
    );
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn open_window(
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        Entity<WebSearchSettingsWindow>,
        &mut VisualTestContext,
    ) {
        open_settings_view::<WebSearchSettingsWindow>(cx, WebSearchSettingsWindow::new)
    }

    #[gpui::test]
    fn search_drafts_parse_without_saving(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        let (url, timeout, results) = settings.read_with(visual, |view, _| {
            (
                view.web_search_url_input.clone(),
                view.web_search_timeout_input.clone(),
                view.web_search_max_results_input.clone(),
            )
        });
        edit_input(
            visual,
            &url,
            "http://127.0.0.1:18082/search?q={query}&format=json",
        );
        set_input_value(visual, &timeout, "0");
        settings.read_with(visual, |view, cx| {
            assert!(view.entered_settings(cx).is_err());
        });
        set_input_value(visual, &timeout, "20");
        set_input_value(visual, &results, "3");
        settings.read_with(visual, |view, cx| {
            let entered = view.entered_settings(cx).unwrap();
            assert_eq!(
                entered.web_search_url_template,
                "http://127.0.0.1:18082/search?q={query}&format=json"
            );
            assert_eq!(entered.web_search_timeout_secs, 20);
            assert_eq!(entered.web_search_max_results, 3);
            assert!(!entered.web_search_enabled);
            let saved = view.services.provider_settings().unwrap();
            assert!(saved.web_search_url_template.is_empty());
            assert_eq!(
                saved.web_search_timeout_secs,
                DEFAULT_WEB_SEARCH_TIMEOUT_SECS
            );
        });
    }

    #[gpui::test]
    fn enabling_search_needs_a_url_and_stays_a_draft(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        click(visual, "ai-web-search-enabled");
        settings.read_with(visual, |view, cx| {
            assert!(view.web_search_enabled);
            assert!(
                view.entered_settings(cx).is_err(),
                "启用后必须有可用的请求模板，否则保存会被拒绝"
            );
            assert!(
                !view
                    .services
                    .provider_settings()
                    .unwrap()
                    .web_search_enabled,
                "勾选不得直接写入设置"
            );
        });

        let url = settings.read_with(visual, |view, _| view.web_search_url_input.clone());
        edit_input(visual, &url, "http://127.0.0.1:18082/search?q={query}");
        settings.read_with(visual, |view, cx| {
            assert!(view.entered_settings(cx).unwrap().web_search_enabled);
            assert!(
                !view
                    .services
                    .provider_settings()
                    .unwrap()
                    .web_search_enabled,
                "勾选不得直接写入设置"
            );
        });
    }

    #[test]
    fn search_timeout_and_result_ranges_are_enforced() {
        assert_eq!(parse_web_search_timeout_secs("1").unwrap(), 1);
        assert_eq!(parse_web_search_timeout_secs(" 20 ").unwrap(), 20);
        for invalid in ["", "0", "-1", "1.5", "abc"] {
            assert!(parse_web_search_timeout_secs(invalid).is_err(), "{invalid}");
        }
        assert_eq!(parse_web_search_max_results("1").unwrap(), 1);
        assert_eq!(
            parse_web_search_max_results(&MAX_WEB_SEARCH_RESULTS.to_string()).unwrap(),
            MAX_WEB_SEARCH_RESULTS
        );
        for invalid in ["", "0", "-1", "1.5", "abc"] {
            assert!(parse_web_search_max_results(invalid).is_err(), "{invalid}");
        }
    }
}
