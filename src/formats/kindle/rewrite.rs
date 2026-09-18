//! Rewriting the image references inside the extracted markup.

use super::*;

/// Points every Kindle media reference at the retained resource.
///
/// Two spellings reach this point: the `kindle:embed:<base32>` a KF8 container
/// writes, and the `images/img_NNNN.ext` form `ebook-rs` substitutes for the
/// references its own decimal, zero-padded search happens to match. Both name
/// the same record counter, so both are mapped back onto it.
pub(crate) fn rewrite_kindle_media_references(
    html: &str,
    paths: &HashMap<usize, String>,
) -> String {
    let html = rewrite_kindle_embed_references(html, paths);
    rewrite_kindle_legacy_references(&html, paths)
}

pub(crate) fn rewrite_kindle_embed_references(
    html: &str,
    paths: &HashMap<usize, String>,
) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find(KINDLE_EMBED_PREFIX) {
        let start = cursor + offset;
        let tail = &html[start + KINDLE_EMBED_PREFIX.len()..];
        let end = tail
            .find(|character: char| {
                !character.is_ascii()
                    || !BASE32_DIGITS.contains(&(character.to_ascii_uppercase() as u8))
            })
            .unwrap_or(tail.len());
        output.push_str(&html[cursor..start]);
        match resource_number_from_base32(&tail[..end]).and_then(|number| paths.get(&number)) {
            Some(path) => {
                output.push_str(path);
                // `?mime=image/jpg` is left over from the original Kindle URL
                // and is meaningless once the reference names a retained asset.
                cursor = start + KINDLE_EMBED_PREFIX.len() + end + mime_query_len(&tail[end..]);
            }
            None => {
                output.push_str(KINDLE_EMBED_PREFIX);
                output.push_str(&tail[..end]);
                cursor = start + KINDLE_EMBED_PREFIX.len() + end;
            }
        }
    }
    output.push_str(&html[cursor..]);
    output
}

pub(crate) fn rewrite_kindle_legacy_references(
    html: &str,
    paths: &HashMap<usize, String>,
) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find(KINDLE_LEGACY_IMAGE_PREFIX) {
        let start = cursor + offset;
        let tail = &html[start + KINDLE_LEGACY_IMAGE_PREFIX.len()..];
        let digits_end = tail
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(tail.len());
        let Some(path) = tail[..digits_end]
            .parse::<usize>()
            .ok()
            .and_then(|number| paths.get(&number))
        else {
            output.push_str(&html[cursor..start + KINDLE_LEGACY_IMAGE_PREFIX.len()]);
            cursor = start + KINDLE_LEGACY_IMAGE_PREFIX.len();
            continue;
        };
        let extension_start = digits_end + usize::from(tail[digits_end..].starts_with('.'));
        let extension_len = tail[extension_start..]
            .find(|character: char| !character.is_ascii_alphanumeric())
            .unwrap_or(tail.len() - extension_start);
        let consumed = extension_start + extension_len;
        output.push_str(&html[cursor..start]);
        output.push_str(path);
        let after = &tail[consumed..];
        cursor = start + KINDLE_LEGACY_IMAGE_PREFIX.len() + consumed + mime_query_len(after);
    }
    output.push_str(&html[cursor..]);
    output
}

pub(crate) fn mime_query_len(value: &str) -> usize {
    let Some(query) = value.strip_prefix("?mime=") else {
        return 0;
    };
    let end = query
        .find(|character: char| {
            !character.is_ascii_alphanumeric()
                && character != '/'
                && character != '-'
                && character != '+'
        })
        .unwrap_or(query.len());
    "?mime=".len() + end
}

pub(crate) fn resource_number_from_base32(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let mut value = 0_usize;
    for byte in text.bytes() {
        let digit = BASE32_DIGITS
            .iter()
            .position(|candidate| *candidate == byte.to_ascii_uppercase())?;
        value = value.checked_mul(32)?.checked_add(digit)?;
    }
    Some(value)
}
