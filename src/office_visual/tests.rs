//! Tests for the enhanced Office renderer.

use std::path::PathBuf;

use super::*;
use crate::{
    document::{AssetRef, AssetRole, BlockDocument, ContentUnit, ContentUnitKind},
    office_com::{OfficeEnhanceOutput, OfficeEnhanceRequest, OfficeEnhancementKind, OfficeFuture},
    preview::{RenderProfile, VisualAssetPayload, VisualDocumentSource},
};
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};

#[derive(Clone, Copy)]
struct FakeSlides;

impl OfficeEnhancer for FakeSlides {
    fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
        Box::pin(async move {
            assert_eq!(request.kind, OfficeEnhancementKind::PowerPointImages);
            let paths = ["Slide10.png", "Slide2.png", "Slide1.png"]
                .into_iter()
                .map(|name| {
                    let path = request.target.join(name);
                    std::fs::write(&path, tiny_png())?;
                    Ok(path)
                })
                .collect::<Result<Vec<PathBuf>>>()?;
            Ok(OfficeEnhanceOutput::Images(paths))
        })
    }
}

#[derive(Clone)]
struct PptxSource(Vec<u8>);

impl VisualDocumentSource for PptxSource {
    fn load_document(
        &self,
        _book_id: String,
        _revision: Revision,
    ) -> PreviewFuture<'_, BookDocument> {
        Box::pin(async { anyhow::bail!("not used") })
    }

    fn load_asset(
        &self,
        _book_id: String,
        _asset_id: String,
    ) -> PreviewFuture<'_, VisualAssetPayload> {
        Box::pin(async { anyhow::bail!("not used") })
    }

    fn load_source(
        &self,
        _book_id: String,
        _source_id: String,
        _revision: Revision,
    ) -> PreviewFuture<'_, VisualSourcePayload> {
        let bytes = self.0.clone();
        Box::pin(async move {
            Ok(VisualSourcePayload {
                format: "pptx".to_string(),
                source_kind: "original".to_string(),
                media_type:
                    "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                        .to_string(),
                bytes,
            })
        })
    }
}

#[derive(Clone, Copy)]
struct FakeWordPdf;

impl OfficeEnhancer for FakeWordPdf {
    fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
        Box::pin(async move {
            assert_eq!(request.kind, OfficeEnhancementKind::WordPdf);
            std::fs::write(&request.target, test_pdf())?;
            Ok(OfficeEnhanceOutput::Pdf(request.target))
        })
    }
}

#[derive(Clone)]
struct DocxSource(Vec<u8>);

impl VisualDocumentSource for DocxSource {
    fn load_document(
        &self,
        _book_id: String,
        _revision: Revision,
    ) -> PreviewFuture<'_, BookDocument> {
        Box::pin(async { anyhow::bail!("not used") })
    }

    fn load_source(
        &self,
        _book_id: String,
        _source_id: String,
        _revision: Revision,
    ) -> PreviewFuture<'_, VisualSourcePayload> {
        let bytes = self.0.clone();
        Box::pin(async move {
            Ok(VisualSourcePayload {
                format: "docx".to_string(),
                source_kind: "original".to_string(),
                media_type:
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                        .to_string(),
                bytes,
            })
        })
    }
}

#[test]
fn powerpoint_pages_are_naturally_ordered_and_keep_native_locators() {
    let original_asset = AssetRef::new(
        "asset-original",
        vec![AssetRole::OriginalSource],
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("slides.pptx".to_string()),
        4,
        blake3::hash(b"pptx").to_hex().to_string(),
    );
    let mut document = BookDocument::new(
        "book-office",
        "Slides",
        BookSource::imported(
            BookFormat::Pptx,
            original_asset.id.clone(),
            Some("slides.pptx".into()),
        ),
    );
    document.assets.push(original_asset);
    for index in 1..=3 {
        document.units.push(
            ContentUnit::new(
                format!("unit-{index}"),
                ContentUnitKind::Slide,
                format!("Slide {index}"),
                String::new(),
                BlockDocument::default(),
            )
            .with_source_locator(SourceLocator::slide(index)),
        );
    }
    document.validate().unwrap();
    let renderer = OfficeEnhancedRenderer::new(Arc::new(FakeSlides));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let pages = runtime
        .block_on(renderer.render_units(
            RenderUnitsRequest {
                document: Arc::new(document),
                source_id: "source-office".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile::default(),
                asset_source: Some(Arc::new(PptxSource(b"pptx".to_vec()))),
            },
            RenderControl::default(),
        ))
        .unwrap();

    assert_eq!(pages.len(), 3);
    assert!(pages.iter().enumerate().all(|(index, page)| {
        page.page_index == index
            && page.content_unit_id.as_deref() == Some(format!("unit-{}", index + 1).as_str())
            && page.metadata.fidelity == RenderFidelity::OfficeEnhanced
            && page.metadata.renderer == OFFICE_ENHANCED_RENDERER_NAME
            && page.locator.source == Some(SourceLocator::slide((index + 1) as u32))
    }));
}

#[test]
fn word_pdf_is_rasterized_to_png_with_office_metadata() {
    let original_asset = AssetRef::new(
        "asset-original-docx",
        vec![AssetRole::OriginalSource],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("document.docx".to_string()),
        4,
        blake3::hash(b"docx").to_hex().to_string(),
    );
    let mut document = BookDocument::new(
        "book-word",
        "Word",
        BookSource::imported(
            BookFormat::Docx,
            original_asset.id.clone(),
            Some("document.docx".into()),
        ),
    );
    document.assets.push(original_asset);
    document.units.push(
        ContentUnit::new(
            "unit-word-1",
            ContentUnitKind::Chapter,
            "Section 1",
            String::new(),
            BlockDocument::default(),
        )
        .with_source_locator(SourceLocator::office_section(1)),
    );
    document.units.push(
        ContentUnit::new(
            "unit-word-2",
            ContentUnitKind::Chapter,
            "Section 2",
            String::new(),
            BlockDocument::default(),
        )
        .with_source_locator(SourceLocator::office_section(2)),
    );
    document.validate().unwrap();
    let renderer = OfficeEnhancedRenderer::new(Arc::new(FakeWordPdf));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let pages = runtime
        .block_on(renderer.render_units(
            RenderUnitsRequest {
                document: Arc::new(document),
                source_id: "source-word".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile::default(),
                asset_source: Some(Arc::new(DocxSource(b"docx".to_vec()))),
            },
            RenderControl::default(),
        ))
        .unwrap();

    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].content_unit_id, None);
    assert_eq!(pages[0].metadata.unit_revision, Revision::INITIAL);
    assert_eq!(pages[0].media_type, "image/png");
    assert!(pages[0].bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(pages[0].metadata.fidelity, RenderFidelity::OfficeEnhanced);
    assert_eq!(
        pages[0].locator.source,
        Some(SourceLocator::office_rendered_page(1))
    );
}

#[test]
fn repaginated_office_pdf_uses_a_distinct_rendered_page_locator() {
    assert_eq!(
        office_pdf_page_locator(2),
        SourceLocator::office_rendered_page(2)
    );
    assert_eq!(
        office_pdf_page_locator(1),
        SourceLocator::office_rendered_page(1)
    );
}

fn tiny_png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

fn test_pdf() -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("Office PDF")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}
