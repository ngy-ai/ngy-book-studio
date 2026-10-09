//! Endpoint normalization and loopback detection.

use super::*;

/// Validates and canonicalizes the endpoint identity without granting either
/// remote-content or insecure-transport permission. Persisted confirmations
/// bind to this exact value so changing endpoint A to endpoint B cannot reuse
/// a checkbox that was accepted for A.
pub fn normalize_provider_base_url(base_url: &str) -> Result<Url> {
    let mut url = Url::parse(base_url).context("AI endpoint is not a valid URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("AI endpoint must use http or https");
    }
    if url.host_str().is_none() {
        bail!("AI endpoint must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("AI endpoint credentials must not be embedded in the URL");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("AI endpoint must not contain a query or fragment");
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// 诊断用的端点来源标签：`scheme://host[:port]`。
///
/// 只到**来源**，不含路径 —— 与 `OpenAiHttpProvider::chat_stream` 的 `ai_http` span
/// 同一条规则：路径可能带租户名，查询串与 userinfo 则已在
/// [`normalize_provider_base_url`] 里被禁止。因此它既够说明「这一发发给了谁」，又不把
/// 端点路径写进日志。
pub(crate) fn endpoint_origin_label(url: &Url) -> String {
    url.origin().ascii_serialization()
}

pub(crate) fn endpoint_is_loopback(url: &Url) -> bool {
    match url.host_str().map(|host| host.trim_matches(['[', ']'])) {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}
