//! 一个 Endpoint 在设置窗口里的草稿，以及围绕它的连接检测。
//!
//! Endpoint 与模型设置两个窗口都要列出 Endpoint、都要能检测某个 Endpoint 的模型，所以
//! 草稿与检测放在这里共用：两个窗口各持有一份自己的草稿（同步在窗口里编辑的字段彼此
//! 独立），保存时才各自提交。

use super::*;

use ngy_book_studio::{
    ai::{
        DEFAULT_AI_REQUEST_TIMEOUT_SECS, DEFAULT_OLLAMA_OPENAI_BASE_URL,
        MAX_AI_REQUEST_TIMEOUT_SECS, MAX_ENDPOINT_RPM, MIN_AI_REQUEST_TIMEOUT_SECS, ModelInfo,
        normalize_provider_base_url,
    },
    services::{ApiKeyUpdate, EndpointSettings, ModelRole},
};

/// 「每分钟请求数上限」留空时表示不限。
pub(crate) const DEFAULT_ENDPOINT_REQUESTS_PER_MINUTE: u32 = 0;

/// 一个 Endpoint 的可编辑草稿。密钥只以掩码输入框的形式存在，明文不回填。
pub(crate) struct EndpointDraft {
    pub(crate) id: String,
    pub(crate) name_input: Entity<InputState>,
    pub(crate) base_url_input: Entity<InputState>,
    pub(crate) request_timeout_input: Entity<InputState>,
    pub(crate) requests_per_minute_input: Entity<InputState>,
    pub(crate) api_key_input: Entity<InputState>,
    pub(crate) remote_content_confirmed: bool,
    pub(crate) allow_insecure_remote_http: bool,
    pub(crate) confirmed_remote_endpoint: String,
    pub(crate) delete_api_key: bool,
    /// 检测结果对应的规范化地址：地址一变，旧结果立刻作废。
    pub(crate) observed_url: String,
    /// 最近一次检测返回的模型名；`None` 表示还没检测过。
    pub(crate) detected_models: Option<Vec<String>>,
    pub(crate) discovery_generation: u64,
    pub(crate) api_key_subscription: Subscription,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl EndpointDraft {
    pub(crate) fn invalidate_models(&mut self) {
        self.detected_models = None;
        self.discovery_generation = self.discovery_generation.wrapping_add(1);
    }

    pub(crate) fn entered(&self, cx: &App) -> Result<EndpointSettings> {
        Ok(EndpointSettings {
            id: self.id.clone(),
            name: self.name_input.read(cx).value().trim().to_string(),
            base_url: self.base_url_input.read(cx).value().trim().to_string(),
            request_timeout_secs: parse_request_timeout_secs(
                self.request_timeout_input.read(cx).value().as_ref(),
            )?,
            requests_per_minute: parse_requests_per_minute(
                self.requests_per_minute_input.read(cx).value().as_ref(),
            )?,
            remote_content_confirmed: self.remote_content_confirmed,
            allow_insecure_remote_http: self.allow_insecure_remote_http,
            confirmed_remote_endpoint: self.confirmed_remote_endpoint.clone(),
        })
    }

    pub(crate) fn api_key_update(&self, cx: &App) -> ApiKeyUpdate {
        api_key_update_for_input(
            self.api_key_input.read(cx).value().as_ref(),
            self.delete_api_key,
        )
    }
}

pub(crate) fn new_endpoint_draft<V: EndpointBackedWindow>(
    endpoint: EndpointSettings,
    window: &mut Window,
    cx: &mut Context<V>,
) -> EndpointDraft {
    let observed_url = endpoint_draft_identity(&endpoint.base_url);
    let name_input = cx.new(|cx| InputState::new(window, cx).default_value(endpoint.name.clone()));
    let base_url_input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(endpoint.base_url.clone())
            .placeholder(DEFAULT_OLLAMA_OPENAI_BASE_URL)
    });
    let request_timeout_input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(endpoint.request_timeout_secs.to_string())
            .placeholder(DEFAULT_AI_REQUEST_TIMEOUT_SECS.to_string())
            .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
    });
    let requests_per_minute_input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(endpoint.requests_per_minute.to_string())
            .placeholder("留空表示不限")
            .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
    });
    let endpoint_id = endpoint.id.clone();
    let url_subscription = cx.subscribe_in(
        &base_url_input,
        window,
        move |this: &mut V, input, event, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let canonical = endpoint_draft_identity(input.read(cx).value().as_ref());
            if let Some(endpoint) = this
                .endpoints_mut()
                .iter_mut()
                .find(|endpoint| endpoint.id == endpoint_id)
                && canonical != endpoint.observed_url
            {
                endpoint.observed_url = canonical;
                endpoint.remote_content_confirmed = false;
                endpoint.allow_insecure_remote_http = false;
                endpoint.confirmed_remote_endpoint.clear();
                endpoint.delete_api_key = false;
                // A different server must never inherit a key through input undo history.
                reset_endpoint_key(endpoint, window, cx);
                endpoint.invalidate_models();
                this.state_mut().notice = None;
                cx.notify();
            }
        },
    );
    let name_subscription = cx.subscribe(&name_input, |_, _, event, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    });
    let api_key_input = new_api_key_input(window, cx);
    let api_key_subscription = subscribe_endpoint_key(&api_key_input, &endpoint.id, cx);
    EndpointDraft {
        id: endpoint.id,
        name_input,
        base_url_input,
        request_timeout_input,
        requests_per_minute_input,
        api_key_input,
        remote_content_confirmed: endpoint.remote_content_confirmed,
        allow_insecure_remote_http: endpoint.allow_insecure_remote_http,
        confirmed_remote_endpoint: endpoint.confirmed_remote_endpoint,
        delete_api_key: false,
        observed_url,
        detected_models: None,
        discovery_generation: 0,
        api_key_subscription,
        _subscriptions: vec![url_subscription, name_subscription],
    }
}

