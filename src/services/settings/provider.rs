//! [`ProviderSettings`]: the combined view the UI and the runtime read.
//! Secrets intentionally cannot be represented by this type; API keys live
//! behind [`CredentialStore`].

use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    pub base_url: String,
    /// Separate persistence keeps the established provider JSON unchanged.
    #[serde(skip)]
    pub endpoint_routing: EndpointRoutingSettings,
    pub chat_model: String,
    /// 整本翻译专用的模型。与问答模型分开：问答模型变化不再让翻译任务重跑。
    /// 老版本的主 JSON 没有这个键，缺失时按当时的问答模型补齐（见
    /// `load_provider_settings`），所以升级不会改变翻译执行身份。
    #[serde(default = "default_translation_model")]
    pub translation_model: String,
    /// Stored under its own settings key and combined here for the UI/runtime.
    #[serde(skip)]
    pub chat_generation: ChatGenerationSettings,
    /// 翻译请求自己的生成参数。与问答参数分开存储；它不进翻译执行身份，因此改这里
    /// 不会让已翻好的译文失效、也不会让翻译任务重跑。
    #[serde(skip)]
    pub translation_generation: TranslationGenerationSettings,
    /// Stored under its own settings key so the established provider JSON
    /// contract remains unchanged.
    #[serde(skip, default = "default_auto_run_background_jobs")]
    pub auto_run_background_jobs: bool,
    /// How many background model jobs may run at the same time. Stored in the
    /// background-job settings row, like the auto-run preference above.
    #[serde(skip, default = "default_background_job_concurrency")]
    pub background_job_concurrency: usize,
    /// Pause in milliseconds one worker waits after finishing a job before it
    /// claims the next one. Stored in the background-job settings row.
    #[serde(skip, default = "default_background_job_interval_ms")]
    pub background_job_interval_ms: u64,
    /// Reading preference of the PDF reader: when enabled the continuous page
    /// column drops the gap between pages. Stored under its own settings key,
    /// like the background-job preference above.
    #[serde(skip, default = "default_pdf_compact_reading")]
    pub pdf_compact_reading: bool,
    /// 界面皮肤（浅色 / 深色）。存进自己的 settings key，与上面的偏好同理；
    /// 它是应用级设置，切换时所有窗口与 WebView 一起跟随。
    #[serde(skip, default)]
    pub appearance: Appearance,
    /// Target language for the reading-time book translation. `None` keeps the
    /// original text. Stored under its own settings key so the established
    /// provider JSON contract remains unchanged, like the preferences above.
    #[serde(skip, default = "default_translation_language")]
    pub default_language: Option<String>,
    /// Reading-time translation display: bilingual by default off, only the
    /// translated text by default on. Stored under the translation settings key
    /// like the target language above.
    #[serde(skip, default = "default_translation_display_mode")]
    pub translation_display_mode: TranslationDisplayMode,
    /// Whether AI and web-search requests honour the system proxy. Stored under
    /// its own settings key, like the preferences above.
    #[serde(skip, default = "default_ai_use_proxy")]
    pub use_proxy: bool,
    /// 朗读用的系统语音 ID；空串表示用系统默认语音。存进自己的 settings key
    /// （`speech.preferences.v1`），与上面的偏好同理：应用级设置，所有阅读窗口共用。
    #[serde(skip, default)]
    pub speech_voice_id: String,
    /// 朗读语速倍率。合法范围见 `ngy_book_studio::speech::SpeechRequest`；读取时按
    /// 该范围钳制，手改坏的行不会让阅读窗口打不开。
    #[serde(skip, default = "default_speech_rate")]
    pub speech_rate: f32,
    /// 选中文本点「AI 解释」时自动填入的问题。存进自己的 settings key
    /// （`ai.question.preferences.v1`），与上面的偏好同理：应用级设置，所有窗口共用。
    #[serde(skip, default = "default_selection_explanation_question")]
    pub selection_explanation_question: String,
    pub embedding_model: String,
    /// Dimension override sent to the embedding provider. Changing this value
    /// invalidates all existing vector indices because stored vectors with the
    /// old dimension are incompatible with the new request/response size.
    #[serde(default = "default_embedding_dimensions")]
    pub embedding_dimensions: usize,
    pub vision_model: String,
    pub remote_content_confirmed: bool,
    pub allow_insecure_remote_http: bool,
    /// Canonical endpoint for which the content and transport acknowledgements
    /// were made. Changing the endpoint invalidates both booleans.
    pub confirmed_remote_endpoint: String,
    pub request_timeout_secs: u64,
    /// 默认 Endpoint 的「每分钟请求数上限」（见 [`EndpointSettings::requests_per_minute`]）。
    /// 与那里一样：`0`（含缺键的老行）是「自动」，正数是固定上限。
    #[serde(default)]
    pub requests_per_minute: u32,
    /// 默认 Endpoint 的「每请求文本块数」（见 [`EndpointSettings::blocks_per_request`]）。
    /// 与那里一样：`0`（含缺键的老行）是「自动」，正数是固定上限。
    #[serde(default)]
    pub blocks_per_request: u32,
    // --- Host-performed web search (off by default) ---
    /// Opt-in internet fallback used only when the authorized books cannot
    /// ground an answer. Disabled by default, so no request is ever issued
    /// unless the user enables and configures one.
    #[serde(default)]
    pub web_search_enabled: bool,
    #[serde(default)]
    pub web_search_url_template: String,
    #[serde(default = "default_web_search_method")]
    pub web_search_method: String,
    /// Optional JSON body template containing `{query}` for POST endpoints
    /// such as Tavily.
    #[serde(default)]
    pub web_search_body_template: Option<String>,
    /// Header name for the web-search key. `None` sends `Authorization: Bearer <key>`.
    #[serde(default)]
    pub web_search_key_header: Option<String>,
    #[serde(default)]
    pub web_search_remote_confirmed: bool,
    #[serde(default)]
    pub web_search_allow_insecure_http: bool,
    /// Canonical web endpoint for which the confirmations above were made.
    #[serde(default)]
    pub web_search_confirmed_remote_endpoint: String,
    #[serde(default = "default_web_search_timeout_secs")]
    pub web_search_timeout_secs: u64,
    #[serde(default = "default_web_search_max_results")]
    pub web_search_max_results: usize,
}

