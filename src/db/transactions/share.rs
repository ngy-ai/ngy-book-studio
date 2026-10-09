//! Writing the notes, conversations and translations of a share package.
//!
//! The receiving side re-anchors every item against its own document, so all of
//! this runs after the shared book has been published and inside one write
//! transaction: a package either lands completely or leaves the library
//! unchanged. Items that no longer describe the receiving book are counted and
//! dropped instead of being guessed at — a stale note must never be pinned onto
//! text the sender never selected.

use std::collections::{HashMap, HashSet};

use super::*;

use crate::{
    ai::translation::StoredTranslation,
    annotations::{
        Annotation, MAX_ANNOTATION_QUOTE_BYTES, TextAnchor, compact_text, validate_anchor,
        validate_comment,
    },
    document::{DocumentLocator, deterministic_id},
    share::{
        MAX_SHARE_QUOTE_BYTES, ShareTranslationStamp, SharedChatThread, SharedNote,
        SharedTranslation,
    },
};

/// One content unit of the receiving document, in reading order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShareUnitTarget {
    pub(crate) ordinal: usize,
    pub(crate) id: String,
    pub(crate) revision: u64,
    /// PDF/DjVu scan pages anchor by range length instead of chapter text.
    pub(crate) is_page: bool,
}

pub(crate) struct ShareApplyRequest<'a> {
    pub(crate) book_id: &'a str,
    pub(crate) document_revision: u64,
    pub(crate) units: &'a [ShareUnitTarget],
    /// Reader body text per unit ordinal, present for reflowable chapters.
    pub(crate) unit_texts: &'a HashMap<usize, String>,
    pub(crate) notes: &'a [SharedNote],
    pub(crate) chats: &'a [SharedChatThread],
    pub(crate) translations: &'a [SharedTranslation],
    pub(crate) stamp: Option<&'a ShareTranslationStamp>,
    /// The language the shared machine译文 should be stored under, or `None` to
    /// keep each row's own language.
    ///
    /// Only set when the whole package is one language and the receiver has a
    /// reading language of its own: writing several shared languages into one
    /// would make them collide on the translation scope key and silently
    /// overwrite each other.
    pub(crate) restamp_language: Option<&'a str>,
    /// Whether the receiving record *is* the sender's document. Only then do
    /// the sender's book, unit and block ids mean anything here, and only then
    /// may a citation keep the sender's block-level locator.
    pub(crate) preserve_locators: bool,
    pub(crate) now: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShareApplyReport {
    pub(crate) notes_imported: usize,
    pub(crate) notes_skipped: usize,
    pub(crate) chat_threads: usize,
    pub(crate) chat_messages: usize,
    pub(crate) translations_imported: usize,
    pub(crate) translations_skipped: usize,
}

pub(crate) fn apply_shared_sections(
    conn: &mut Connection,
    request: &ShareApplyRequest<'_>,
) -> Result<ShareApplyReport> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分享包写入事务")?;
    let mut report = ShareApplyReport::default();
    import_notes(&tx, request, &mut report)?;
    import_chats(&tx, request, &mut report)?;
    import_translations(&tx, request, &mut report)?;
    tx.commit().context("无法提交分享包写入事务")?;
    Ok(report)
}

fn import_notes(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    report: &mut ShareApplyReport,
) -> Result<()> {
    for note in request.notes {
        if insert_shared_note(conn, request, note)? {
            report.notes_imported += 1;
        } else {
            report.notes_skipped += 1;
            tracing::debug!(
                target: "ngy_share",
                chapter = %shared_unit_label(&note.unit),
                ordinal = note.unit.ordinal,
                "分享包：笔记与接收方正文对不上，已跳过"
            );
        }
    }
    Ok(())
}

/// Names the chapter an item belonged to, for diagnostics. The sender's title is
/// the only human-readable handle that survives the transfer, because the
/// chapter's own IDs do not.
fn shared_unit_label(unit: &crate::share::SharedUnitRef) -> String {
    match unit.title.as_deref() {
        Some(title) if !title.trim().is_empty() => title.to_string(),
        _ => format!("第 {} 章", unit.ordinal + 1),
    }
}

