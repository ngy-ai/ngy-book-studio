//! Parsing and bounding the tool arguments the model sends.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchBooksArguments {
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) book_ids: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) mode: Option<SearchMode>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadPassagesArguments {
    pub(crate) passage_ids: Vec<String>,
    #[serde(default)]
    pub(crate) book_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetOutlineArguments {
    #[serde(default)]
    pub(crate) book_ids: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) max_depth: Option<usize>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

pub(crate) fn parse_arguments<T>(arguments: &str) -> AgentResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    if arguments.len() > 64 * 1024 {
        return Err(AgentError::LimitExceeded("tool argument size"));
    }
    serde_json::from_str(arguments).map_err(|error| AgentError::InvalidArguments(error.to_string()))
}

pub(crate) fn effective_limit(requested: Option<usize>, maximum: usize) -> AgentResult<usize> {
    let requested = requested.unwrap_or(maximum);
    if requested == 0 {
        return Err(AgentError::InvalidArguments(
            "limit must be greater than zero".to_string(),
        ));
    }
    Ok(requested.min(maximum))
}
