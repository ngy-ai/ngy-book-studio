//! Spreadsheet rendering: A1 ranges, slices and the SVG table they become.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct A1Range {
    pub(crate) start_col: u32,
    pub(crate) start_row: u32,
    pub(crate) end_col: u32,
    pub(crate) end_row: u32,
}

impl A1Range {
    pub(crate) fn from_dimensions(
        start_col: u32,
        start_row: u32,
        columns: usize,
        rows: usize,
    ) -> Result<Self> {
        ensure!(columns > 0 && rows > 0, "工作表视觉区域不能为空");
        let columns = u32::try_from(columns).context("工作表视觉列数过大")?;
        let rows = u32::try_from(rows).context("工作表视觉行数过大")?;
        let end_col = start_col
            .checked_add(columns - 1)
            .context("工作表视觉列范围溢出")?;
        let end_row = start_row
            .checked_add(rows - 1)
            .context("工作表视觉行范围溢出")?;
        ensure!(
            end_col < MAX_XLSX_COLUMNS && end_row < MAX_XLSX_ROWS,
            "工作表视觉区域超过 XLSX 的有效边界"
        );
        Ok(Self {
            start_col,
            start_row,
            end_col,
            end_row,
        })
    }

    pub(crate) fn to_a1(self) -> String {
        format!(
            "{}{}:{}{}",
            a1_column_name(self.start_col),
            self.start_row + 1,
            a1_column_name(self.end_col),
            self.end_row + 1
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn render_structural_worksheet(
    request: &RenderUnitsRequest,
    unit: &ContentUnit,
    control: &RenderControl,
    descriptor: &RendererDescriptor,
    profile_id: &str,
    emitter: &RenderPageEmitter,
    total_bytes: &mut u64,
) -> Result<()> {
    let Some((table_id, rows)) = worksheet_table_rows(unit) else {
        return render_structural_flow(
            request,
            unit,
            control,
            descriptor,
            profile_id,
            emitter,
            total_bytes,
        )
        .await;
    };
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() || column_count == 0 {
        return render_structural_flow(
            request,
            unit,
            control,
            descriptor,
            profile_id,
            emitter,
            total_bytes,
        )
        .await;
    }

    let (sheet_name, source_start_col, source_start_row) = worksheet_source_origin(unit)?;
    let rows_per_page =
        ((request.profile.viewport_height.saturating_sub(192)) / 32).max(1) as usize;
    let columns_per_page =
        ((request.profile.viewport_width.saturating_sub(120)) / 180).max(1) as usize;
    let row_page_count = rows.len().div_ceil(rows_per_page);
    let column_page_count = column_count.div_ceil(columns_per_page);
    let page_count = row_page_count
        .checked_mul(column_page_count)
        .context("工作表视觉分页数量溢出")?;
    ensure!(
        page_count <= MAX_STRUCTURAL_PAGES_PER_UNIT,
        "单个工作表的结构化页面超过 {MAX_STRUCTURAL_PAGES_PER_UNIT} 页上限"
    );

    let mut unit_page = 0_usize;
    for row_start in (0..rows.len()).step_by(rows_per_page) {
        let row_end = (row_start + rows_per_page).min(rows.len());
        for column_start in (0..column_count).step_by(columns_per_page) {
            control.checkpoint().map_err(anyhow::Error::new)?;
            let (page_index, should_render) = emitter.claim_page();
            if !should_render {
                unit_page += 1;
                continue;
            }
            let column_end = (column_start + columns_per_page).min(column_count);
            let page_range = A1Range::from_dimensions(
                source_start_col
                    .checked_add(u32::try_from(column_start).context("工作表列偏移过大")?)
                    .context("工作表列范围溢出")?,
                source_start_row
                    .checked_add(u32::try_from(row_start).context("工作表行偏移过大")?)
                    .context("工作表行范围溢出")?,
                column_end - column_start,
                row_end - row_start,
            )?;
            let range_text = page_range.to_a1();
            let page_lines = worksheet_slice_lines(
                &unit.title,
                &range_text,
                &rows,
                row_start,
                row_end,
                column_start,
                column_end,
            );
            let profile = request.profile.clone();
            let raster_control = control.clone();
            let raster = tokio::task::spawn_blocking(move || {
                render_structural_png(&page_lines, &profile, &raster_control)
            })
            .await
            .context("工作表结构化 PNG 渲染线程异常退出")??;
            add_visual_page_bytes(total_bytes, raster.bytes.len())?;
            let metadata = render_metadata(request, unit, descriptor, profile_id);
            let locator =
                DocumentLocator::block(&request.document.id, &unit.id, &table_id).with_source(
                    SourceLocator::worksheet(sheet_name.clone(), Some(range_text.clone())),
                );
            let id_seed = format!(
                "{}\0{}\0{}\0worksheet\0{}\0{}\0{}",
                request.document.id,
                request.document.revision.get(),
                unit.id,
                range_text,
                metadata.renderer_version,
                metadata.profile_id
            );
            emitter
                .emit(RenderedVisualPage {
                    id: deterministic_id("visual-page", id_seed),
                    page_index,
                    content_unit_id: Some(unit.id.clone()),
                    width: raster.width,
                    height: raster.height,
                    media_type: "image/png".to_string(),
                    bytes: raster.bytes,
                    locator,
                    metadata,
                })
                .await?;
            unit_page += 1;
            tokio::task::yield_now().await;
        }
    }
    debug_assert_eq!(unit_page, page_count);

    append_structural_image_pages(
        request,
        unit,
        control,
        descriptor,
        profile_id,
        emitter,
        total_bytes,
    )
    .await
}

pub(crate) fn worksheet_table_rows(unit: &ContentUnit) -> Option<(String, Vec<Vec<String>>)> {
    unit.document.blocks.iter().find_map(|block| {
        let Block::Table { id, header, rows } = block else {
            return None;
        };
        let mut output = Vec::with_capacity(rows.len() + usize::from(header.is_some()));
        if let Some(header) = header {
            output.push(worksheet_row_values(header));
        }
        output.extend(rows.iter().map(worksheet_row_values));
        Some((id.clone(), output))
    })
}

pub(crate) fn worksheet_row_values(row: &TableRow) -> Vec<String> {
    row.cells.iter().map(|cell| cell.plain_text()).collect()
}

pub(crate) fn worksheet_source_origin(unit: &ContentUnit) -> Result<(String, u32, u32)> {
    let Some(SourceLocator::Worksheet { name, range }) = unit.source_locator.as_ref() else {
        return Ok((unit.title.clone(), 0, 0));
    };
    let Some(range) = range.as_deref() else {
        return Ok((name.clone(), 0, 0));
    };
    let parsed = parse_a1_range(range)
        .with_context(|| format!("工作表 {} 的来源区域不是有效的 A1 范围：{range}", name))?;
    Ok((name.clone(), parsed.start_col, parsed.start_row))
}

pub(crate) fn worksheet_slice_lines(
    title: &str,
    range: &str,
    rows: &[Vec<String>],
    row_start: usize,
    row_end: usize,
    column_start: usize,
    column_end: usize,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(row_end - row_start + 1);
    lines.push(format!("{title} · {range}"));
    let available_columns = column_end.saturating_sub(column_start).max(1);
    let cell_characters = (72 / available_columns).clamp(6, 36);
    for row in &rows[row_start..row_end] {
        let line = (column_start..column_end)
            .map(|column| {
                truncate_visual_cell(
                    row.get(column).map(String::as_str).unwrap_or_default(),
                    cell_characters,
                )
            })
            .collect::<Vec<_>>()
            .join(" │ ");
        lines.push(line);
    }
    lines
}

pub(crate) fn truncate_visual_cell(value: &str, max_characters: usize) -> String {
    let value = value.replace(['\r', '\n'], " ");
    if value.chars().count() <= max_characters {
        return value;
    }
    let mut output = value
        .chars()
        .take(max_characters.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

pub(crate) fn parse_a1_range(value: &str) -> Option<A1Range> {
    let mut parts = value.split(':');
    let start = parse_a1_cell(parts.next()?)?;
    let end = match parts.next() {
        Some(value) => parse_a1_cell(value)?,
        None => start,
    };
    if parts.next().is_some() || start.0 > end.0 || start.1 > end.1 {
        return None;
    }
    Some(A1Range {
        start_col: start.0,
        start_row: start.1,
        end_col: end.0,
        end_row: end.1,
    })
}

pub(crate) fn parse_a1_cell(value: &str) -> Option<(u32, u32)> {
    let normalized = value.trim().replace('$', "");
    let digit = normalized.find(|character: char| character.is_ascii_digit())?;
    let (column, row) = normalized.split_at(digit);
    if column.is_empty()
        || row.is_empty()
        || !column.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !row.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut column_index = 0_u32;
    for byte in column.bytes() {
        column_index = column_index
            .checked_mul(26)?
            .checked_add(u32::from(byte.to_ascii_uppercase() - b'A') + 1)?;
    }
    let row = row.parse::<u32>().ok()?;
    if row == 0 || row > MAX_XLSX_ROWS || column_index == 0 || column_index > MAX_XLSX_COLUMNS {
        return None;
    }
    Some((column_index - 1, row - 1))
}

pub(crate) fn a1_column_name(column: u32) -> String {
    let mut value = u64::from(column) + 1;
    let mut bytes = Vec::new();
    while value > 0 {
        value -= 1;
        bytes.push(b'A' + (value % 26) as u8);
        value /= 26;
    }
    bytes.reverse();
    String::from_utf8(bytes).expect("A1 columns are ASCII")
}
