//! User-initiated Microsoft Office preview enhancement.
//!
//! The caller supplies the byte-exact imported original. Work happens in an
//! isolated temporary directory and the result is copied into memory before
//! that directory is removed, so neither the original object nor a user's
//! source file can be modified by Office automation.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    document::BookFormat,
    office_com::{
        OfficeCancellation, OfficeEnhanceOutput, OfficeEnhanceRequest, OfficeEnhancementKind,
        OfficeEnhancer,
    },
};

const MAX_ENHANCED_PDF_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SLIDE_IMAGES: usize = 20_000;
const MAX_SLIDE_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnhancedSlideImage {
    pub file_name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnhancedOfficePreview {
    Pdf(Vec<u8>),
    Slides(Vec<EnhancedSlideImage>),
}

#[derive(Clone)]
pub struct OfficePreviewService {
    enhancer: Arc<dyn OfficeEnhancer>,
}

impl std::fmt::Debug for OfficePreviewService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OfficePreviewService")
            .finish_non_exhaustive()
    }
}

impl OfficePreviewService {
    pub fn new(enhancer: Arc<dyn OfficeEnhancer>) -> Self {
        Self { enhancer }
    }

    /// Runs the explicitly requested enhancement and returns owned output.
    /// Dropping this future or cancelling its token never changes `original`.
    pub async fn enhance(
        &self,
        format: BookFormat,
        original: Arc<Vec<u8>>,
        cancellation: OfficeCancellation,
    ) -> Result<EnhancedOfficePreview> {
        ensure!(!original.is_empty(), "Office 原件为空");
        let (kind, extension) = enhancement_for(format)?;
        let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
            .await
            .context("Office 临时目录任务异常退出")?
            .context("无法创建 Office 临时目录")?;
        let source = workspace.path().join(format!("original.{extension}"));
        tokio::fs::write(&source, original.as_slice())
            .await
            .with_context(|| format!("无法写入 Office 临时原件：{}", source.display()))?;

        let target = match kind {
            OfficeEnhancementKind::WordPdf | OfficeEnhancementKind::ExcelPdf => {
                workspace.path().join("preview.pdf")
            }
            OfficeEnhancementKind::PowerPointImages => {
                let directory = workspace.path().join("slides");
                tokio::fs::create_dir(&directory)
                    .await
                    .context("无法创建 PowerPoint 临时图片目录")?;
                directory
            }
        };
        let mut request = OfficeEnhanceRequest::new(kind, source, target);
        request.cancellation = cancellation;
        let output = self
            .enhancer
            .enhance(request)
            .await
            .context("Microsoft Office 增强预览不可用；请继续使用结构化预览")?;

        match output {
            OfficeEnhanceOutput::Pdf(path) => {
                ensure_inside(workspace.path(), &path)?;
                let metadata = tokio::fs::metadata(&path)
                    .await
                    .context("无法读取 Office PDF 元数据")?;
                ensure!(
                    metadata.len() <= MAX_ENHANCED_PDF_BYTES,
                    "Office PDF 超过大小上限"
                );
                let bytes = tokio::fs::read(&path)
                    .await
                    .context("无法读取 Office PDF")?;
                ensure!(bytes.starts_with(b"%PDF-"), "Office 返回的文件不是 PDF");
                let bytes = tokio::task::spawn_blocking(move || {
                    lopdf::Document::load_mem(&bytes).context("Office 返回的 PDF 无效")?;
                    Ok::<_, anyhow::Error>(bytes)
                })
                .await
                .context("Office PDF 校验线程异常退出")??;
                Ok(EnhancedOfficePreview::Pdf(bytes))
            }
            OfficeEnhanceOutput::Images(paths) => {
                ensure!(
                    !paths.is_empty() && paths.len() <= MAX_SLIDE_IMAGES,
                    "PowerPoint 图片数量无效"
                );
                let mut slides = Vec::with_capacity(paths.len());
                for path in paths {
                    ensure_inside(workspace.path(), &path)?;
                    let metadata = tokio::fs::metadata(&path)
                        .await
                        .context("无法读取 PowerPoint 图片元数据")?;
                    ensure!(
                        metadata.len() <= MAX_SLIDE_IMAGE_BYTES,
                        "PowerPoint 单页图片超过大小上限"
                    );
                    let bytes = tokio::fs::read(&path)
                        .await
                        .context("无法读取 PowerPoint 图片")?;
                    let image_format =
                        image::guess_format(&bytes).context("PowerPoint 返回了无法识别的图片")?;
                    let media_type = match image_format {
                        image::ImageFormat::Png => "image/png",
                        image::ImageFormat::Jpeg => "image/jpeg",
                        _ => bail!("PowerPoint 返回了不受支持的图片格式"),
                    };
                    slides.push(EnhancedSlideImage {
                        file_name: path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("slide")
                            .to_string(),
                        media_type: media_type.to_string(),
                        bytes,
                    });
                }
                slides.sort_by(|left, right| {
                    natural_slide_key(&left.file_name).cmp(&natural_slide_key(&right.file_name))
                });
                Ok(EnhancedOfficePreview::Slides(slides))
            }
        }
    }
}