/// Returns `false` when the note cannot describe this document. The receiver's
/// own text must still carry the quote at the recorded offsets: a note is never
/// relocated by searching for its text elsewhere.
fn insert_shared_note(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    note: &SharedNote,
) -> Result<bool> {
    let Some(kind) = note.kind() else {
        return Ok(false);
    };
    let Some(unit) = request.units.get(note.unit.ordinal) else {
        return Ok(false);
    };
    if note.quote.len() > MAX_ANNOTATION_QUOTE_BYTES
        || note.start >= note.end
        || compact_text(&note.quote).is_empty()
    {
        return Ok(false);
    }
    let anchor = TextAnchor {
        quote: note.quote.clone(),
        start: note.start,
        end: note.end,
    };
    if !anchors_current_text(request, unit, &anchor) {
        return Ok(false);
    }
    if kind.is_comment() {
        let Some(comment) = note.comment.as_deref() else {
            return Ok(false);
        };
        if validate_comment(comment, kind).is_err() {
            return Ok(false);
        }
    } else if note.comment.is_some() {
        return Ok(false);
    }
    let updated_at = note.updated_at.max(note.created_at);
    let stored = Annotation {
        id: deterministic_id(
            "annotation",
            format!(
                "share\0{}\0{}\0{}\0{}\0{}\0{}",
                request.book_id,
                unit.id,
                kind.as_str(),
                anchor.start,
                anchor.end,
                note.created_at
            ),
        ),
        book_id: request.book_id.to_string(),
        content_unit_id: unit.id.clone(),
        document_revision: request.document_revision,
        unit_revision: unit.revision,
        anchor,
        kind,
        comment: note.comment.clone(),
        created_at: note.created_at,
        updated_at,
        stale: false,
    };
    if kind.is_mark()
        && let Some(existing) = super::super::annotations::mark_at_anchor(
            conn,
            request.book_id,
            &unit.id,
            request.document_revision,
            unit.revision,
            &stored.anchor,
        )?
    {
        // One style per exact selection is a schema invariant. Re-importing the
        // same package finds the same style and is a no-op; a mark the receiver
        // placed on that very range keeps the style its own reader chose instead
        // of being recolored by somebody else's package.
        return Ok(existing.kind == kind);
    }
    // A thought whose derived ID is already stored was imported by an earlier
    // run; that is a no-op, not a skip.
    super::super::annotations::insert_if_absent(conn, &stored)?;
    Ok(true)
}

fn anchors_current_text(
    request: &ShareApplyRequest<'_>,
    unit: &ShareUnitTarget,
    anchor: &TextAnchor,
) -> bool {
    if unit.is_page {
        // Scanned pages have no host-side text projection; their quote and range
        // are two views of the same selection, exactly as when it was created.
        return u64::from(anchor.end - anchor.start)
            == compact_text(&anchor.quote).encode_utf16().count() as u64;
    }
    let Some(text) = request.unit_texts.get(&unit.ordinal) else {
        return false;
    };
    validate_anchor(text, anchor).is_ok()
}

fn import_chats(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    report: &mut ShareApplyReport,
) -> Result<()> {
    for thread in request.chats {
        let (thread_id, created) = ensure_shared_thread(conn, request, thread)?;
        if created {
            report.chat_threads += 1;
        }
        report.chat_messages += append_shared_messages(conn, request, &thread_id, thread)?;
    }
    Ok(())
}

/// A shared conversation's identity: the sender's title, its creation instant
/// and the first turn it carries.
///
/// Position in a list is deliberately *not* part of it. The sender's thread list
/// is ordered by recent activity, so replying to an older conversation would
/// renumber it and make the next export look like a brand new conversation. The
/// first turn never changes (conversations are append-only here) and still
/// distinguishes a thread that was recreated with the same title in the same
/// second.
fn shared_thread_id(book_id: &str, thread: &SharedChatThread) -> String {
    let first = match thread.messages.first() {
        Some(message) => format!(
            "{}\0{}",
            message.created_at,
            blake3::hash(message.content.as_bytes()).to_hex()
        ),
        None => thread.created_at.to_string(),
    };
    deterministic_id(
        "chat-thread",
        format!(
            "share\0{book_id}\0{}\0{}\0{first}",
            thread.created_at, thread.title
        ),
    )
}