pub(crate) fn new_api_key_input<V>(window: &mut Window, cx: &mut Context<V>) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("留空则保持当前 Endpoint 的现有密钥")
    })
}

pub(crate) fn subscribe_endpoint_key<V: EndpointBackedWindow>(
    input: &Entity<InputState>,
    endpoint_id: &str,
    cx: &mut Context<V>,
) -> Subscription {
    let endpoint_id = endpoint_id.to_string();
    cx.subscribe(input, move |this, _, event, cx| {
        if matches!(event, InputEvent::Change)
            && let Some(endpoint) = this
                .endpoints_mut()
                .iter_mut()
                .find(|endpoint| endpoint.id == endpoint_id)
        {
            endpoint.invalidate_models();
            this.state_mut().notice = None;
            cx.notify();
        }
    })
}

/// 换一个地址就换一套密钥：清空输入框并重建订阅，撤销历史不会把旧密钥带回来。
pub(crate) fn reset_endpoint_key<V: EndpointBackedWindow>(
    endpoint: &mut EndpointDraft,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    endpoint.api_key_input = new_api_key_input(window, cx);
    endpoint.api_key_subscription =
        subscribe_endpoint_key(&endpoint.api_key_input, &endpoint.id, cx);
}

/// 勾选/取消「删除已保存的密钥」。勾选时同时清空输入框，避免撤销把旧值带回来。
pub(crate) fn set_delete_endpoint_key<V: EndpointBackedWindow>(
    this: &mut V,
    endpoint_id: &str,
    delete: bool,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    if let Some(endpoint) = this
        .endpoints_mut()
        .iter_mut()
        .find(|endpoint| endpoint.id == endpoint_id)
    {
        endpoint.delete_api_key = delete;
        endpoint.invalidate_models();
        if delete {
            reset_endpoint_key(endpoint, window, cx);
        }
        this.state_mut().notice = None;
        cx.notify();
    }
}

