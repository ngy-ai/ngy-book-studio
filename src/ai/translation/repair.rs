//! Repairing structural punctuation the model wrote in full width.

use super::*;

/// How a string literal was opened. Full-width quotes are only treated as
/// delimiters for a string those same characters opened, so a full-width quote
/// inside an ASCII-quoted translation stays content.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuoteStyle {
    Ascii,
    FullWidth,
}

/// Full-width punctuation a CJK-oriented model writes where JSON requires a
/// structural character. Only the characters that can never be content outside
/// a string literal are mapped; everything else (including every character
/// inside a string) is left byte-identical.
pub(crate) fn structural_ascii(value: char) -> Option<char> {
    Some(match value {
        '\u{ff1a}' => ':', // ：
        '\u{ff0c}' => ',', // ，
        '\u{ff5b}' => '{',
        '\u{ff5d}' => '}',
        '\u{ff3b}' => '[',
        '\u{ff3d}' => ']',
        '\u{ff02}' | '\u{201c}' | '\u{201d}' => '"', // ＂ “ ”
        _ => return None,
    })
}

/// Rewrites structural full-width punctuation to its ASCII form and reports how
/// many characters changed. Positions inside string literals are copied
/// verbatim, so this can never alter a translation. A key that already absorbed
/// the separator (`"text："`) has no character left to rewrite and is rejected
/// instead of guessed.
pub(crate) fn repair_structural_punctuation(response: &str) -> (Cow<'_, str>, usize) {
    let mut repaired: Option<String> = None;
    let mut copied = 0;
    let mut replaced = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, value) in response.char_indices() {
        match quote {
            Some(QuoteStyle::Ascii) => {
                if escaped {
                    escaped = false;
                } else if value == '\\' {
                    escaped = true;
                } else if value == '"' {
                    quote = None;
                }
                continue;
            }
            Some(QuoteStyle::FullWidth) => {
                if matches!(value, '\u{201d}' | '\u{ff02}') {
                    replace_with(
                        &mut repaired,
                        &mut copied,
                        &mut replaced,
                        response,
                        index,
                        '"',
                    );
                    quote = None;
                }
                continue;
            }
            None => {}
        }
        match value {
            '"' => quote = Some(QuoteStyle::Ascii),
            '\u{201c}' | '\u{ff02}' => {
                replace_with(
                    &mut repaired,
                    &mut copied,
                    &mut replaced,
                    response,
                    index,
                    '"',
                );
                quote = Some(QuoteStyle::FullWidth);
            }
            value => {
                if let Some(ascii) = structural_ascii(value) {
                    replace_with(
                        &mut repaired,
                        &mut copied,
                        &mut replaced,
                        response,
                        index,
                        ascii,
                    );
                }
            }
        }
    }
    match repaired {
        Some(mut text) => {
            text.push_str(&response[copied..]);
            (Cow::Owned(text), replaced)
        }
        None => (Cow::Borrowed(response), replaced),
    }
}

pub(crate) fn replace_with(
    repaired: &mut Option<String>,
    copied: &mut usize,
    replaced: &mut usize,
    source: &str,
    index: usize,
    ascii: char,
) {
    let text = repaired.get_or_insert_with(|| String::with_capacity(source.len()));
    text.push_str(&source[*copied..index]);
    text.push(ascii);
    *copied = index + source[index..].chars().next().map_or(0, char::len_utf8);
    *replaced += 1;
}
