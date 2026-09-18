//! The backends the agent reads through.

use super::*;

pub trait SearchBackend: Send + Sync {
    fn search(&self, request: SearchRequest) -> BoxFuture<'_, AnyResult<Vec<PassageRecord>>>;
}

/// One internet result served to the model as a citable source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchResult {
    pub title: String,
    /// Absolute http(s) URL. Non-http(s) schemes are rejected by the client.
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSearchRequest {
    pub query: String,
    pub limit: usize,
}

/// Internet retrieval performed by the host, never by the model.
///
/// Web search is deliberately *not* exposed as a tool: the host decides when
/// the authorized books cannot answer and runs the fallback itself, so the
/// read-only agent keeps exactly three model-facing tools.
pub trait WebSearchBackend: Send + Sync {
    fn web_search(
        &self,
        request: WebSearchRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<WebSearchResult>>>;
}

impl<T> WebSearchBackend for Arc<T>
where
    T: WebSearchBackend + ?Sized,
{
    fn web_search(
        &self,
        request: WebSearchRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<WebSearchResult>>> {
        (**self).web_search(request)
    }
}

impl<T> SearchBackend for Arc<T>
where
    T: SearchBackend + ?Sized,
{
    fn search(&self, request: SearchRequest) -> BoxFuture<'_, AnyResult<Vec<PassageRecord>>> {
        (**self).search(request)
    }
}

pub trait BookBackend: Send + Sync {
    fn read_passages(
        &self,
        request: ReadPassagesRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<PassageRecord>>>;

    fn get_outline(
        &self,
        request: OutlineRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<BookOutlineRecord>>>;
}

impl<T> BookBackend for Arc<T>
where
    T: BookBackend + ?Sized,
{
    fn read_passages(
        &self,
        request: ReadPassagesRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<PassageRecord>>> {
        (**self).read_passages(request)
    }

    fn get_outline(
        &self,
        request: OutlineRequest,
    ) -> BoxFuture<'_, AnyResult<Vec<BookOutlineRecord>>> {
        (**self).get_outline(request)
    }
}