/// 用当前已保存的 Endpoint 列表刷新草稿列表，返回列表是否真的变了。
///
/// 模型设置窗口的 Endpoint 选择来自打开窗口时的一份快照，而 Endpoint 列表由「Endpoint」
/// 窗口维护：那边新增、改名、改地址或删除并保存之后，这份快照必须跟上，否则新 Endpoint
/// 在模型设置里根本选不到。同一个 id 且已保存取值逐字相同的草稿原样保留（输入内容与最近
/// 一次检测结果都不丢）；取值变了或新增的按已保存值重建；消失的移除；顺序以已保存列表为准。
///
/// 只读已保存设置，不改写草稿里的未提交输入（模型设置窗口本来就不可编辑这些字段），
/// 也不写数据库：提交仍由各窗口自己的保存路径负责。
pub(crate) fn refresh_endpoint_drafts<V: EndpointBackedWindow>(
    this: &mut V,
    saved: Vec<EndpointSettings>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> bool {
    let before: Vec<String> = this
        .endpoints()
        .iter()
        .map(|draft| draft.id.clone())
        .collect();
    let mut existing = std::mem::take(this.endpoints_mut());
    let mut refreshed = Vec::with_capacity(saved.len());
    let mut changed = false;
    for endpoint in saved {
        match existing.iter().position(|draft| draft.id == endpoint.id) {
            Some(index) => {
                let draft = existing.remove(index);
                if endpoint_matches_draft(&draft, &endpoint, cx) {
                    refreshed.push(draft);
                } else {
                    // 已保存的名称、地址或授权变了：整个草稿重建，旧的检测结果随之作废。
                    refreshed.push(new_endpoint_draft(endpoint, window, cx));
                    changed = true;
                }
            }
            None => {
                refreshed.push(new_endpoint_draft(endpoint, window, cx));
                changed = true;
            }
        }
    }
    // 已保存列表里已经没有的 Endpoint，草稿也一并去掉。
    changed |= !existing.is_empty();
    changed |= before.len() != refreshed.len()
        || before
            .iter()
            .zip(refreshed.iter())
            .any(|(id, draft)| *id != draft.id);
    *this.endpoints_mut() = refreshed;
    changed
}

/// 草稿的取值是否与已保存的 Endpoint 逐字相同。模型设置窗口不编辑 Endpoint 字段，所以
/// 只有「那边保存过改动」才会返回 false。
fn endpoint_matches_draft(draft: &EndpointDraft, saved: &EndpointSettings, cx: &App) -> bool {
    draft.name_input.read(cx).value().as_ref() == saved.name.as_str()
        && draft.base_url_input.read(cx).value().as_ref() == saved.base_url.as_str()
        && draft.request_timeout_input.read(cx).value().as_ref()
            == saved.request_timeout_secs.to_string().as_str()
        // 上限按解析后的数字比：输入框里是「7」还是「07」说的是同一件事，不该因为
        // 写法不同就把草稿连同检测结果一起重建。
        && parse_requests_per_minute(draft.requests_per_minute_input.read(cx).value().as_ref())
            .is_ok_and(|requests| requests == saved.requests_per_minute)
        && draft.remote_content_confirmed == saved.remote_content_confirmed
        && draft.allow_insecure_remote_http == saved.allow_insecure_remote_http
        && draft.confirmed_remote_endpoint == saved.confirmed_remote_endpoint
}

/// 读取某个 Endpoint 的模型列表。凭据与网络 I/O 都由 `AppServices` 派发到它自己的
/// Tokio runtime 上，这个 GPUI 任务只负责等待与应用结果。
pub(crate) fn detect_models<V: EndpointBackedWindow>(
    this: &mut V,
    endpoint_id: &str,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    if this.state().busy() {
        return;
    }
    let Some(index) = this
        .endpoints()
        .iter()
        .position(|endpoint| endpoint.id == endpoint_id)
    else {
        return;
    };
    this.endpoints_mut()[index].invalidate_models();
    let generation = this.endpoints()[index].discovery_generation;
    let endpoint = match this.endpoints()[index].entered(cx).and_then(|endpoint| {
        endpoint.validate()?;
        Ok(endpoint)
    }) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            this.state_mut()
                .report(format!("AI Provider 设置无效：{error:#}"), true);
            cx.notify();
            return;
        }
    };
    let key_update = this.endpoints()[index].api_key_update(cx);
    this.state_mut()
        .begin_detection(format!("正在读取 {} 的模型…", endpoint.name));

    let services = Arc::clone(this.services());
    cx.spawn_in(window, async move |view, cx| {
        let outcome = services
            .probe_endpoint_models(endpoint.clone(), key_update)
            .await;
        let _ = view.update(cx, |this, cx| {
            this.state_mut().operation = PendingOperation::Idle;
            if !detection_matches(this, &endpoint, generation, cx) {
                this.state_mut().notice = None;
                cx.notify();
                return;
            }
            match outcome {
                Ok(models) => {
                    // Endpoint 窗口没有模型输入，检测结果只列出来；模型设置窗口可以直接点选。
                    let picks_models = this.role_model(ModelRole::Chat, cx).is_some();
                    apply_detected_models(this, &endpoint, models, picks_models);
                }
                Err(error) => this.state_mut().report(
                    format!(
                        "无法读取 {} 的模型：{error:#}。若使用本地 Ollama，请确认服务已运行。",
                        endpoint.name
                    ),
                    true,
                ),
            }
            cx.notify();
        });
    })
    .detach();
    cx.notify();
}

