//! Timestamp and error-text helpers.

use super::*;

pub(crate) fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix epoch")
        .map(|duration| duration.as_secs())
}

pub(crate) fn truncate_error(error: &str) -> String {
    pub(crate) const MAX_ERROR_CHARS: usize = 4_096;
    if error.chars().count() <= MAX_ERROR_CHARS {
        error.to_string()
    } else {
        error.chars().take(MAX_ERROR_CHARS).collect()
    }
}
