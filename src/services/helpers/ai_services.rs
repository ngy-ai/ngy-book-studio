//! Assembling the AI services from the stored settings.

use super::*;

pub(crate) fn build_ai_services(
    db_path: &Path,
    settings: ProviderSettings,
    api_keys: &BTreeMap<String, Option<String>>,
) -> Result<AiServices> {
    settings.validate()?;
    let build_role = |role| -> Result<Arc<dyn OpenAiCompatibleProvider>> {
        let endpoint = settings.endpoint_for(role)?;
        let api_key = api_keys.get(&endpoint.id).cloned().flatten();
        Ok(Arc::new(OpenAiHttpProvider::new_with_proxy(
            endpoint.provider_config(api_key),
            settings.use_proxy,
        )?))
    };
    let provider = build_role(ModelRole::Chat)?;
    let embedding_provider = build_role(ModelRole::Embedding)?;
    let vision_provider = build_role(ModelRole::Vision)?;
    let search = Arc::new(SearchService::new_with_execution_identity(
        db_path,
        Arc::clone(&embedding_provider),
        &settings.embedding_model,
        settings.embedding_dimensions,
        embedding_execution_identity(&settings)?,
    )?);
    Ok(AiServices {
        settings,
        provider,
        embedding_provider,
        vision_provider,
        search,
    })
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
