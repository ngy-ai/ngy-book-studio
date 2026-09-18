//! Wrapping text, laying out pages and serializing the PDF objects.

use super::*;

pub(crate) fn wrap_visual_line(line: &str, max_width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut width = 0;
    for character in line.chars() {
        let character_width = if character.is_ascii() { 1 } else { 2 };
        if width > 0 && width + character_width > max_width {
            result.push(std::mem::take(&mut current));
            width = 0;
        }
        current.push(character);
        width += character_width;
    }
    if !current.is_empty() || result.is_empty() {
        result.push(current);
    }
    result
}

pub(crate) fn build_pdf(document: &BookDocument, pages: &[PreparedPdfPage]) -> Result<Vec<u8>> {
    ensure!(!pages.is_empty(), "PDF 至少需要一页");
    let mut pdf = PdfBuilder::default();
    let catalog_id = pdf.reserve();
    let pages_id = pdf.reserve();
    let font_id = pdf.reserve();
    let cid_font_id = pdf.reserve();
    let font_descriptor_id = pdf.reserve();
    let info_id = pdf.reserve();

    pdf.set(
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    )?;
    pdf.set(
        font_id,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H /DescendantFonts [{cid_font_id} 0 R] >>"
        )
        .into_bytes(),
    )?;
    pdf.set(
        cid_font_id,
        format!(
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> /FontDescriptor {font_descriptor_id} 0 R /DW 1000 >>"
        )
        .into_bytes(),
    )?;
    pdf.set(
        font_descriptor_id,
        b"<< /Type /FontDescriptor /FontName /STSong-Light /Flags 6 /FontBBox [-25 -254 1000 880] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 >>".to_vec(),
    )?;
    let author = document.authors.join(", ");
    pdf.set(
        info_id,
        format!(
            "<< /Title <{}> /Author <{}> /Producer <{}> >>",
            pdf_utf16_hex(&document.title),
            pdf_utf16_hex(&author),
            pdf_utf16_hex("墨页")
        )
        .into_bytes(),
    )?;

    let mut page_ids = Vec::with_capacity(pages.len());
    for page in pages {
        let page_id = pdf.reserve();
        page_ids.push(page_id);
        match page {
            PreparedPdfPage::Text(lines) => {
                let content_id = pdf.add(pdf_stream(&render_pdf_text(lines), ""));
                pdf.set(
                    page_id,
                    format!(
                        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PDF_WIDTH} {PDF_HEIGHT}] /Resources << /Font << /F1 {font_id} 0 R >> >> /Contents {content_id} 0 R >>"
                    )
                    .into_bytes(),
                )?;
            }
            PreparedPdfPage::Image(image) => {
                let image_id = pdf.add(pdf_stream(
                    &image.rgb,
                    &format!(
                        "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8",
                        image.width, image.height
                    ),
                ));
                let content = render_pdf_image_placement(image.width, image.height);
                let content_id = pdf.add(pdf_stream(content.as_bytes(), ""));
                pdf.set(
                    page_id,
                    format!(
                        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PDF_WIDTH} {PDF_HEIGHT}] /Resources << /XObject << /Im1 {image_id} 0 R >> >> /Contents {content_id} 0 R >>"
                    )
                    .into_bytes(),
                )?;
            }
        }
    }
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.set(
        pages_id,
        format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} >>",
            page_ids.len()
        )
        .into_bytes(),
    )?;
    let document_hash = document.document_hash();
    let document_id = &document_hash[..32];
    pdf.finish(catalog_id, info_id, document_id)
}

pub(crate) fn render_pdf_text(lines: &[String]) -> Vec<u8> {
    let mut content = format!(
        "BT\n/F1 11 Tf\n15 TL\n{PDF_MARGIN} {} Td\n",
        PDF_HEIGHT - PDF_MARGIN - 11.0
    );
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            content.push_str("T*\n");
        }
        if !line.is_empty() {
            content.push('<');
            content.push_str(&pdf_ucs2_hex(line));
            content.push_str("> Tj\n");
        }
    }
    content.push_str("ET\n");
    content.into_bytes()
}

pub(crate) fn render_pdf_image_placement(width: u32, height: u32) -> String {
    let available_width = PDF_WIDTH - 2.0 * PDF_MARGIN;
    let available_height = PDF_HEIGHT - 2.0 * PDF_MARGIN;
    let scale = (available_width / width as f32).min(available_height / height as f32);
    let draw_width = width as f32 * scale;
    let draw_height = height as f32 * scale;
    let x = (PDF_WIDTH - draw_width) / 2.0;
    let y = (PDF_HEIGHT - draw_height) / 2.0;
    format!("q\n{draw_width:.3} 0 0 {draw_height:.3} {x:.3} {y:.3} cm\n/Im1 Do\nQ\n")
}

pub(crate) fn pdf_ucs2_hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 4);
    for character in value.chars() {
        let scalar = character as u32;
        let value = if scalar <= 0xffff {
            scalar as u16
        } else {
            0xfffd
        };
        output.push_str(&format!("{value:04X}"));
    }
    output
}

pub(crate) fn pdf_utf16_hex(value: &str) -> String {
    let mut output = String::from("FEFF");
    for value in value.encode_utf16() {
        output.push_str(&format!("{value:04X}"));
    }
    output
}

pub(crate) fn pdf_stream(bytes: &[u8], dictionary: &str) -> Vec<u8> {
    let separator = if dictionary.is_empty() { "" } else { " " };
    let mut output = format!(
        "<<{separator}{dictionary} /Length {} >>\nstream\n",
        bytes.len()
    )
    .into_bytes();
    output.extend_from_slice(bytes);
    output.extend_from_slice(b"\nendstream");
    output
}

#[derive(Default)]
pub(crate) struct PdfBuilder {
    pub(crate) objects: Vec<Option<Vec<u8>>>,
}

impl PdfBuilder {
    pub(crate) fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }

    pub(crate) fn add(&mut self, object: Vec<u8>) -> usize {
        let id = self.reserve();
        self.objects[id - 1] = Some(object);
        id
    }

    pub(crate) fn set(&mut self, id: usize, object: Vec<u8>) -> Result<()> {
        let slot = self
            .objects
            .get_mut(id.checked_sub(1).context("PDF 对象编号不能为 0")?)
            .context("PDF 对象编号越界")?;
        ensure!(slot.is_none(), "PDF 对象 {id} 被重复写入");
        *slot = Some(object);
        Ok(())
    }

    pub(crate) fn finish(
        self,
        root_id: usize,
        info_id: usize,
        document_id: &str,
    ) -> Result<Vec<u8>> {
        let mut output = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let object_count = self.objects.len();
        let mut offsets = Vec::with_capacity(object_count);
        for (index, object) in self.objects.into_iter().enumerate() {
            let object = object.with_context(|| format!("PDF 对象 {} 未初始化", index + 1))?;
            offsets.push(output.len());
            output.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            output.extend_from_slice(&object);
            output.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = output.len();
        output.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        output.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            ensure!(offset <= 9_999_999_999, "PDF 文件过大，交叉引用偏移溢出");
            output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        output.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root_id} 0 R /Info {info_id} 0 R /ID [<{document_id}><{document_id}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
                object_count + 1
            )
            .as_bytes(),
        );
        Ok(output)
    }
}
