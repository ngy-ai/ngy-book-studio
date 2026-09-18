//! Turning a spreadsheet used range into the HTML table the unit carries.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorksheetRange {
    pub(crate) start_col: u32,
    pub(crate) start_row: u32,
    pub(crate) end_col: u32,
    pub(crate) end_row: u32,
}

impl WorksheetRange {
    pub(crate) fn to_a1(self) -> String {
        format!(
            "{}{}:{}{}",
            office_oxide::xlsx::CellRef::col_name(self.start_col),
            self.start_row + 1,
            office_oxide::xlsx::CellRef::col_name(self.end_col),
            self.end_row + 1
        )
    }

    pub(crate) fn row_count(self) -> u32 {
        self.end_row - self.start_row + 1
    }

    pub(crate) fn column_count(self) -> u32 {
        self.end_col - self.start_col + 1
    }
}

pub(crate) fn worksheet_used_range(
    worksheet: &office_oxide::xlsx::Worksheet,
    workbook: &office_oxide::xlsx::XlsxDocument,
) -> Option<WorksheetRange> {
    let mut range: Option<WorksheetRange> = None;
    for row in &worksheet.rows {
        for cell in &row.cells {
            let visible = !workbook.format_cell_value(cell).trim().is_empty()
                || cell
                    .formula
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
            if !visible {
                continue;
            }
            let col = cell.reference.col;
            let row = cell.reference.row;
            range = Some(match range {
                Some(range) => WorksheetRange {
                    start_col: range.start_col.min(col),
                    start_row: range.start_row.min(row),
                    end_col: range.end_col.max(col),
                    end_row: range.end_row.max(row),
                },
                None => WorksheetRange {
                    start_col: col,
                    start_row: row,
                    end_col: col,
                    end_row: row,
                },
            });
        }
    }
    range
}

pub(crate) fn worksheet_grid_html(
    worksheet: &office_oxide::xlsx::Worksheet,
    workbook: &office_oxide::xlsx::XlsxDocument,
    range: Option<WorksheetRange>,
    limits: &ImportLimits,
) -> Result<String> {
    let mut html = String::new();
    if let Some(range) = range {
        let rows = range.row_count();
        let columns = range.column_count();
        if range.end_col >= MAX_XLSX_COLUMNS || range.end_row >= MAX_XLSX_ROWS {
            bail!("XLSX used range exceeds the supported worksheet boundary");
        }
        if rows > MAX_XLSX_GRID_ROWS {
            bail!("XLSX used range exceeds the {MAX_XLSX_GRID_ROWS} row safety limit");
        }
        let cells = u64::from(rows)
            .checked_mul(u64::from(columns))
            .context("XLSX used range size overflowed")?;
        if cells > MAX_XLSX_GRID_CELLS {
            bail!("XLSX used range exceeds the {MAX_XLSX_GRID_CELLS} cell safety limit");
        }

        let mut values = HashMap::new();
        for row in &worksheet.rows {
            for cell in &row.cells {
                if cell.reference.row < range.start_row
                    || cell.reference.row > range.end_row
                    || cell.reference.col < range.start_col
                    || cell.reference.col > range.end_col
                {
                    continue;
                }
                let mut value = workbook.format_cell_value(cell);
                if value.trim().is_empty()
                    && let Some(formula) = cell
                        .formula
                        .as_deref()
                        .map(str::trim)
                        .filter(|formula| !formula.is_empty())
                {
                    value = format!("={formula}");
                }
                if !value.is_empty() {
                    values.insert((cell.reference.row, cell.reference.col), value);
                }
            }
        }

        html.push_str("<table><thead>");
        push_worksheet_html_row(&mut html, range.start_row, range, &values, "th");
        html.push_str("</thead><tbody>");
        for row in range.start_row + 1..=range.end_row {
            push_worksheet_html_row(&mut html, row, range, &values, "td");
            if html.len() > limits.max_unit_text_bytes {
                bail!("Office content unit exceeds the text safety limit");
            }
        }
        html.push_str("</tbody></table>");
    }
    for shape in &worksheet.text_shapes {
        let text = shape.text.trim();
        if text.is_empty() {
            continue;
        }
        html.push_str("<p>");
        push_escaped_html_text(&mut html, text);
        html.push_str("</p>");
        if html.len() > limits.max_unit_text_bytes {
            bail!("Office content unit exceeds the text safety limit");
        }
    }
    if html.len() > limits.max_unit_text_bytes {
        bail!("Office content unit exceeds the text safety limit");
    }
    Ok(html)
}

pub(crate) fn push_worksheet_html_row(
    output: &mut String,
    row: u32,
    range: WorksheetRange,
    values: &HashMap<(u32, u32), String>,
    cell_tag: &str,
) {
    output.push_str("<tr>");
    for col in range.start_col..=range.end_col {
        output.push('<');
        output.push_str(cell_tag);
        output.push('>');
        if let Some(value) = values.get(&(row, col)) {
            push_escaped_html_text(output, value);
        }
        output.push_str("</");
        output.push_str(cell_tag);
        output.push('>');
    }
    output.push_str("</tr>");
}

pub(crate) fn push_escaped_html_text(output: &mut String, value: &str) {
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                output.push_str("<br>");
            }
            '\n' => output.push_str("<br>"),
            _ => output.push(character),
        }
    }
}