/// Inserts the conversation unless it is already here. An existing thread keeps
/// the title and timestamps the receiver may have edited; whichever of its turns
/// are missing are appended by [`append_shared_messages`].
fn ensure_shared_thread(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    thread: &SharedChatThread,
) -> Result<(String, bool)> {
    let thread_id = shared_thread_id(request.book_id, thread);
    if super::super::chat_threads::get(conn, &thread_id)?.is_some() {
        return Ok((thread_id, false));
    }
    let window_kind = match thread.window_kind.as_str() {
        "editor" => "editor",
        _ => "reader",
    };
    let scope_json = serde_json::to_string(&serde_json::json!({
        "book_ids": [request.book_id],
    }))
    .context("无法序列化分享对话的授权范围")?;
    super::super::chat_threads::insert(
        conn,
        &super::super::chat_threads::ChatThread {
            id: thread_id.clone(),
            book_id: Some(request.book_id.to_string()),
            title: thread.title.clone(),
            scope_json,
            window_kind: window_kind.to_string(),
            created_at: thread.created_at,
            updated_at: thread.updated_at.max(thread.created_at),
        },
    )?;
    Ok((thread_id, true))
}

/// Appends the turns this thread does not have yet, so re-sharing an updated
/// conversation adds what is new instead of duplicating the whole thing.
///
/// Returns how many messages were actually written.
fn append_shared_messages(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    thread_id: &str,
    thread: &SharedChatThread,
) -> Result<usize> {
    let stored = super::super::chat_messages::list_for_thread(conn, thread_id)?
        .into_iter()
        .map(|message| message.ordinal)
        .collect::<HashSet<_>>();
    let message_ids = thread
        .messages
        .iter()
        .map(|message| {
            (
                message.ordinal,
                deterministic_id(
                    "chat-message",
                    format!("{thread_id}\0{}\0{}", message.ordinal, message.created_at),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut written = 0;
    for message in &thread.messages {
        let Some(id) = message_ids.get(&message.ordinal) else {
            continue;
        };
        // Either already imported, or this ordinal holds a different turn of a
        // thread that merely shares a name; neither may be displaced.
        if stored.contains(&message.ordinal) {
            continue;
        }
        let parent_id = message
            .parent_ordinal
            .and_then(|ordinal| message_ids.get(&ordinal).cloned());
        super::super::chat_messages::insert(
            conn,
            &super::super::chat_messages::ChatMessage {
                id: id.clone(),
                thread_id: thread_id.to_string(),
                parent_id,
                ordinal: message.ordinal,
                role: normalize_chat_role(&message.role).to_string(),
                content: message.content.clone(),
                model: message.model.clone(),
                created_at: message.created_at,
            },
        )?;
        for (ordinal, citation) in message.citations.iter().enumerate() {
            let Some(stored) = shared_citation(conn, request, id, ordinal, citation)? else {
                continue;
            };
            super::super::chat_citations::insert(conn, &stored)?;
        }
        written += 1;
    }
    Ok(written)
}

/// Rebuilds one citation against the receiving document.
///
/// A receiver that owns the sender's own book keeps the sender's locator after
/// revalidating it, so the citation still jumps to its block and its quote. Any
/// other receiver gets a locator that names the chapter only: the exact block of
/// a document this library published itself is not reproducible, and a locator
/// that merely looks plausible would be worse than none. Web citations carry no
/// locator at all.
fn shared_citation(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    message_id: &str,
    ordinal: usize,
    citation: &crate::share::SharedChatCitation,
) -> Result<Option<super::super::chat_citations::ChatCitation>> {
    if citation.quote.len() > MAX_SHARE_QUOTE_BYTES {
        return Ok(None);
    }
    let is_web = citation.source_kind == "web";
    let unit = citation
        .unit
        .as_ref()
        .and_then(|unit| request.units.get(unit.ordinal));
    if !is_web && unit.is_none() {
        return Ok(None);
    }
    let id = deterministic_id(
        "chat-citation",
        format!("{message_id}\0{ordinal}\0{}", citation.quote),
    );
    if super::super::chat_citations::get(conn, &id)?.is_some() {
        return Ok(None);
    }
    let chapter_locator = match unit {
        Some(unit) => Some(
            serde_json::to_string(&DocumentLocator::unit(request.book_id, unit.id.clone()))
                .context("无法序列化分享引用的定位信息")?,
        ),
        None => None,
    };
    let mut stored = super::super::chat_citations::ChatCitation {
        id,
        message_id: message_id.to_string(),
        content_unit_id: unit.map(|unit| unit.id.clone()),
        // The receiving library's search index is derived and rebuilt locally;
        // an index chunk reference from the sender would be dangling.
        search_chunk_id: None,
        ordinal,
        quote: citation.quote.clone(),
        document_revision: request.document_revision,
        unit_revision: unit
            .map(|unit| unit.revision)
            .unwrap_or(request.document_revision),
        locator_json: chapter_locator.clone(),
        source_kind: if is_web { "web" } else { "book" }.to_string(),
        url: citation.url.clone(),
        source_title: citation.source_title.clone(),
        created_at: request.now,
    };
    let sender_locator = citation
        .locator_json
        .as_deref()
        .filter(|_| request.preserve_locators);
    if let Some(sender_locator) = sender_locator {
        stored.locator_json = Some(sender_locator.to_string());
        // The locator was written against somebody else's database; it is kept
        // only if it still describes this very document, block and all.
        let allowed = [request.book_id.to_string()];
        if super::validate_current_chat_citation(conn, &stored, &allowed).is_err() {
            stored.locator_json = chapter_locator;
        }
    }
    Ok(Some(stored))
}

fn normalize_chat_role(role: &str) -> &'static str {
    match role {
        "system" => "system",
        "assistant" => "assistant",
        "tool" => "tool",
        _ => "user",
    }
}

fn import_translations(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    report: &mut ShareApplyReport,
) -> Result<()> {
    for translation in request.translations {
        if insert_shared_translation(conn, request, translation)? {
            report.translations_imported += 1;
        } else {
            report.translations_skipped += 1;
            tracing::debug!(
                target: "ngy_share",
                chapter = %shared_unit_label(&translation.unit),
                ordinal = translation.unit.ordinal,
                "分享包：译文与接收方文本块对不上，已跳过"
            );
        }
    }
    Ok(())
}

fn insert_shared_translation(
    conn: &Connection,
    request: &ShareApplyRequest<'_>,
    translation: &SharedTranslation,
) -> Result<bool> {
    let Some(unit) = request.units.get(translation.unit.ordinal) else {
        return Ok(false);
    };
    if translation.source_text.is_empty()
        || translation.translated_text.len() > crate::share::MAX_SHARE_TRANSLATION_BYTES
        || translation.target_language.trim().is_empty()
    {
        return Ok(false);
    }
    let model = match request.stamp {
        Some(stamp) => stamp.model.clone(),
        None => translation.model.clone(),
    };
    if model.trim().is_empty() {
        return Ok(false);
    }
    // The reader filters machine rows by the active translation engine. A package
    // is an explicit adoption, so the receiving side stamps the shared text with
    // its own engine; without a configured engine the sender's identity is kept
    // and the rows stay hidden until the settings line up.
    let translated_text = match request.stamp {
        Some(stamp) => {
            let Ok(mut stored) =
                serde_json::from_str::<StoredTranslation>(&translation.translated_text)
            else {
                return Ok(false);
            };
            stored.execution_identity = stamp.execution_identity.clone();
            serde_json::to_string(&stored).context("无法写入分享译文")?
        }
        None => {
            if serde_json::from_str::<StoredTranslation>(&translation.translated_text).is_err() {
                return Ok(false);
            }
            translation.translated_text.clone()
        }
    };
    let manual_text = match translation.manual_text.as_deref() {
        None => None,
        Some(manual) => {
            if manual.len() > crate::share::MAX_SHARE_TRANSLATION_BYTES
                || serde_json::from_str::<StoredTranslation>(manual).is_err()
            {
                None
            } else {
                Some(manual.to_string())
            }
        }
    };
    let target_language = match request.restamp_language {
        Some(language) => language.to_string(),
        None => translation.target_language.clone(),
    };
    let block_id = format!("{}::h{}", unit.id, translation.block_index);
    super::super::translations::upsert(
        conn,
        &super::super::translations::NewTranslation {
            book_id: request.book_id.to_string(),
            content_unit_id: unit.id.clone(),
            // The receiving document numbers its own blocks; only the unit and
            // the block position inside it are portable.
            block_id: block_id.clone(),
            ordinal: translation.ordinal as u64,
            document_revision: request.document_revision,
            unit_revision: unit.revision,
            target_language: target_language.clone(),
            source_language: translation.source_language.clone(),
            model,
            source_text: translation.source_text.clone(),
            translated_text,
            created_at: request.now,
            updated_at: request.now,
        },
    )?;
    // The machine path never writes the manual column, and the shared text of a
    // block the sender hand-edited is exactly that column.
    if let Some(manual_text) = manual_text {
        super::super::translations::set_manual_text(
            conn,
            request.book_id,
            &unit.id,
            &block_id,
            &target_language,
            request.document_revision,
            unit.revision,
            Some(&manual_text),
            request.now,
        )?;
    }
    Ok(true)
}
