//! Identifier validation and UTF-8 safe truncation.

use super::*;

pub(crate) fn validate_identifier(field: &str, value: &str) -> AgentResult<()> {
    if !valid_identifier(value) {
        return Err(AgentError::InvalidArguments(format!(
            "{field} must be 1..=512 bytes and contain no whitespace or control characters"
        )));
    }
    Ok(())
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .chars()
            .all(|character| !character.is_whitespace() && !character.is_control())
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let suffix = "…";
    let mut end = max_bytes.saturating_sub(suffix.len()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], suffix)
}
