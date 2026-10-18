//! One independently authorized connection and the role bindings that pick it.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRole {
    Chat,
    /// 整本翻译专用的模型与 Endpoint：与问答模型分开配置，翻译执行身份也只由它派生，
    /// 因此换问答模型不会让已翻完的书重翻。
    Translation,
    Embedding,
    Vision,
}

/// One independently authorized connection; credentials are stored by its
/// canonical URL in the operating-system credential store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointSettings {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub remote_content_confirmed: bool,
    pub allow_insecure_remote_http: bool,
    pub confirmed_remote_endpoint: String,
    pub request_timeout_secs: u64,
    /// 这台 Endpoint 的每分钟请求数上限（RPM）。`0`（含缺键的老行）是「自动」：本机端点
    /// 不限、远程端点按 [`crate::ai::DEFAULT_ENDPOINT_RPM`] 节流，由 [`Self::resolved_requests_per_minute`]
    /// 按 `base_url` 解析 —— 开箱直撞服务商的计量，代价是整台端点一起退避，而「慢一点」
    /// 用户自己就能调大。正数就是固定上限。
    ///
    /// 服务商按端点计量，所以额度按 `base_url` 共享：同一台服务器上的问答、整本翻译、
    /// 向量索引、视觉理解与模型检测走的是同一份配额。限速只影响请求什么时候发出，
    /// **不属于**任何执行身份，改它不会让已翻好的译文或已建好的索引失效。
    #[serde(default)]
    pub requests_per_minute: u32,
    /// 这台 Endpoint 的「每请求文本块数」。`0`（含缺键的老行）是「自动」：本机端点一次
    /// 一块，远程端点按 [`crate::ai::translation::MAX_TRANSLATION_BATCH_BLOCKS`] 打包，
    /// 由 [`Self::resolved_blocks_per_request`] 按 `base_url` 解析；`1` 是显式关闭
    /// （一块一请求），`> 1` 是固定上限。
    ///
    /// 打包只改变**一发请求带走几块**：译文、缓存键、落库形状与游标仍按块，所以它不属于
    /// 任何执行身份，改它不会让已翻好的译文失效。整本翻译的块窗口因此最多同时打开
    /// 「任务并发 × 每请求块数」个块。
    #[serde(default)]
    pub blocks_per_request: u32,
}

impl EndpointSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 128
                && self
                    .id
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_')),
            "Endpoint 标识无效"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.trim() == self.name
                && self.name.chars().count() <= 80
                && !self.name.chars().any(char::is_control),
            "Endpoint 名称须为 1 至 80 个字符，且不能包含首尾空白或控制字符"
        );
        self.provider_config(None)
            .validated_base_url()
            .with_context(|| format!("Endpoint「{}」配置无效", self.name))?;
        ensure!(
            self.requests_per_minute <= MAX_ENDPOINT_RPM,
            "Endpoint「{}」的每分钟请求数上限不能超过 {MAX_ENDPOINT_RPM}",
            self.name
        );
        ensure!(
            self.blocks_per_request <= crate::ai::translation::MAX_TRANSLATION_BATCH_BLOCKS_SETTING,
            "Endpoint「{}」的每请求文本块数不能超过 {}",
            self.name,
            crate::ai::translation::MAX_TRANSLATION_BATCH_BLOCKS_SETTING
        );
        Ok(())
    }

    pub fn provider_config(&self, api_key: Option<String>) -> ProviderConfig {
        let confirmation_matches = normalize_provider_base_url(&self.base_url)
            .ok()
            .is_some_and(|url| self.confirmed_remote_endpoint == url.as_str());
        ProviderConfig {
            base_url: self.base_url.clone(),
            api_key,
            remote_content_confirmed: self.remote_content_confirmed && confirmation_matches,
            allow_insecure_remote_http: self.allow_insecure_remote_http && confirmation_matches,
            request_timeout_secs: self.request_timeout_secs,
        }
    }

    /// 真正接进额度闸门的每分钟请求数上限：`0` 是「自动」，按地址解析成具体的数字。
    ///
    /// 读这个字段的地方（装配 provider、保存后发布额度、模型检测）都必须走这里，
    /// 直接读 [`Self::requests_per_minute`] 拿到的是没解析过的「自动」。
    pub fn resolved_requests_per_minute(&self) -> u32 {
        resolve_requests_per_minute(self.requests_per_minute, &self.base_url)
    }

    /// 这台 Endpoint 真正接进整本翻译的每请求块数：`0` 是「自动」，按地址解析成具体的
    /// 数字（见 [`Self::blocks_per_request`]）。
    ///
    /// 装配翻译 provider 的地方都必须走这里，直接读 [`Self::blocks_per_request`] 拿到的
    /// 是没解析过的「自动」。
    pub fn resolved_blocks_per_request(&self) -> usize {
        resolve_blocks_per_request(self.blocks_per_request, &self.base_url)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointRoutingSettings {
    pub default_endpoint_name: String,
    pub additional_endpoints: Vec<EndpointSettings>,
    pub chat_endpoint_id: String,
    /// 翻译模型自己的 Endpoint。老版本的绑定行里没有这个键，缺失时
    /// `load_provider_settings` 会把它填成当时的问答 Endpoint。
    #[serde(default = "default_endpoint_id")]
    pub translation_endpoint_id: String,
    pub embedding_endpoint_id: String,
    pub vision_endpoint_id: String,
}

impl Default for EndpointRoutingSettings {
    fn default() -> Self {
        Self {
            default_endpoint_name: "默认 Endpoint".into(),
            additional_endpoints: Vec::new(),
            chat_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
            translation_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
            embedding_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
            vision_endpoint_id: DEFAULT_ENDPOINT_ID.into(),
        }
    }
}

/// `0` 是「自动」：交给地址决定（本机不限、远程 [`crate::ai::DEFAULT_ENDPOINT_RPM`]），
/// 正数是固定上限。
pub(crate) fn resolve_requests_per_minute(configured: u32, base_url: &str) -> u32 {
    if configured > 0 {
        configured
    } else {
        default_endpoint_requests_per_minute(base_url)
    }
}

/// `0` 是「自动」：交给地址决定（本机端点一次一块、远程端点按
/// [`crate::ai::translation::MAX_TRANSLATION_BATCH_BLOCKS`] 打包），正数是固定上限。
///
/// 与「每分钟请求数上限」同一套回环判定，只是默认值方向相反：限速默认给本机**放开**，
/// 打包默认给本机**关掉**（弱模型更容易把多个块合并成一条，批会让一处坏形状牵连更多块）。
/// 地址解析不出来时按远程处理：打包本身不花代价，少发几发请求是收益。
pub(crate) fn resolve_blocks_per_request(configured: u32, base_url: &str) -> usize {
    match normalize_provider_base_url(base_url) {
        Ok(url) => crate::ai::translation::batch_block_limit(
            configured,
            crate::ai::endpoint_is_loopback(&url),
        ),
        Err(_) => crate::ai::translation::batch_block_limit(configured, false),
    }
}