fn enhancement_for(format: BookFormat) -> Result<(OfficeEnhancementKind, &'static str)> {
    Ok(match format {
        BookFormat::Doc => (OfficeEnhancementKind::WordPdf, "doc"),
        BookFormat::Docx => (OfficeEnhancementKind::WordPdf, "docx"),
        BookFormat::Pptx => (OfficeEnhancementKind::PowerPointImages, "pptx"),
        BookFormat::Xlsx => (OfficeEnhancementKind::ExcelPdf, "xlsx"),
        _ => bail!("该图书格式不支持 Microsoft Office 增强预览"),
    })
}

fn ensure_inside(root: &Path, output: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root).context("无法解析 Office 临时目录")?;
    let output = std::fs::canonicalize(output).context("无法解析 Office 增强产物")?;
    ensure!(output.starts_with(root), "Office 增强产物逃逸临时目录");
    Ok(())
}

fn natural_slide_key(name: &str) -> (String, u64, String) {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let split = stem
        .trim_end_matches(|character: char| character.is_ascii_digit())
        .len();
    let (prefix, number) = stem.split_at(split);
    (
        prefix.to_ascii_lowercase(),
        number.parse().unwrap_or(u64::MAX),
        name.to_ascii_lowercase(),
    )
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin};

    use lopdf::dictionary;

    use super::*;

    #[derive(Clone, Copy)]
    struct FakeEnhancer {
        slides: bool,
    }

    impl OfficeEnhancer for FakeEnhancer {
        fn enhance<'a>(
            &'a self,
            request: OfficeEnhanceRequest,
        ) -> Pin<Box<dyn Future<Output = Result<OfficeEnhanceOutput>> + Send + 'a>> {
            Box::pin(async move {
                assert!(request.source.is_file());
                if self.slides {
                    let first = request.target.join("Slide10.png");
                    let second = request.target.join("Slide2.png");
                    let png = tiny_png();
                    std::fs::write(&first, &png)?;
                    std::fs::write(&second, &png)?;
                    Ok(OfficeEnhanceOutput::Images(vec![first, second]))
                } else {
                    let pdf = test_pdf();
                    std::fs::write(&request.target, pdf)?;
                    Ok(OfficeEnhanceOutput::Pdf(request.target))
                }
            })
        }
    }

    fn tiny_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([1, 2, 3, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    }

    fn test_pdf() -> Vec<u8> {
        let mut document = lopdf::Document::with_version("1.7");
        let pages = document.new_object_id();
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        });
        document.objects.insert(
            pages,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page.into()],
                "Count" => 1,
            }),
        );
        let catalog = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages,
        });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn reads_valid_pdf_before_removing_the_temporary_workspace() {
        let service = OfficePreviewService::new(Arc::new(FakeEnhancer { slides: false }));
        let output = service
            .enhance(
                BookFormat::Docx,
                Arc::new(b"docx".to_vec()),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(matches!(output, EnhancedOfficePreview::Pdf(bytes) if bytes.starts_with(b"%PDF-")));
    }

    #[tokio::test]
    async fn slide_names_are_naturally_sorted_and_bytes_are_owned() {
        let service = OfficePreviewService::new(Arc::new(FakeEnhancer { slides: true }));
        let output = service
            .enhance(
                BookFormat::Pptx,
                Arc::new(b"pptx".to_vec()),
                Default::default(),
            )
            .await
            .unwrap();
        let EnhancedOfficePreview::Slides(slides) = output else {
            panic!("expected slide images");
        };
        assert_eq!(
            slides
                .iter()
                .map(|slide| slide.file_name.as_str())
                .collect::<Vec<_>>(),
            ["Slide2.png", "Slide10.png"]
        );
        assert!(slides.iter().all(|slide| !slide.bytes.is_empty()));
    }

    #[test]
    fn rejects_non_office_formats_without_running_a_worker() {
        assert!(enhancement_for(BookFormat::Pdf).is_err());
    }

    #[test]
    fn natural_slide_sort_handles_non_ascii_prefixes() {
        let mut names = ["幻灯片10.png", "幻灯片2.png"];
        names.sort_by_key(|name| natural_slide_key(name));
        assert_eq!(names, ["幻灯片2.png", "幻灯片10.png"]);
    }
}
