//! Assembling the AI services from the stored settings.

use super::*;

pub(crate) fn build_ai_services(
    db_path: &Path,
    settings: ProviderSettings,
    api_keys: &BTreeMap<String, Option<String>>,
    endpoint_limits: Arc<EndpointRateLimits>,
) -> Result<AiServices> {
    settings.validate()?;
    let build_role = |role, class| -> Result<Arc<dyn OpenAiCompatibleProvider>> {
        let endpoint = settings.endpoint_for(role)?;
        let api_key = api_keys.get(&endpoint.id).cloned().flatten();
        let budget = EndpointBudget::new(
            endpoint_limits.slot(&endpoint.base_url, endpoint.requests_per_minute)?,
            class,
        );
        Ok(Arc::new(
            OpenAiHttpProvider::new_with_proxy(
                endpoint.provider_config(api_key),
                settings.use_proxy,
            )?
            .with_endpoint_budget(budget),
        ))
    };
    // 只有问答这一路是用户正等着看的请求，其余三路都是后台任务：额度共用，排队时
    // 后台任务让问答与模型检测先走（见 `RequestClass`）。
    let provider = build_role(ModelRole::Chat, RequestClass::Interactive)?;
    let translation_provider = build_role(ModelRole::Translation, RequestClass::Background)?;
    let embedding_provider = build_role(ModelRole::Embedding, RequestClass::Background)?;
    let vision_provider = build_role(ModelRole::Vision, RequestClass::Background)?;
    // 检索用的查询向量是问答链路的前半段（侧栏提问与图书库语义搜索都在等它），所以它与
    // 后台索引**共用同一份额度、但算交互请求**：额度是一份，排队位置是两类。后台索引走的
    // 是上面那个 `embedding_provider`，不受这里影响。
    let search_provider = build_role(ModelRole::Embedding, RequestClass::Interactive)?;
    let search = Arc::new(SearchService::new_with_execution_identity(
        db_path,
        search_provider,
        &settings.embedding_model,
        settings.embedding_dimensions,
        embedding_execution_identity(&settings)?,
    )?);
    Ok(AiServices {
        settings,
        provider,
        translation_provider,
        embedding_provider,
        vision_provider,
        search,
    })
}

/// 把设置里每条 Endpoint 的请求上限同步进额度表。
///
/// [`build_ai_services`] 只覆盖被模型绑定的那条 Endpoint；这里补上列表里其余的那些，
/// 免得「改了上限、保存后没生效」只发生在没被任何模型选中的地址上。只改上限，不等：
/// 已经记下的请求时刻与正在排队的请求都不受影响。
pub(crate) fn publish_endpoint_limits(
    endpoint_limits: &EndpointRateLimits,
    settings: &ProviderSettings,
) -> Result<()> {
    for endpoint in settings.endpoints() {
        endpoint_limits.slot(&endpoint.base_url, endpoint.requests_per_minute)?;
    }
    Ok(())
}

pub(crate) fn load_endpoint_api_keys(
    settings: &ProviderSettings,
    credentials: &dyn CredentialStore,
) -> Result<BTreeMap<String, Option<String>>> {
    settings
        .endpoints()
        .into_iter()
        .map(|endpoint| {
            let target = normalize_provider_base_url(&endpoint.base_url)?;
            Ok((endpoint.id, credentials.api_key(target.as_str())?))
        })
        .collect()
}

/// Attempt every entry even when one credential operation fails, so rollback
/// does not abandon independent keys after a single failure.
pub(crate) fn restore_endpoint_keys(
    credentials: &dyn CredentialStore,
    values: &BTreeMap<String, Option<String>>,
) -> Result<()> {
    let mut failure = None;
    for (target, value) in values {
        let result = match value {
            Some(value) => credentials.set_api_key(target, value),
            None => credentials.delete_api_key(target),
        };
        if let Err(error) = result {
            failure = Some(match failure {
                Some(previous) => {
                    error.context(format!("another credential operation failed: {previous:#}"))
                }
                None => error,
            });
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