/// 检测结果回到界面时，草稿是否还是发起检测的那一份（同一个 Endpoint、同一代、同一地址）。
pub(crate) fn detection_matches<V: EndpointBackedWindow>(
    this: &V,
    endpoint: &EndpointSettings,
    generation: u64,
    cx: &App,
) -> bool {
    this.endpoints().iter().any(|draft| {
        draft.id == endpoint.id
            && draft.discovery_generation == generation
            && canonical_endpoint(draft.base_url_input.read(cx).value().as_ref())
                == canonical_endpoint(&endpoint.base_url)
    })
}

pub(crate) fn apply_detected_models<V: EndpointBackedWindow>(
    this: &mut V,
    endpoint: &EndpointSettings,
    models: Vec<ModelInfo>,
    picks_models: bool,
) {
    let detected_models: Vec<_> = models.into_iter().map(|model| model.id).collect();
    let count = detected_models.len();
    if let Some(draft) = this
        .endpoints_mut()
        .iter_mut()
        .find(|draft| draft.id == endpoint.id)
    {
        draft.detected_models = Some(detected_models);
    }
    let hint = if picks_models {
        "可在各模型区域查看或选择。"
    } else {
        "可在下方列表查看，回到「模型设置」窗口即可选用。"
    };
    this.state_mut().report(
        format!("{} 连接成功，返回 {count} 个模型；{hint}", endpoint.name),
        false,
    );
}

/// 检测结果的卡片：返回的模型列表、与当前模型不匹配的名字、以及本地 Ollama 的拉取提示。
pub(crate) fn render_model_detection<V: EndpointBackedWindow>(
    view: &V,
    endpoint: &EndpointDraft,
    role: Option<ModelRole>,
    cx: &mut Context<V>,
) -> Option<gpui_kit::AnyElement> {
    let models = endpoint.detected_models.as_ref()?;
    let missing_models = role
        .and_then(|role| view.role_model(role, cx))
        .filter(|model| !models.contains(model))
        .into_iter()
        .collect::<Vec<_>>();
    let commands = ollama_pull_commands(
        endpoint.base_url_input.read(cx).value().as_ref(),
        &missing_models,
    );
    let busy = view.state().busy();
    Some(
        div()
            .v_flex()
            .gap_2()
            .p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).paper))
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(palette(cx).ink))
                    .child(format!(
                        "{} 返回的模型（{}）",
                        endpoint.name_input.read(cx).value(),
                        models.len()
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(gpui_kit::relative(1.5))
                    .text_color(rgb(palette(cx).muted))
                    .child(if models.is_empty() {
                        "未返回模型".to_string()
                    } else {
                        "模型列表不标注能力，请选择支持当前用途的模型。".to_string()
                    }),
            )
            .when(!missing_models.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).danger))
                        .child(format!(
                            "当前模型未在此 Endpoint 的列表中：{}",
                            missing_models.join("、")
                        )),
                )
            })
            .child(
                div()
                    .h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(models.iter().enumerate().map(|(index, model)| {
                        let model = model.clone();
                        let selected_role = role;
                        Button::new(("ai-detected-model", index))
                            .small()
                            .label(model.clone())
                            .disabled(busy || role.is_none())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(role) = selected_role {
                                    this.adopt_detected_model(role, &model, window, cx);
                                }
                            }))
                    })),
            )
            .children(commands.into_iter().map(|command| {
                div()
                    .px_2()
                    .py_1()
                    .rounded(px(5.))
                    .bg(rgb(palette(cx).sidebar))
                    .text_xs()
                    .text_color(rgb(palette(cx).ink))
                    .child(command)
            }))
            .into_any_element(),
    )
}