fn default_web_search_method() -> String {
    "GET".to_string()
}

fn default_web_search_timeout_secs() -> u64 {
    crate::ai::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS
}

fn default_web_search_max_results() -> usize {
    crate::ai::web_search::MAX_WEB_SEARCH_RESULTS
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_OPENAI_BASE_URL.to_string(),
            endpoint_routing: EndpointRoutingSettings::default(),
            chat_model: DEFAULT_CHAT_MODEL.to_string(),
            translation_model: default_translation_model(),
            chat_generation: ChatGenerationSettings::default(),
            translation_generation: TranslationGenerationSettings::default(),
            auto_run_background_jobs: default_auto_run_background_jobs(),
            background_job_concurrency: default_background_job_concurrency(),
            background_job_interval_ms: default_background_job_interval_ms(),
            pdf_compact_reading: default_pdf_compact_reading(),
            appearance: Appearance::default(),
            default_language: default_translation_language(),
            translation_display_mode: default_translation_display_mode(),
            use_proxy: default_ai_use_proxy(),
            embedding_model: DEFAULT_EMBEDDING_MODEL.to_string(),
            embedding_dimensions: DEFAULT_EMBEDDING_DIMENSIONS,
            vision_model: DEFAULT_VISION_MODEL.to_string(),
            remote_content_confirmed: false,
            allow_insecure_remote_http: false,
            confirmed_remote_endpoint: String::new(),
            request_timeout_secs: DEFAULT_AI_REQUEST_TIMEOUT_SECS,
            requests_per_minute: 0,
            blocks_per_request: 0,
            web_search_enabled: false,
            web_search_url_template: String::new(),
            web_search_method: default_web_search_method(),
            web_search_body_template: None,
            web_search_key_header: None,
            web_search_remote_confirmed: false,
            web_search_allow_insecure_http: false,
            web_search_confirmed_remote_endpoint: String::new(),
            web_search_timeout_secs: crate::ai::web_search::DEFAULT_WEB_SEARCH_TIMEOUT_SECS,
            web_search_max_results: crate::ai::web_search::MAX_WEB_SEARCH_RESULTS,
            // 空 ID 表示系统默认语音；默认语速 1.0 倍。
            speech_voice_id: String::new(),
            speech_rate: default_speech_rate(),
            selection_explanation_question: default_selection_explanation_question(),
        }
    }
}

