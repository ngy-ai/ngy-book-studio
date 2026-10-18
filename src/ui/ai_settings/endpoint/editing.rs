//! Draft and selection state changes of the Endpoint settings window.

use super::*;

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
        settings.requests_per_minute = default_endpoint.requests_per_minute;
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
            window.blur(cx);
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
            // 新端点从「自动」起步：地址填成远程时会自己带上默认的节流。
            requests_per_minute: 0,
            // 打包同样从「自动」起步：本机端点一次一块，远程端点按批上限。
            blocks_per_request: 0,
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
}
