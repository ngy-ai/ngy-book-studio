//! Roles, identifiers, titles and timestamps.

use super::*;

pub(crate) fn role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

pub(crate) fn parse_role(value: &str) -> Result<ChatRole> {
    match value {
        "system" => Ok(ChatRole::System),
        "user" => Ok(ChatRole::User),
        "assistant" => Ok(ChatRole::Assistant),
        "tool" => Ok(ChatRole::Tool),
        _ => bail!("unknown chat role: {value}"),
    }
}

pub(crate) fn validate_id(label: &str, value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.trim() == value
            && value.len() <= 512
            && !value.chars().any(char::is_control),
        "{label} is invalid"
    );
    Ok(())
}

pub(crate) fn validate_title(title: &str) -> Result<()> {
    ensure!(!title.trim().is_empty(), "chat title cannot be empty");
    ensure!(
        title.chars().count() <= MAX_TITLE_CHARS && !title.chars().any(char::is_control),
        "chat title is invalid"
    );
    Ok(())
}

pub(crate) fn normalized_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_secs())
}

pub(crate) fn next_id(namespace: &str) -> Result<String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(deterministic_id(
        namespace,
        format!("{}:{}:{}", elapsed.as_nanos(), std::process::id(), sequence),
    ))
}
