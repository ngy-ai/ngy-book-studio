//! Small helpers for MIME types, reader paths and labels.

use super::*;

pub(crate) fn resource_mime(manifest_mime: &str, safe_path: &str) -> String {
    normalize_mime(manifest_mime)
        .or_else(|| {
            mime_guess::from_path(safe_path)
                .first_raw()
                .and_then(normalize_mime)
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

pub(crate) fn reader_url_path(url: &str) -> Option<&str> {
    const INTERNAL_ORIGINS: [&str; 3] = [
        "epubreader://book",
        "http://epubreader.book",
        "https://epubreader.book",
    ];
    INTERNAL_ORIGINS
        .iter()
        .find_map(|origin| url.strip_prefix(origin))
        .filter(|path| path.starts_with('/'))
        .map(|path| path.split(['?', '#']).next().unwrap_or(path))
}

/// 把当前阅读外观写到章节文档的根元素上。
///
/// 字号与皮肤都是根元素上的自定义属性，所以一次调用同时生效：章节不必为了改
/// 字号或换皮肤重新请求，每次章节加载后重放一次即可。字号在这里再夹一次，因为
/// 宿主是最后一处能约束旧设置行或页面回传值的关口。
///
/// 皮肤只改配色相关的自定义属性与 `color-scheme`，不动正文结构：标注位置按正文
/// 文本计算，换色不能让任何锚点位移。
pub fn reader_appearance_script(font_size: u8, appearance: Appearance) -> String {
    let font_size = reader_font_size(font_size);
    let palette = Palette::for_appearance(appearance);
    let setters = palette.css_variable_setters();
    format!(
        "(() => {{ const root = document.documentElement; \
         root.style.setProperty('--ngy-font-size', '{font_size}px'); \
         {setters}root.style.colorScheme = '{scheme}'; }})()",
        scheme = appearance.color_scheme()
    )
}

/// The one range a reading text size is allowed to take.
pub fn reader_font_size(font_size: u8) -> u8 {
    font_size.clamp(READER_FONT_SIZE_MIN, READER_FONT_SIZE_MAX)
}

pub(crate) fn safe_resource_path(request_path: &str) -> Result<String> {
    let encoded_path = request_path
        .split(['?', '#'])
        .next()
        .unwrap_or(request_path);
    let decoded = percent_decode_str(encoded_path)
        .decode_utf8()
        .context("EPUB 资源路径不是有效的 UTF-8")?;
    if decoded.contains('\0') || decoded.contains('\\') || decoded.contains(':') {
        bail!("EPUB 资源路径不安全");
    }
    if decoded
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        bail!("EPUB 资源路径越过了图书边界");
    }
    if !encoded_path.starts_with('/') || encoded_path == "/" {
        bail!("EPUB 资源路径无效");
    }
    Ok(encoded_path.to_string())
}

pub(crate) fn canonical_resource_path(request_path: &str) -> Result<String> {
    let safe_path = safe_resource_path(request_path)?;
    percent_decode_str(&safe_path)
        .decode_utf8()
        .context("EPUB 资源路径不是有效的 UTF-8")
        .map(|path| path.into_owned())
}

pub(crate) fn is_html_mime(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    matches!(mime, "application/xhtml+xml" | "text/html")
}

pub(crate) fn normalize_mime(mime: &str) -> Option<String> {
    let mime = mime.split(';').next()?.trim().to_ascii_lowercase();
    let (kind, subtype) = mime.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    };
    (valid(kind) && valid(subtype)).then_some(mime)
}

pub(crate) fn title_from_href(href: &str) -> String {
    let path = href.split(['?', '#']).next().unwrap_or(href);
    let decoded = percent_decode_str(path).decode_utf8_lossy();
    decoded
        .rsplit('/')
        .next()
        .unwrap_or("章节")
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or("章节")
        .replace(['-', '_'], " ")
}

pub(crate) fn non_empty_label(label: &str, href: Option<&str>) -> String {
    let label = label.trim();
    if label.is_empty() {
        href.map(title_from_href)
            .unwrap_or_else(|| "未命名分组".to_string())
    } else {
        label.to_string()
    }
}
