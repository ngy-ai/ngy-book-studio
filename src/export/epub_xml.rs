//! Cleaning up raw HTML and the XHTML/XML escaping helpers.

use super::*;

pub(crate) fn sanitize_raw_html(source: &str) -> String {
    pub(crate) fn safe_relative_url(url: &str) -> Option<Cow<'_, str>> {
        safe_link_href(url).then_some(Cow::Borrowed(url))
    }

    let mut builder = ammonia::Builder::default();
    builder
        .url_schemes(HashSet::from(["data"]))
        .url_relative(ammonia::UrlRelative::Custom(Box::new(safe_relative_url)));
    xhtmlify_void_elements(&builder.clean(source).to_string())
}

pub(crate) fn xhtmlify_void_elements(html: &str) -> String {
    pub(crate) const VOID_ELEMENTS: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];

    let bytes = html.as_bytes();
    let mut output = String::with_capacity(html.len());
    let mut copied_until = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'<'
            || bytes.get(cursor + 1).is_none_or(|byte| {
                matches!(*byte, b'/' | b'!' | b'?') || !byte.is_ascii_alphabetic()
            })
        {
            cursor += 1;
            continue;
        }
        let name_start = cursor + 1;
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b':'))
        {
            name_end += 1;
        }
        let name = &html[name_start..name_end];
        if !VOID_ELEMENTS
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            cursor = name_end;
            continue;
        }

        let mut end = name_end;
        let mut quote = None;
        while end < bytes.len() {
            match (bytes[end], quote) {
                (b'\'' | b'"', None) => quote = Some(bytes[end]),
                (byte, Some(active)) if byte == active => quote = None,
                (b'>', None) => break,
                _ => {}
            }
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
        let before_end = html[name_end..end].trim_end();
        if !before_end.ends_with('/') {
            output.push_str(&html[copied_until..end]);
            output.push('/');
            copied_until = end;
        }
        cursor = end + 1;
    }
    output.push_str(&html[copied_until..]);
    output
}

pub(crate) fn safe_link_href(href: &str) -> bool {
    let trimmed = href.trim();
    !trimmed.is_empty()
        && !trimmed.starts_with('/')
        && !trimmed.starts_with("\\\\")
        && !trimmed.split('/').any(|segment| segment == "..")
        && (!trimmed.contains(':') || trimmed.starts_with('#'))
}

pub(crate) fn required_asset_href<'a>(
    asset_hrefs: &'a HashMap<&str, String>,
    asset_id: &str,
) -> Result<&'a str> {
    asset_hrefs
        .get(asset_id)
        .map(String::as_str)
        .with_context(|| format!("EPUB 正文引用了无法导出的资源 {asset_id}"))
}

pub(crate) fn anchor_for_id(id: &str) -> String {
    format!("block-{}", blake3::hash(id.as_bytes()).to_hex())
}

pub(crate) fn escape_xml(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ if is_xml_character(character) => output.push(character),
            _ => output.push('\u{fffd}'),
        }
    }
    output
}

pub(crate) fn is_xml_character(character: char) -> bool {
    matches!(character, '\u{9}' | '\u{a}' | '\u{d}')
        || matches!(character as u32, 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}

pub(crate) fn base_media_type(media_type: &str) -> &str {
    media_type.split(';').next().unwrap_or(media_type).trim()
}

pub(crate) fn extension_for_media_type(media_type: &str) -> &'static str {
    match base_media_type(media_type) {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "text/css" => "css",
        "font/woff" | "application/font-woff" => "woff",
        "font/woff2" => "woff2",
        "font/ttf" | "application/x-font-ttf" => "ttf",
        "font/otf" | "application/vnd.ms-opentype" => "otf",
        _ => "bin",
    }
}
