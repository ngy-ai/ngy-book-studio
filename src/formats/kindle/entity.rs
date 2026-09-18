//! Decoding the HTML entities Kindle markup still carries.

/// How far past an `&` the decoder looks for the `;` that closes an entity.
/// Entity names are ASCII, so a window that stops a little early is harmless —
/// but it must never cut a character in half: `value.len()` counts bytes, and a
/// bare `&` in CJK text puts byte 34 inside a three-byte character.
pub(crate) const ENTITY_SCAN_BYTES: usize = 34;

/// First [`ENTITY_SCAN_BYTES`] bytes of `value`, floored to a character
/// boundary so the caller can slice with it.
pub(crate) fn entity_scan_window(value: &str) -> &str {
    let mut end = value.len().min(ENTITY_SCAN_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn decode_entities(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(semicolon) = entity_scan_window(tail).find(';') else {
            output.push('&');
            rest = &rest[index + 1..];
            continue;
        };
        match decode_entity(&tail[1..semicolon]) {
            Some(decoded) => {
                output.push_str(&decoded);
                rest = &tail[semicolon + 1..];
            }
            None => {
                output.push('&');
                rest = &rest[index + 1..];
            }
        }
    }
    output.push_str(rest);
    output
}

pub(crate) fn decode_entity(entity: &str) -> Option<String> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return char::from_u32(u32::from_str_radix(hex, 16).ok()?).map(String::from);
    }
    if let Some(decimal) = entity.strip_prefix('#') {
        return char::from_u32(decimal.parse().ok()?).map(String::from);
    }
    Some(
        match entity {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" => "\u{00A0}",
            "mdash" => "\u{2014}",
            "ndash" => "\u{2013}",
            "hellip" => "\u{2026}",
            "lsquo" => "\u{2018}",
            "rsquo" => "\u{2019}",
            "ldquo" => "\u{201C}",
            "rdquo" => "\u{201D}",
            _ => return None,
        }
        .to_string(),
    )
}

pub(crate) fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}
