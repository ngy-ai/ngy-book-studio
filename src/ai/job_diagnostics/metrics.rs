//! Aggregated counters for one background job.

use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobLogMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_column: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<JobLogErrorKind>,
}

impl JobLogMetrics {
    pub fn for_error(error: &anyhow::Error) -> Self {
        let http_status = error
            .downcast_ref::<crate::ai::ProviderHttpError>()
            .map(|error| u64::from(error.status))
            .or_else(|| {
                error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(reqwest::Error::status)
                    .map(|status| u64::from(status.as_u16()))
            });
        Self {
            error_kind: Some(classify_error(error)),
            http_status,
            ..Self::default()
        }
    }
}
