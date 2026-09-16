//! Host-performed internet search for the read-only AI agent.
//!
//! Web search is deliberately **not** a model-facing tool. The host runs it
//! only when the authorized books cannot ground an answer, so the agent keeps
//! exactly three read-only tools (`search_books`, `read_passages`,
//! `get_outline`) and the model can never trigger network egress on its own.
//!
//! The client is backend agnostic: a URL template plus an optional body
//! template and API key cover SearxNG, Brave and Tavily style endpoints, and
//! several common response shapes are recognised when extracting results.

use std::{net::IpAddr, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::future::BoxFuture;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Client, Method, Url};
use serde::Deserialize;

use crate::agent::{WebSearchBackend, WebSearchRequest, WebSearchResult};

pub const DEFAULT_WEB_SEARCH_TIMEOUT_SECS: u64 = 20;
pub const MIN_WEB_SEARCH_TIMEOUT_SECS: u64 = 1;
pub const MAX_WEB_SEARCH_TIMEOUT_SECS: u64 = 120;
pub const MAX_WEB_SEARCH_RESULTS: usize = 8;
pub const MAX_WEB_SEARCH_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_FIELD_BYTES: usize = 4096;
const MAX_QUERY_BYTES: usize = 2048;

/// Query placeholder recognised in [`WebSearchConfig::url_template`] and
/// [`WebSearchConfig::body_template`].
pub const QUERY_PLACEHOLDER: &str = "{query}";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSearchConfig {
    /// Opt-in. Disabled by default so no search request is ever issued unless
    /// the user explicitly enables and configures one.
    pub enabled: bool,
    /// URL template containing `{query}`, for example
    /// `http://127.0.0.1:8080/search?q={query}&format=json` (SearxNG).
    pub url_template: String,
    /// `GET` or `POST`.
    pub method: String,
    /// Optional body template containing `{query}`, used for POST endpoints
    /// such as Tavily (`{"query":"{query}"}`).
    pub body_template: Option<String>,
    /// Secret resolved from Windows Credential Manager; never persisted in
    /// SQLite and never logged.
    pub api_key: Option<String>,
    /// Header name used to send the key. `None` sends
    /// `Authorization: Bearer <key>`.
    pub api_key_header: Option<String>,
    /// User confirmation required before a non-loopback endpoint is used.
    pub remote_confirmed: bool,
    /// Separate opt-in required before a remote plain-HTTP endpoint is used.
    pub allow_insecure_remote_http: bool,
    pub timeout_secs: u64,
    pub max_results: usize,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url_template: String::new(),
            method: "GET".to_string(),
            body_template: None,
            api_key: None,
            api_key_header: None,
            remote_confirmed: false,
            allow_insecure_remote_http: false,
            timeout_secs: DEFAULT_WEB_SEARCH_TIMEOUT_SECS,
            max_results: MAX_WEB_SEARCH_RESULTS,
        }
    }
}

impl WebSearchConfig {
    /// Validates the configuration and resolves the endpoint host. This grants
    /// no permission by itself: the endpoint policy is enforced here so an
    /// unconfirmed or insecure remote endpoint can never be contacted.
    pub fn validated_endpoint(&self) -> Result<Url> {
        ensure!(self.enabled, "web search is disabled");
        let url = self.rendered_url("search")?;
        match url.scheme() {
            "http" | "https" => {}
            other => bail!("web search endpoint must use http or https, got {other}"),
        }
        let loopback = endpoint_is_loopback(&url);
        if !loopback && !self.remote_confirmed {
            bail!("using a remote web search endpoint has not been confirmed");
        }
        if !loopback && url.scheme() != "https" && !self.allow_insecure_remote_http {
            bail!(
                "remote web search endpoints must use HTTPS unless insecure HTTP is explicitly allowed"
            );
        }
        ensure!(
            (MIN_WEB_SEARCH_TIMEOUT_SECS..=MAX_WEB_SEARCH_TIMEOUT_SECS)
                .contains(&self.timeout_secs),
            "web search timeout must be between {MIN_WEB_SEARCH_TIMEOUT_SECS} and {MAX_WEB_SEARCH_TIMEOUT_SECS} seconds"
        );
        ensure!(
            (1..=MAX_WEB_SEARCH_RESULTS).contains(&self.max_results),
            "web search result count must be between 1 and {MAX_WEB_SEARCH_RESULTS}"
        );
        Ok(url)
    }

