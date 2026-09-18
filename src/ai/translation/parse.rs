//! Parsing an accepted answer and the character classification helpers.

use super::*;

/// Validates an already size-bounded provider response and restores source order.
/// Neither provider field names nor response excerpts are included in errors.
pub fn parse_response(
    source: &TranslationSource,
    response: &str,
    identity: &str,
) -> Result<StoredTranslation> {
    ensure!(!identity.trim().is_empty(), "翻译执行身份不能为空");
    ensure!(!source.segments.is_empty(), "原文没有可翻译片段");
    let response = decode_response(response)?;
    if response.translations.len() != source.segments.len() {
        return Err(ResponseError {
            expected_segments: Some(source.segments.len()),
            actual_segments: Some(response.translations.len()),
            ..ResponseError::new("segment_count_mismatch")
        }
        .into());
    }
    let mut translated = vec![None; source.segments.len()];
    for segment in response.translations {
        if segment.id >= translated.len() {
            return Err(ResponseError::new("unknown_segment_id").into());
        }
        if translated[segment.id].is_some() {
            return Err(ResponseError::new("duplicate_segment_id").into());
        }
        let value = segment.text.trim_matches(is_matching_whitespace);
        if value.is_empty() {
            return Err(ResponseError::new("empty_segment_text").into());
        }
        if value.contains('\0') {
            return Err(ResponseError::new("invalid_segment_text").into());
        }

        // Translation changes the text, while spacing at formatting boundaries
        // belongs to the source template (for example `word <strong>word</strong>`).
        let original = &source.segments[segment.id];
        let start = original.len() - original.trim_start_matches(is_matching_whitespace).len();
        let end = original.trim_end_matches(is_matching_whitespace).len();
        ensure!(start < end, "原文片段没有可翻译文字");
        translated[segment.id] = Some(format!("{}{value}{}", &original[..start], &original[end..]));
    }
    let segments = source
        .segments
        .iter()
        .zip(translated)
        .map(|(source, translated)| {
            Ok(TranslationSegment {
                source: source.clone(),
                translated: translated.ok_or_else(|| ResponseError::new("missing_segment_id"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(StoredTranslation {
        execution_identity: identity.to_string(),
        segments,
    })
}

/// Diagnostics-only shape of one response: the fragment id and translated char
/// count of every segment the strict decoder accepted. It runs the exact same
/// decoder as [`parse_response`], so a rejected response is described by exactly
/// the fragments the protocol saw. Only numbers leave this function — no model
/// text, field name or wrapper prose can reach a log line (see the translation
/// diagnostics constraints in `docs/agent/code-map-ai.md`).
pub fn decoded_segment_shapes(
    response: &str,
) -> std::result::Result<Vec<(usize, usize)>, ResponseError> {
    Ok(decode_response(response)?
        .translations
        .into_iter()
        .map(|segment| (segment.id, segment.text.chars().count()))
        .collect())
}

/// ECMAScript `\s`: Rust's `char::is_whitespace` differs for BOM and U+0085.
pub(crate) fn is_matching_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// Unicode `Cf` format characters that render as nothing but are not
/// ECMAScript `\s`. A known EPUB code listing indents lines with ZWSP, which
/// survives the whitespace filter alone and then has nothing to translate.
pub(crate) fn is_invisible_format(value: char) -> bool {
    matches!(
        value,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// A text leaf owns a translation slot only when it has visible content.
/// Whitespace-only or invisible-format-only leaves have nothing to translate:
/// a model answers them with blanks, and the protocol rejects blank segment
/// text for the whole block. `src/ui/reader/translations.js` filters its leaves
/// with the same rule, so both sides agree on which leaves are slots.
pub(crate) fn has_visible_text(value: &str) -> bool {
    value
        .chars()
        .any(|value| !is_matching_whitespace(value) && !is_invisible_format(value))
}

pub(crate) fn normalize_source_text(value: &str) -> String {
    value
        .split(is_matching_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