impl ProviderSettings {
    pub fn validate(&self) -> Result<()> {
        validate_model_name("chat", &self.chat_model)?;
        validate_model_name("translation", &self.translation_model)?;
        validate_model_name("embedding", &self.embedding_model)?;
        validate_model_name("vision", &self.vision_model)?;
        ensure!(
            self.embedding_dimensions > 0
                && self.embedding_dimensions <= crate::ai::MAX_EMBEDDING_DIMENSIONS,
            "embedding 维度必须在 1 到 {} 之间",
            crate::ai::MAX_EMBEDDING_DIMENSIONS
        );
        self.chat_generation.validate()?;
        self.translation_generation.validate()?;
        if let Some(language) = &self.default_language {
            ensure!(
                TRANSLATION_LANGUAGES
                    .iter()
                    .any(|(tag, _)| *tag == language.as_str()),
                "默认显示语言无效"
            );
        }
        ensure!(
            (MIN_BACKGROUND_JOB_CONCURRENCY..=MAX_BACKGROUND_JOB_CONCURRENCY)
                .contains(&self.background_job_concurrency),
            "后台任务并发必须是 {MIN_BACKGROUND_JOB_CONCURRENCY} 到 {MAX_BACKGROUND_JOB_CONCURRENCY} 之间的整数"
        );
        ensure!(
            self.background_job_interval_ms <= MAX_BACKGROUND_JOB_INTERVAL_MS,
            "后台任务间隔必须是 0 到 {MAX_BACKGROUND_JOB_INTERVAL_MS} 之间的整数毫秒"
        );
        // 解释问题随每次解释请求发送：空问题会让模型无从回答，超长会挤占侧栏的
        // 问题预算，控制字符则会把请求体弄脏。读取时不做截断，只在保存时拒绝。
        let question = self.selection_explanation_question.trim();
        ensure!(!question.is_empty(), "AI 解释提示词不能为空");
        ensure!(
            question.len() <= MAX_SELECTION_EXPLANATION_QUESTION_BYTES,
            "AI 解释提示词不能超过 {MAX_SELECTION_EXPLANATION_QUESTION_BYTES} 字节"
        );
        ensure!(
            !question
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t')),
            "AI 解释提示词不能包含控制字符"
        );
        let mut ids = BTreeSet::new();
        let mut urls = BTreeSet::new();
        for endpoint in self.endpoints() {
            endpoint.validate()?;
            ensure!(ids.insert(endpoint.id.clone()), "Endpoint 标识重复");
            ensure!(
                urls.insert(normalize_provider_base_url(&endpoint.base_url)?.to_string()),
                "Endpoint 地址重复：请共用已有 Endpoint，并为各模型选择它"
            );
        }
        for role in [
            ModelRole::Chat,
            ModelRole::Translation,
            ModelRole::Embedding,
            ModelRole::Vision,
        ] {
            self.endpoint_for(role)?;
        }
        // Validate the web endpoint eagerly so a malformed configuration is
        // caught at save time instead of silently disabling the fallback.
        self.web_search_config(None)?;
        Ok(())
    }