    /// Substitutes the query into the template. The query is percent-encoded
    /// so it cannot inject additional query parameters or path segments.
    fn rendered_url(&self, query: &str) -> Result<Url> {
        let template = self.url_template.trim();
        ensure!(
            !template.is_empty(),
            "web search endpoint URL template is empty"
        );
        let encoded = utf8_percent_encode(query, NON_ALPHANUMERIC).to_string();
        let rendered = template.replace(QUERY_PLACEHOLDER, &encoded);
        Url::parse(&rendered).context("invalid web search endpoint URL")
    }

    fn method(&self) -> Result<Method> {
        match self.method.trim().to_ascii_uppercase().as_str() {
            "GET" => Ok(Method::GET),
            "POST" => Ok(Method::POST),
            other => bail!("unsupported web search method {other}"),
        }
    }

    fn body(&self, query: &str) -> Result<Option<String>> {
        let Some(template) = self.body_template.as_deref() else {
            return Ok(None);
        };
        // JSON-escape instead of percent-encoding: the body is a JSON template.
        let escaped = serde_json::to_string(query).context("failed to encode search query")?;
        let inner = &escaped[1..escaped.len() - 1];
        Ok(Some(template.replace(QUERY_PLACEHOLDER, inner)))
    }
}

fn endpoint_is_loopback(url: &Url) -> bool {
    match url.host_str().map(|host| host.trim_matches(['[', ']'])) {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

/// Normalizes a web search endpoint for acknowledgement comparison: lowercases
/// scheme and host, drops the default port, query and fragment, and trims a
/// trailing slash. The same host/path authority is treated as one
/// acknowledgement target so changing it invalidates prior confirmations.
pub fn normalize_web_endpoint(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port_or_known_default().filter(|port| {
        !((url.scheme() == "https" && *port == 443) || (url.scheme() == "http" && *port == 80))
    });
    let mut normalized = format!("{}://{host}", url.scheme());
    if let Some(port) = port {
        normalized.push_str(&format!(":{port}"));
    }
    normalized.push_str(url.path().trim_end_matches('/'));
    Some(normalized)
}

#[derive(Clone)]
pub struct HttpWebSearch {
    config: WebSearchConfig,
    client: Client,
}

impl std::fmt::Debug for HttpWebSearch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpWebSearch")
            .field("url_template", &self.config.url_template)
            .field("method", &self.config.method)
            .field("has_api_key", &self.config.api_key.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpWebSearch {
    pub fn new(config: WebSearchConfig) -> Result<Self> {
        Self::new_with_proxy(config, crate::ai::DEFAULT_AI_USE_PROXY)
    }

    /// Mirrors [`crate::ai::OpenAiHttpProvider::new_with_proxy`]: the proxy
    /// preference is a system-wide setting, so the web-search client follows the
    /// same switch as the model clients instead of reading the environment
    /// variables on its own.
    pub fn new_with_proxy(config: WebSearchConfig, use_proxy: bool) -> Result<Self> {
        config.validated_endpoint()?;
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(config.timeout_secs));
        if !use_proxy {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .context("failed to build web search HTTP client")?;
        Ok(Self { config, client })
    }

    pub fn config(&self) -> &WebSearchConfig {
        &self.config
    }
}

impl WebSearchBackend for HttpWebSearch {
    fn web_search(
        &self,
        request: WebSearchRequest,
    ) -> BoxFuture<'_, anyhow::Result<Vec<WebSearchResult>>> {
        let config = self.config.clone();
        let client = self.client.clone();
        Box::pin(async move { run_search(&client, &config, request).await })
    }
}

async fn run_search(
    client: &Client,
    config: &WebSearchConfig,
    request: WebSearchRequest,
) -> Result<Vec<WebSearchResult>> {
    let query = request.query.trim();
    ensure!(!query.is_empty(), "web search query is empty");
    ensure!(
        query.len() <= MAX_QUERY_BYTES,
        "web search query exceeds {MAX_QUERY_BYTES} bytes"
    );
    config.validated_endpoint()?;

    let url = config.rendered_url(query)?;
    let method = config.method()?;
    let mut builder = client.request(method, url);
    if let Some(key) = config
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
    {
        builder = match config.api_key_header.as_deref() {
            Some(header) if !header.trim().is_empty() => builder.header(header.trim(), key),
            _ => builder.bearer_auth(key),
        };
    }
    if let Some(body) = config.body(query)? {
        builder = builder
            .header("content-type", "application/json")
            .body(body);
    }
    let response = builder
        .send()
        .await
        .context("web search request failed")?
        .error_for_status()
        .context("web search endpoint returned an error status")?;

    let bytes = bounded_body(response).await?;
    let parsed: SearchResponseShape =
        serde_json::from_slice(&bytes).context("web search response is not valid JSON")?;
    Ok(extract_results(parsed, config.max_results))
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>> {
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read web search response")?
    {
        ensure!(
            body.len().saturating_add(chunk.len()) <= MAX_WEB_SEARCH_RESPONSE_BYTES,
            "web search response exceeds {MAX_WEB_SEARCH_RESPONSE_BYTES} bytes"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Recognised response envelopes. Unknown shapes simply yield no results
/// instead of failing the whole answer.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SearchResponseShape {
    results: Option<Vec<RawResult>>,
    /// Brave nests results under `web`.
    web: Option<SearchWebShape>,
    organic: Option<Vec<RawResult>>,
    items: Option<Vec<RawResult>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SearchWebShape {
    results: Option<Vec<RawResult>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawResult {
    title: Option<String>,
    url: Option<String>,
    /// Tavily / SearxNG body text.
    content: Option<String>,
    /// Brave summary.
    description: Option<String>,
    snippet: Option<String>,
}

impl RawResult {
    fn snippet(&self) -> String {
        self.content
            .as_deref()
            .or(self.description.as_deref())
            .or(self.snippet.as_deref())
            .unwrap_or_default()
            .to_string()
    }
}

fn extract_results(shape: SearchResponseShape, max_results: usize) -> Vec<WebSearchResult> {
    let raw = shape
        .results
        .or(shape.web.and_then(|web| web.results))
        .or(shape.organic)
        .or(shape.items)
        .unwrap_or_default();

    let mut results = Vec::new();
    for entry in raw {
        if results.len() >= max_results {
            break;
        }
        // Only absolute http(s) URLs become model-visible sources, so a
        // malicious or misconfigured engine cannot smuggle file:, javascript:
        // or other schemes into a citation the UI might render or open.
        let Some(url) = entry.url.as_deref().map(str::trim).filter(is_http_url) else {
            continue;
        };
        let title = truncate(entry.title.as_deref().unwrap_or_default(), MAX_FIELD_BYTES);
        let snippet = truncate(&entry.snippet(), MAX_FIELD_BYTES);
        results.push(WebSearchResult {
            title: if title.is_empty() {
                (*url).to_string()
            } else {
                title
            },
            url: (*url).to_string(),
            snippet,
        });
    }
    results
}

fn is_http_url(url: &&str) -> bool {
    match Url::parse(url) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https"),
        Err(_) => false,
    }
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str) -> WebSearchConfig {
        WebSearchConfig {
            enabled: true,
            url_template: url.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_config_is_rejected() {
        let config = WebSearchConfig::default();
        assert!(config.validated_endpoint().is_err());
    }

    #[test]
    fn loopback_endpoint_needs_no_confirmation() {
        let config = config("http://127.0.0.1:8080/search?q={query}&format=json");
        assert!(config.validated_endpoint().is_ok());
    }

    #[test]
    fn remote_endpoint_requires_confirmation_and_https() {
        let unconfirmed = config("https://api.example.test/search?q={query}");
        assert!(unconfirmed.validated_endpoint().is_err());

        let confirmed = WebSearchConfig {
            remote_confirmed: true,
            ..config("https://api.example.test/search?q={query}")
        };
        assert!(confirmed.validated_endpoint().is_ok());

        let insecure = WebSearchConfig {
            remote_confirmed: true,
            ..config("http://api.example.test/search?q={query}")
        };
        assert!(insecure.validated_endpoint().is_err());

        let explicitly_insecure = WebSearchConfig {
            remote_confirmed: true,
            allow_insecure_remote_http: true,
            ..config("http://api.example.test/search?q={query}")
        };
        assert!(explicitly_insecure.validated_endpoint().is_ok());
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        let config = config("file:///etc/passwd?q={query}");
        assert!(config.validated_endpoint().is_err());
    }

    #[test]
    fn normalized_endpoint_drops_query_fragment_default_port_and_trailing_slash() {
        assert_eq!(
            normalize_web_endpoint("HTTPS://Api.Example.test:443/search/?q=1#frag").as_deref(),
            Some("https://api.example.test/search")
        );
        assert_eq!(
            normalize_web_endpoint("http://api.example.test:8080/search/").as_deref(),
            Some("http://api.example.test:8080/search")
        );
        // A host or path change must not keep an older acknowledgement valid.
        assert_ne!(
            normalize_web_endpoint("https://api.example.test/search"),
            normalize_web_endpoint("https://api.example.test/other")
        );
        assert!(normalize_web_endpoint("file:///etc/passwd").is_none());
        assert!(normalize_web_endpoint("not a url").is_none());
    }

    #[test]
    fn query_is_percent_encoded_so_it_cannot_inject_parameters() {
        let config = config("http://127.0.0.1:8080/search?q={query}&format=json");
        let url = config.rendered_url("a b&format=xml").unwrap();
        assert_eq!(url.query(), Some("q=a%20b%26format%3Dxml&format=json"));
    }

    #[test]
    fn body_template_json_escapes_the_query() {
        let config = WebSearchConfig {
            body_template: Some(r#"{"query":"{query}"}"#.to_string()),
            ..config("https://api.example.test/search")
        };
        let body = config.body(r#"say "hi""#).unwrap().unwrap();
        assert_eq!(body, r#"{"query":"say \"hi\""}"#);
    }

    fn raw(title: &str, url: &str) -> RawResult {
        RawResult {
            title: Some(title.to_string()),
            url: Some(url.to_string()),
            content: Some("body".to_string()),
            description: None,
            snippet: None,
        }
    }

    #[test]
    fn searxng_and_tavily_shape_is_extracted() {
        let shape = SearchResponseShape {
            results: Some(vec![raw("Title", "https://example.test/a")]),
            ..Default::default()
        };
        let results = extract_results(shape, 8);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.test/a");
        assert_eq!(results[0].snippet, "body");
    }

    #[test]
    fn brave_shape_is_extracted() {
        let shape = SearchResponseShape {
            web: Some(SearchWebShape {
                results: Some(vec![RawResult {
                    title: Some("Brave".to_string()),
                    url: Some("https://example.test/b".to_string()),
                    content: None,
                    description: Some("summary".to_string()),
                    snippet: None,
                }]),
            }),
            ..Default::default()
        };
        let results = extract_results(shape, 8);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Brave");
        assert_eq!(results[0].snippet, "summary");
    }

    #[test]
    fn non_http_and_javascript_urls_are_dropped() {
        let shape = SearchResponseShape {
            results: Some(vec![
                raw("ok", "https://example.test/a"),
                raw("js", "javascript:alert(1)"),
                raw("file", "file:///etc/passwd"),
                raw("data", "data:text/html,<script>"),
            ]),
            ..Default::default()
        };
        let results = extract_results(shape, 8);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.test/a");
    }

    #[test]
    fn result_count_is_bounded() {
        let entries = (0..50)
            .map(|index| raw("t", &format!("https://example.test/{index}")))
            .collect::<Vec<_>>();
        let shape = SearchResponseShape {
            results: Some(entries),
            ..Default::default()
        };
        assert_eq!(extract_results(shape, 3).len(), 3);
    }

    #[test]
    fn unknown_shape_yields_no_results() {
        let shape = SearchResponseShape::default();
        assert!(extract_results(shape, 8).is_empty());
    }
}
