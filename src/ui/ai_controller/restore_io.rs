//! Restoring the persisted conversation and validating its live sources.

use super::*;

pub(crate) fn restored_messages(session: ChatSession) -> Vec<AiRestoredMessage> {
    session
        .messages
        .into_iter()
        .filter_map(|message| {
            let role = match message.role {
                ChatRole::User => AiRestoredRole::User,
                ChatRole::Assistant => AiRestoredRole::Assistant,
                ChatRole::System | ChatRole::Tool => return None,
            };
            // Persisted citation-row presence is the durable grounding fact. Record it
            // before malformed, stale, or unauthorized links are filtered for display.
            let source_status = match role {
                AiRestoredRole::User => None,
                AiRestoredRole::Assistant => Some(if message.citations.is_empty() {
                    AgentAnswerSourceStatus::NoVerifiedSources
                } else {
                    AgentAnswerSourceStatus::VerifiedKnowledgeBaseSources
                }),
            };
            Some(AiRestoredMessage {
                role,
                content: message.content,
                sources: message
                    .citations
                    .into_iter()
                    .filter_map(source_link_from_chat_citation)
                    .collect(),
                source_status,
            })
        })
        .collect()
}

pub(crate) async fn validated_restored_messages(
    services: Arc<AppServices>,
    session: ChatSession,
) -> Result<Vec<AiRestoredMessage>> {
    let allowed_book_ids = session.thread.scope.book_ids.clone();
    let messages = restored_messages(session);
    services
        .spawn_library_read(move |library| {
            let allowed = allowed_book_ids
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut messages = messages;
            for message in &mut messages {
                message.sources.retain_mut(|source| {
                    if !allowed.contains(source.book_id.as_str()) {
                        return false;
                    }
                    let document = library.document(&source.book_id).ok();
                    source.mark_restored_status(document.as_ref());
                    true
                });
            }
            Ok(messages)
        })
        .await
        .context("恢复引用状态的任务已停止")?
}

pub(crate) async fn validate_live_sources(
    services: Arc<AppServices>,
    allowed_book_ids: Vec<String>,
    sources: Vec<AiSourceLink>,
) -> Result<Vec<AiSourceLink>> {
    services
        .spawn_library_read(move |library| {
            let allowed = allowed_book_ids
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut validated = Vec::with_capacity(sources.len());
            for mut source in sources {
                if !allowed.contains(source.book_id.as_str()) {
                    continue;
                }
                let document = library.document(&source.book_id).ok();
                source.mark_current_status(document.as_ref());
                validated.push(source);
            }
            Ok(validated)
        })
        .await
        .context("校验引用状态的任务已停止")?
}