pub(crate) fn parse_request_timeout_secs(value: &str) -> Result<u64> {
    let value = value.trim();
    let timeout = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "请求超时必须是 {MIN_AI_REQUEST_TIMEOUT_SECS} 到 {MAX_AI_REQUEST_TIMEOUT_SECS} 之间的整数秒数"
        )
    })?;
    anyhow::ensure!(
        (MIN_AI_REQUEST_TIMEOUT_SECS..=MAX_AI_REQUEST_TIMEOUT_SECS).contains(&timeout),
        "请求超时必须是 {MIN_AI_REQUEST_TIMEOUT_SECS} 到 {MAX_AI_REQUEST_TIMEOUT_SECS} 之间的整数秒数"
    );
    Ok(timeout)
}

/// 「每分钟请求数上限」：留空或 `0` 都表示不限。
pub(crate) fn parse_requests_per_minute(value: &str) -> Result<u32> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(DEFAULT_ENDPOINT_REQUESTS_PER_MINUTE);
    }
    let requests = value.parse::<u32>().map_err(|_| {
        anyhow::anyhow!(
            "每分钟请求数上限必须是 0 到 {MAX_ENDPOINT_RPM} 之间的整数；留空或 0 表示不限"
        )
    })?;
    anyhow::ensure!(
        requests <= MAX_ENDPOINT_RPM,
        "每分钟请求数上限必须是 0 到 {MAX_ENDPOINT_RPM} 之间的整数；留空或 0 表示不限"
    );
    Ok(requests)
}

pub(crate) fn canonical_endpoint(endpoint: &str) -> Option<String> {
    normalize_provider_base_url(endpoint.trim())
        .ok()
        .map(|url| url.to_string())
}

pub(crate) fn endpoint_draft_identity(endpoint: &str) -> String {
    canonical_endpoint(endpoint).unwrap_or_else(|| endpoint.trim().to_string())
}

pub(crate) fn ollama_pull_commands(endpoint: &str, missing_models: &[String]) -> Vec<String> {
    if !looks_like_local_ollama(endpoint) {
        return Vec::new();
    }
    missing_models
        .iter()
        .map(|model| format!("ollama pull {model}"))
        .collect()
}

pub(crate) fn looks_like_local_ollama(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint.trim()) else {
        return false;
    };
    let local_host = url.host_str().is_some_and(|host| {
        let host = host.trim_matches(['[', ']']);
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    local_host && url.port_or_known_default() == Some(11_434)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_guidance_is_only_generated_for_local_ollama() {
        let missing = vec!["chat-model".to_string(), "embed-model".to_string()];
        assert_eq!(
            ollama_pull_commands(DEFAULT_OLLAMA_OPENAI_BASE_URL, &missing),
            vec![
                "ollama pull chat-model".to_string(),
                "ollama pull embed-model".to_string()
            ]
        );
        assert!(ollama_pull_commands("https://models.example.test/v1/", &missing).is_empty());
        assert_eq!(
            ollama_pull_commands("http://localhost:11434", &missing).len(),
            2
        );
    }

    #[test]
    fn request_per_minute_limit_is_optional_and_bounded() {
        assert_eq!(parse_requests_per_minute("").unwrap(), 0);
        assert_eq!(parse_requests_per_minute("   ").unwrap(), 0);
        assert_eq!(parse_requests_per_minute("0").unwrap(), 0);
        assert_eq!(parse_requests_per_minute("10").unwrap(), 10);
        assert_eq!(parse_requests_per_minute(" 600 ").unwrap(), 600);
        assert_eq!(
            parse_requests_per_minute(&MAX_ENDPOINT_RPM.to_string()).unwrap(),
            MAX_ENDPOINT_RPM
        );
        let above_max = (u64::from(MAX_ENDPOINT_RPM) + 1).to_string();
        for invalid in ["-1", "1.5", "ten", "4294967296", above_max.as_str()] {
            assert!(
                parse_requests_per_minute(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
    }

    #[test]
    fn request_timeout_is_an_integer_within_the_provider_range() {
        assert_eq!(parse_request_timeout_secs("1").unwrap(), 1);
        assert_eq!(parse_request_timeout_secs(" 120 ").unwrap(), 120);
        assert_eq!(parse_request_timeout_secs("600").unwrap(), 600);
        for invalid in ["", "0", "601", "1.5", "-1", "ten", "18446744073709551616"] {
            assert!(
                parse_request_timeout_secs(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
    }
}
