//! Turning laid-out lines into the SVG page handed to the rasterizer.

use super::*;

pub(crate) fn add_visual_page_bytes(total: &mut u64, byte_len: usize) -> Result<()> {
    *total = total
        .checked_add(byte_len as u64)
        .context("视觉页面输出总大小溢出")?;
    ensure!(
        *total <= MAX_VISUAL_BATCH_BYTES,
        "视觉页面输出超过 {MAX_VISUAL_BATCH_BYTES} 字节总上限"
    );
    Ok(())
}

pub(crate) fn select_units(
    request: &RenderUnitsRequest,
) -> Result<Vec<&crate::document::ContentUnit>> {
    if request.unit_ids.is_empty() {
        return Ok(request.document.units.iter().collect());
    }
    let mut selected = Vec::with_capacity(request.unit_ids.len());
    for unit_id in &request.unit_ids {
        let unit = request
            .document
            .find_unit(unit_id)
            .with_context(|| format!("视觉渲染引用了不存在的内容单元 {unit_id}"))?;
        ensure!(
            !selected
                .iter()
                .any(|selected: &&crate::document::ContentUnit| selected.id == unit.id),
            "视觉渲染不能重复选择同一内容单元 {unit_id}"
        );
        selected.push(unit);
    }
    Ok(selected)
}

pub(crate) fn render_svg_page(lines: &[String], profile: &RenderProfile) -> String {
    let (background, foreground, muted) = if profile.color_scheme == "dark" {
        ("#191919", "#f2efe7", "#a6a29a")
    } else {
        ("#fdfbf6", "#24221f", "#77716a")
    };
    let mut output = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}"><rect width="100%" height="100%" fill="{}"/><g fill="{}" font-family="Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif" font-size="22">"#,
        profile.viewport_width,
        profile.viewport_height,
        profile.viewport_width,
        profile.viewport_height,
        background,
        foreground,
    );
    for (index, line) in lines.iter().enumerate() {
        let y = 80 + index as u32 * 32;
        output.push_str(&format!(
            "<text x=\"60\" y=\"{y}\">{}</text>",
            escape_xml(line)
        ));
    }
    output.push_str(&format!(
        "</g><text x=\"60\" y=\"{}\" fill=\"{}\" font-family=\"Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif\" font-size=\"14\">{}</text></svg>",
        profile.viewport_height.saturating_sub(28),
        muted,
        escape_xml(&profile.name)
    ));
    output
}

pub(crate) fn render_slide_svg_page(
    lines: &[String],
    profile: &RenderProfile,
    image_count: usize,
) -> String {
    let (background, foreground, muted) = if profile.color_scheme == "dark" {
        ("#191919", "#f2efe7", "#a6a29a")
    } else {
        ("#fdfbf6", "#24221f", "#77716a")
    };
    let text_bottom = if image_count == 0 {
        profile.viewport_height.saturating_sub(48)
    } else {
        profile.viewport_height.saturating_mul(48) / 100
    };
    let line_count = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    let line_height = text_bottom
        .saturating_sub(72)
        .checked_div(line_count)
        .unwrap_or(1)
        .clamp(8, 32);
    let font_size = line_height
        .saturating_mul(3)
        .checked_div(4)
        .unwrap_or(8)
        .max(7);
    let mut output = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}"><rect width="100%" height="100%" fill="{}"/><g fill="{}" font-family="Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif" font-size="{}">"#,
        profile.viewport_width,
        profile.viewport_height,
        profile.viewport_width,
        profile.viewport_height,
        background,
        foreground,
        font_size,
    );
    for (index, line) in lines.iter().enumerate() {
        let y = 52_u32.saturating_add(
            u32::try_from(index)
                .unwrap_or(u32::MAX)
                .saturating_mul(line_height),
        );
        if y >= text_bottom {
            break;
        }
        output.push_str(&format!(
            "<text x=\"60\" y=\"{y}\">{}</text>",
            escape_xml(line)
        ));
    }
    if image_count > 0 {
        output.push_str(&format!(
            "</g><line x1=\"60\" x2=\"{}\" y1=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
            profile.viewport_width.saturating_sub(60),
            profile.viewport_height.saturating_mul(50) / 100,
            profile.viewport_height.saturating_mul(50) / 100,
            muted,
        ));
    } else {
        output.push_str("</g>");
    }
    output.push_str(&format!(
        "<text x=\"60\" y=\"{}\" fill=\"{}\" font-family=\"Microsoft YaHei, Noto Sans CJK SC, Segoe UI, sans-serif\" font-size=\"14\">{}</text></svg>",
        profile.viewport_height.saturating_sub(20),
        muted,
        escape_xml(&profile.name)
    ));
    output
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
