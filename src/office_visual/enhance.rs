//! The enhancement control loop and the source checks around it.

use super::*;

pub(crate) async fn enhance_with_control(
    service: &OfficePreviewService,
    format: BookFormat,
    original: Arc<Vec<u8>>,
    cancellation: OfficeCancellation,
    control: &RenderControl,
) -> Result<EnhancedOfficePreview> {
    let future = service.enhance(format, original, cancellation.clone());
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return output,
            _ = tokio::time::sleep(CANCELLATION_POLL_INTERVAL) => {
                if let Err(interrupted) = control.checkpoint() {
                    cancellation.cancel();
                    // Let the STA worker observe cancellation before its
                    // temporary workspace is dropped.
                    let _ = future.await;
                    return Err(anyhow::Error::new(interrupted));
                }
            }
        }
    }
}

pub(crate) fn imported_office_format(document: &BookDocument) -> Result<BookFormat> {
    match document.source {
        BookSource::Imported { format, .. }
            if matches!(
                format,
                BookFormat::Doc | BookFormat::Docx | BookFormat::Pptx | BookFormat::Xlsx
            ) =>
        {
            Ok(format)
        }
        _ => anyhow::bail!("Office 增强 renderer 只能处理导入的 Office 文档"),
    }
}

pub(crate) fn validate_office_source(
    source: &VisualSourcePayload,
    format: BookFormat,
) -> Result<()> {
    ensure!(
        source.source_kind == "original",
        "规范化编辑后的文档不能继续使用原始 Office 增强页面"
    );
    ensure!(
        source.format == format_name(format),
        "Office 增强来源格式与当前文档不一致"
    );
    ensure!(!source.bytes.is_empty(), "Office 增强来源内容为空");
    Ok(())
}

pub(crate) fn format_name(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Doc => "doc",
        BookFormat::Docx => "docx",
        BookFormat::Pptx => "pptx",
        BookFormat::Xlsx => "xlsx",
        _ => "unsupported",
    }
}
