//! Mapping a canonical source unit onto the document and its images.

use super::*;

/// Resolves a source link only after binding any native coordinate back to the
/// current canonical unit and the document's actual source format. Callers
/// may still apply a narrower window-specific type check (for example, PDF
/// viewers accept only `PdfPage`).
pub(crate) fn current_canonical_source_unit_index(
    source: &AiSourceLink,
    document: &BookDocument,
) -> std::result::Result<usize, String> {
    let unit_index = source
        .current_unit_index(document)
        .ok_or_else(|| "引用对应的文档或内容版本已失效".to_string())?;
    if source.locator.is_none() {
        return Ok(unit_index);
    }
    let locator = source
        .validated_locator()
        .ok_or_else(|| "引用定位无效".to_string())?;
    if let Some(native) = locator.source.as_ref() {
        let unit = &document.units[unit_index];
        if unit.source_locator.as_ref() != Some(native) {
            return Err("引用的原始来源位置与当前内容单元不匹配".to_string());
        }
        if !source_locator_matches_document(&document.source, native) {
            return Err("引用的原始来源类型与当前图书格式不匹配".to_string());
        }
    }
    Ok(unit_index)
}

pub(crate) fn source_locator_matches_document(
    source: &CanonicalBookSource,
    locator: &SourceLocator,
) -> bool {
    matches!(
        (source, locator),
        (CanonicalBookSource::Created, SourceLocator::Created)
            | (CanonicalBookSource::Imported { .. }, SourceLocator::Created)
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Epub,
                    ..
                },
                SourceLocator::Epub { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Pdf,
                    ..
                },
                SourceLocator::PdfPage { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Doc | BookFormat::Docx,
                    ..
                },
                SourceLocator::OfficeSection { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Pptx,
                    ..
                },
                SourceLocator::Slide { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Xlsx,
                    ..
                },
                SourceLocator::Worksheet { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Mobi | BookFormat::Azw | BookFormat::Azw3,
                    ..
                },
                SourceLocator::KindleSection { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Kfx,
                    ..
                },
                SourceLocator::KindleSection { .. },
            )
            | (
                CanonicalBookSource::Imported {
                    format: BookFormat::Djvu,
                    ..
                },
                SourceLocator::DjvuPage { .. },
            )
    )
}

pub(crate) fn image_format_from_mime(mime: &str) -> Option<ImageFormat> {
    match mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/png" => Some(ImageFormat::Png),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}
