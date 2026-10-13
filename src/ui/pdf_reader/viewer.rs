//! The viewer URL, its scripts and the resources it loads.

use super::*;

/// Page scale the reader opens at, in thousandths of the viewer's base scale
/// (one PDF point becomes 1.5 CSS pixels). The bundled viewer clamps and steps
/// the same range under Ctrl + wheel; `the_bundled_viewer_zooms_in_the_host_range`
/// fails if the two copies drift apart.
pub(crate) const PDF_ZOOM_DEFAULT_MILLI: u32 = 1_500;
pub(crate) const PDF_ZOOM_MIN_MILLI: u32 = 500;
pub(crate) const PDF_ZOOM_MAX_MILLI: u32 = 4_000;

pub(crate) fn pdf_zoom_milli(zoom_milli: u32) -> u32 {
    zoom_milli.clamp(PDF_ZOOM_MIN_MILLI, PDF_ZOOM_MAX_MILLI)
}

pub(crate) fn pdf_viewer_url(initial_page: u32, compact_reading: bool) -> String {
    // The default URL stays byte-identical to the pre-preference one; the
    // preference is only named when it is enabled.
    if compact_reading {
        format!("ngypdf://viewer/viewer.html?startPage={initial_page}&compact=1")
    } else {
        format!("ngypdf://viewer/viewer.html?startPage={initial_page}")
    }
}

/// The single contract behind the compact reading preference: page spacing is
/// one attribute on the viewer document, so the URL parameter and a live update
/// of an already open reader apply exactly the same state.
pub(crate) fn pdf_compact_reading_script(compact_reading: bool) -> &'static str {
    if compact_reading {
        "document.documentElement.setAttribute(\"data-pdf-compact\", \"1\");"
    } else {
        "document.documentElement.removeAttribute(\"data-pdf-compact\");"
    }
}

/// PDF.js viewer 里跟着皮肤走的部分。
///
/// viewer 的样式表是随 PDF.js 一起发布的产物（`assets/pdfjs/viewer.html`），它的
/// 外壳写死成深色，宿主改不了产物源码，只能在运行时覆盖根元素与占位页。页面卡片
/// 本身**不跟着变色**：纸张是白的，两套皮肤下染白页都会让正文对比度变差。
///
/// 覆盖规则只能画半透明层。正文的字形画在 canvas 上，`.textLayer` 的 span 是
/// `color: transparent` 的覆盖层，任何不透明的底色都画在 canvas 之上、把正文连同
/// 选中区域一起盖住：选中一段文字后那里只剩一块纯色。选中底色因此是叠了 alpha 的
/// 强调色（与 `Palette::selection()` 同一档 40%），浅色与深色皮肤落在白纸上都读得清；
/// 万一 WebView 不认识 `color-mix`，这条声明会被整条丢弃，产物自带的半透明蓝底
/// （`rgb(52 104 255 / 0.35)`）仍是可读的兜底。
pub(crate) const PDF_APPEARANCE_RULES: &str = r#"
#status { color: var(--ngy-muted); }
.pdf-page[data-rendered="false"] { background: var(--ngy-sidebar); }
.page-placeholder { color: var(--ngy-muted); }
.page-placeholder.error { color: var(--ngy-danger); }
.textLayer ::selection { background: color-mix(in srgb, var(--ngy-accent) 40%, transparent); }
"#;

/// 把当前皮肤写进 PDF.js viewer。
///
/// 与编辑器同一套做法：变量写在根元素上，覆盖规则只写一次，两套皮肤各取各的值。
/// viewer 是独立文档，所以变量与样式表都要在它自己的文档里再放一份。
pub(crate) fn pdf_appearance_script(appearance: Appearance) -> String {
    let palette = Palette::for_appearance(appearance);
    let scheme = appearance.color_scheme();
    let setters = palette.css_variable_setters();
    let rules = serde_json::to_string(PDF_APPEARANCE_RULES).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(() => {{ const root = document.documentElement; \
         root.setAttribute('data-ngy-theme', '{scheme}'); \
         root.style.colorScheme = '{scheme}'; \
         root.style.background = '{}'; \
         root.style.color = '{}'; \
         {setters}\
         let style = document.getElementById('ngy-pdf-appearance'); \
         if (!style) {{ style = document.createElement('style'); \
         style.id = 'ngy-pdf-appearance'; \
         (document.head || document.documentElement).appendChild(style); }} \
         style.textContent = {rules}; }})()",
        Palette::hex(palette.paper),
        Palette::hex(palette.ink),
    )
}

/// Sends one page scale to an already open viewer. The viewer keeps the page
/// being read at the top of the window while it re-lays out, and reports the
/// scale back once the reader stops wheeling.
pub(crate) fn pdf_zoom_script(zoom_milli: u32) -> String {
    let payload = serde_json::json!({
        "type": "ngy-pdf-zoom",
        "zoomMilli": pdf_zoom_milli(zoom_milli),
    });
    format!("window.postMessage({payload}, window.location.origin);")
}

pub(crate) fn pdf_progress_write(
    page_number: u32,
    page: Option<&PdfReaderPage>,
) -> Result<ReadingProgressWrite, String> {
    let page = page
        .filter(|page| page.page_number == page_number)
        .ok_or_else(|| format!("第 {page_number} 页没有对应的规范内容单元"))?;
    let unit_id = page
        .unit_id
        .as_deref()
        .filter(|unit_id| !unit_id.trim().is_empty())
        .ok_or_else(|| format!("第 {page_number} 页缺少稳定内容单元定位"))?;
    let spine_index = page
        .unit_index
        .ok_or_else(|| format!("第 {page_number} 页缺少规范内容单元序号"))?;
    Ok(ReadingProgressWrite::Unit {
        spine_index,
        unit_id: unit_id.to_string(),
        anchor: None,
    })
}

pub(crate) fn pdf_resource(
    pdf_bytes: &[u8],
    request_path: &str,
) -> Option<(&'static str, Cow<'static, [u8]>)> {
    let path = request_path.strip_prefix('/').unwrap_or(request_path);
    if path == "document.pdf" {
        return Some(("application/pdf", Cow::Owned(pdf_bytes.to_vec())));
    }
    // PDF.js ships an optional QuickJS sandbox for document actions. This
    // reader renders static pages only, so do not expose that executable at
    // all even though XFA and annotations are also disabled in the viewer.
    if matches!(path, "wasm/quickjs-eval.js" | "wasm/quickjs-eval.wasm") {
        return None;
    }
    bundled_pdfjs_asset(path).map(|(mime, bytes)| (mime, Cow::Borrowed(bytes)))
}