    pub fn endpoints(&self) -> Vec<EndpointSettings> {
        let mut endpoints =
            Vec::with_capacity(self.endpoint_routing.additional_endpoints.len() + 1);
        endpoints.push(EndpointSettings {
            id: DEFAULT_ENDPOINT_ID.into(),
            name: self.endpoint_routing.default_endpoint_name.clone(),
            base_url: self.base_url.clone(),
            remote_content_confirmed: self.remote_content_confirmed,
            allow_insecure_remote_http: self.allow_insecure_remote_http,
            confirmed_remote_endpoint: self.confirmed_remote_endpoint.clone(),
            request_timeout_secs: self.request_timeout_secs,
            requests_per_minute: self.requests_per_minute,
            blocks_per_request: self.blocks_per_request,
        });
        endpoints.extend(self.endpoint_routing.additional_endpoints.iter().cloned());
        endpoints
    }

    pub fn endpoint_by_id(&self, id: &str) -> Result<EndpointSettings> {
        self.endpoints()
            .into_iter()
            .find(|endpoint| endpoint.id == id)
            .context("模型绑定的 Endpoint 不存在，请重新选择")
    }

    pub fn endpoint_for(&self, role: ModelRole) -> Result<EndpointSettings> {
        let id = match role {
            ModelRole::Chat => &self.endpoint_routing.chat_endpoint_id,
            ModelRole::Translation => &self.endpoint_routing.translation_endpoint_id,
            ModelRole::Embedding => &self.endpoint_routing.embedding_endpoint_id,
            ModelRole::Vision => &self.endpoint_routing.vision_endpoint_id,
        };
        self.endpoint_by_id(id)
    }

    /// 默认 Endpoint 真正接进额度闸门的上限（`0`＝自动，见
    /// [`EndpointSettings::resolved_requests_per_minute`]）。
    pub fn resolved_requests_per_minute(&self) -> u32 {
        resolve_requests_per_minute(self.requests_per_minute, &self.base_url)
    }

    /// 默认 Endpoint 真正接进整本翻译的每请求块数（`0`＝自动，见
    /// [`EndpointSettings::resolved_blocks_per_request`]）。
    pub fn resolved_blocks_per_request(&self) -> usize {
        resolve_blocks_per_request(self.blocks_per_request, &self.base_url)
    }

    /// **翻译**那条 Endpoint 真正接进整本翻译的每请求块数。
    ///
    /// 装配 `TranslationServices` 的调用点用这个：打包是端点级的，跟着翻译绑定的那台
    /// Endpoint 走，而不是默认 Endpoint。
    pub fn resolved_translation_blocks_per_request(&self) -> Result<usize> {
        Ok(self
            .endpoint_for(ModelRole::Translation)?
            .resolved_blocks_per_request())
    }

    /// Builds the validated web-search configuration, or `None` when the
    /// fallback is disabled. The API key is supplied by the caller (it lives in
    /// the credential store, never in persisted JSON).
    pub fn web_search_config(&self, api_key: Option<String>) -> Result<Option<WebSearchConfig>> {
        if !self.web_search_enabled {
            return Ok(None);
        }
        let confirmation_matches = normalize_web_endpoint(&self.web_search_url_template)
            .is_some_and(|url| self.web_search_confirmed_remote_endpoint == url);
        let config = WebSearchConfig {
            enabled: true,
            url_template: self.web_search_url_template.clone(),
            method: self.web_search_method.clone(),
            body_template: self.web_search_body_template.clone(),
            api_key,
            api_key_header: self.web_search_key_header.clone(),
            remote_confirmed: self.web_search_remote_confirmed && confirmation_matches,
            allow_insecure_remote_http: self.web_search_allow_insecure_http && confirmation_matches,
            timeout_secs: self.web_search_timeout_secs,
            max_results: self.web_search_max_results,
        };
        config
            .validated_endpoint()
            .context("联网搜索端点配置无效")?;
        Ok(Some(config))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiKeyUpdate {
    Keep,
    Set(String),
    Delete,
}
