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

pub(crate) fn endpoint_is_loopback(url: &Url) -> bool {
    match url.host_str().map(|host| host.trim_matches(['[', ']'])) {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}
