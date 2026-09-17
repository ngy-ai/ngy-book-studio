//! Tests for the structural renderer and its raster limits.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_renderer_publishes_deterministic_png_and_is_interruptible() {
    let renderer = StructuralPngRenderer;
    let request = RenderUnitsRequest {
        document: Arc::new(sample_document()),
        source_id: "source-test".to_string(),
        unit_ids: Vec::new(),
        profile: RenderProfile::default(),
        asset_source: None,
    };
    let first = renderer
        .render_units(request.clone(), RenderControl::default())
        .await
        .expect("render PNG");
    let second = renderer
        .render_units(request, RenderControl::default())
        .await
        .expect("repeat PNG");
    assert_eq!(first, second);
    assert_eq!(first[0].media_type, "image/png");
    assert!(first[0].bytes.starts_with(PNG_SIGNATURE));
    let decoded = image::load_from_memory_with_format(&first[0].bytes, image::ImageFormat::Png)
        .expect("valid structural PNG");
    assert_eq!(decoded.width(), first[0].width);
    assert_eq!(decoded.height(), first[0].height);
    let svg = render_svg_page(
        &["包含 <script>、& 和中文。".to_string()],
        &RenderProfile::default(),
    );
    assert!(svg.contains("&lt;script&gt;"));
    assert!(!svg.contains("<script>"));
    assert!(svg.contains("&amp;"));
    assert!(svg.contains("中文"));
    assert_eq!(first[0].metadata.document_revision, Revision::new(3));

    let control = RenderControl::default();
    control.request_pause();
    let error = renderer
        .render_units(
            RenderUnitsRequest {
                document: Arc::new(sample_document()),
                source_id: "source-test".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile::default(),
                asset_source: None,
            },
            control,
        )
        .await
        .expect_err("paused render must stop");
    assert_eq!(
        error.downcast_ref::<RenderInterrupted>(),
        Some(&RenderInterrupted::Paused)
    );
}

#[tokio::test]
async fn structural_renderer_adds_owned_image_pages_with_block_locators() {
    let bytes = png_fixture(2, 3);
    let asset = AssetRef {
        id: "asset-image".to_string(),
        roles: vec![AssetRole::ContentImage],
        media_type: "image/png".to_string(),
        original_file_name: Some("picture.png".to_string()),
        byte_len: bytes.len() as u64,
        content_hash: blake3::hash(&bytes).to_hex().to_string(),
    };
    let mut document = BookDocument::created("book-image", "图片视觉测试");
    document.assets.push(asset);
    document.units.push(ContentUnit::new(
        "unit-image",
        ContentUnitKind::Chapter,
        "图片章节",
        "<img src=\"ngy-asset:asset-image\" alt=\"说明\">",
        BlockDocument::new(vec![Block::Image {
            id: "block-image".to_string(),
            asset_id: "asset-image".to_string(),
            alt: "说明".to_string(),
            title: None,
            caption: Vec::new(),
        }]),
    ));
    document.validate().expect("valid image document");
    let source: Arc<dyn VisualDocumentSource> = Arc::new(ImageDocumentSource {
        document: document.clone(),
        payload: VisualAssetPayload {
            media_type: "image/png".to_string(),
            bytes: bytes.clone(),
        },
    });

    let pages = StructuralPngRenderer
        .render_units(
            RenderUnitsRequest {
                document: Arc::new(document),
                source_id: "source-image".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile::default(),
                asset_source: Some(source),
            },
            RenderControl::default(),
        )
        .await
        .expect("render text and image pages");

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1].media_type, "image/png");
    assert_eq!(pages[1].bytes, bytes);
    assert_eq!(
        pages[1].locator,
        DocumentLocator::block("book-image", "unit-image", "block-image")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_slide_merges_text_and_images_into_exactly_one_page() {
    let bytes = png_fixture(40, 30);
    let asset = AssetRef {
        id: "asset-image".to_string(),
        roles: vec![AssetRole::ContentImage],
        media_type: "image/png".to_string(),
        original_file_name: Some("slide.png".to_string()),
        byte_len: bytes.len() as u64,
        content_hash: blake3::hash(&bytes).to_hex().to_string(),
    };
    let mut document = BookDocument::created("book-slide", "幻灯片视觉测试");
    document.assets.push(asset);
    document.units.push(
        ContentUnit::new(
            "unit-slide",
            ContentUnitKind::Slide,
            "第三张幻灯片",
            "slide source",
            BlockDocument::new(vec![
                Block::paragraph("block-slide-text", "一段需要和图片位于同一页的文字。"),
                Block::Image {
                    id: "block-image".to_string(),
                    asset_id: "asset-image".to_string(),
                    alt: "示意图".to_string(),
                    title: None,
                    caption: Vec::new(),
                },
            ]),
        )
        .with_source_locator(SourceLocator::slide(3)),
    );
    document.validate().expect("valid slide document");
    let source: Arc<dyn VisualDocumentSource> = Arc::new(ImageDocumentSource {
        document: document.clone(),
        payload: VisualAssetPayload {
            media_type: "image/png".to_string(),
            bytes: bytes.clone(),
        },
    });

    let pages = StructuralPngRenderer
        .render_units(
            RenderUnitsRequest {
                document: Arc::new(document.clone()),
                source_id: "source-slide".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile {
                    viewport_width: 640,
                    viewport_height: 480,
                    ..RenderProfile::default()
                },
                asset_source: Some(source),
            },
            RenderControl::default(),
        )
        .await
        .expect("render one composite slide");

    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].media_type, "image/png");
    assert_ne!(pages[0].bytes, bytes, "the page must be a composite PNG");
    assert_eq!(pages[0].locator.source, Some(SourceLocator::slide(3)));
    assert_eq!(
        pages[0].locator.block_id.as_deref(),
        Some("block-slide-text")
    );
    document
        .validate_locator(&pages[0].locator)
        .expect("composite slide locator is valid");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_worksheet_uses_two_dimensional_a1_page_ranges() {
    let header = TableRow::new(
        (0..7)
            .map(|column| TableCell::text(format!("H{column}")))
            .collect(),
    );
    let rows = (1..6)
        .map(|row| {
            TableRow::new(
                (0..7)
                    .map(|column| TableCell::text(format!("R{row}C{column}")))
                    .collect(),
            )
        })
        .collect();
    let mut document = BookDocument::created("book-sheet", "工作表视觉测试");
    document.units.push(
        ContentUnit::new(
            "unit-sheet",
            ContentUnitKind::Worksheet,
            "Sales",
            "table source",
            BlockDocument::new(vec![Block::Table {
                id: "block-table".to_string(),
                header: Some(header),
                rows,
            }]),
        )
        .with_source_locator(SourceLocator::worksheet(
            "Sales",
            Some("C5:I10".to_string()),
        )),
    );
    document.validate().expect("valid worksheet document");

    let pages = StructuralPngRenderer
        .render_units(
            RenderUnitsRequest {
                document: Arc::new(document.clone()),
                source_id: "source-sheet".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile {
                    viewport_width: 500,
                    viewport_height: 400,
                    ..RenderProfile::default()
                },
                asset_source: None,
            },
            RenderControl::default(),
        )
        .await
        .expect("render worksheet tiles");

    let ranges = pages
        .iter()
        .map(|page| match page.locator.source.as_ref() {
            Some(SourceLocator::Worksheet {
                name,
                range: Some(range),
            }) => {
                assert_eq!(name, "Sales");
                range.as_str()
            }
            other => panic!("unexpected worksheet locator: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges, ["C5:D10", "E5:F10", "G5:H10", "I5:I10"]);
    assert!(pages.iter().all(|page| {
        page.locator.block_id.as_deref() == Some("block-table")
            && page.locator.text_range.is_none()
            && page.bytes.starts_with(PNG_SIGNATURE)
    }));
    for page in &pages {
        document
            .validate_locator(&page.locator)
            .expect("worksheet tile locator is valid");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_text_pages_retain_block_utf8_ranges() {
    let text = "中文段落".repeat(160);
    let mut document = BookDocument::created("book-ranges", "文本定位测试");
    document.units.push(
        ContentUnit::new(
            "unit-ranges",
            ContentUnitKind::Chapter,
            "章节",
            text.clone(),
            BlockDocument::new(vec![Block::paragraph("block-ranges", text)]),
        )
        .with_source_locator(SourceLocator::created()),
    );
    document.validate().expect("valid ranged document");

    let pages = StructuralPngRenderer
        .render_units(
            RenderUnitsRequest {
                document: Arc::new(document.clone()),
                source_id: "source-ranges".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile {
                    viewport_width: 320,
                    viewport_height: 320,
                    ..RenderProfile::default()
                },
                asset_source: None,
            },
            RenderControl::default(),
        )
        .await
        .expect("render ranged text pages");

    assert!(pages.len() > 1);
    let mut previous_end = 0;
    for page in &pages {
        assert_eq!(page.locator.block_id.as_deref(), Some("block-ranges"));
        assert_eq!(page.locator.source, Some(SourceLocator::created()));
        let range = page.locator.text_range.expect("page text range");
        assert!(range.end_byte > range.start_byte);
        assert!(range.start_byte >= previous_end);
        previous_end = range.end_byte;
        document
            .validate_locator(&page.locator)
            .expect("text page locator is valid");
    }
}

#[test]
fn supported_raster_image_pages_keep_their_original_bytes() {
    let rgba = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        3,
        image::Rgba([10, 20, 30, 255]),
    ));
    let rgb =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 3, image::Rgb([10, 20, 30])));
    for (media_type, format, image) in [
        ("image/png", image::ImageFormat::Png, &rgba),
        ("image/jpeg", image::ImageFormat::Jpeg, &rgb),
        ("image/webp", image::ImageFormat::WebP, &rgba),
        ("image/gif", image::ImageFormat::Gif, &rgba),
    ] {
        let mut source = Cursor::new(Vec::new());
        image
            .write_to(&mut source, format)
            .expect("encode raster fixture");
        let source = source.into_inner();
        let prepared = prepare_visual_image(
            VisualAssetPayload {
                media_type: media_type.to_string(),
                bytes: source.clone(),
            },
            media_type,
            &RenderProfile::default(),
            &RenderControl::default(),
        )
        .expect("validate raster page");
        assert_eq!(prepared.media_type, media_type);
        assert_eq!(prepared.bytes, source);
        assert_eq!((prepared.width, prepared.height), (2, 3));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_renderer_rasterizes_svg_assets_before_publication() {
    let bytes = r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32" fill="white"/><text x="2" y="20">中文 &amp; safe</text></svg>"#
            .as_bytes()
            .to_vec();
    let asset = AssetRef {
        id: "asset-image".to_string(),
        roles: vec![AssetRole::ContentImage],
        media_type: "image/svg+xml".to_string(),
        original_file_name: Some("picture.svg".to_string()),
        byte_len: bytes.len() as u64,
        content_hash: blake3::hash(&bytes).to_hex().to_string(),
    };
    let mut document = BookDocument::created("book-image", "SVG 图片视觉测试");
    document.assets.push(asset);
    document.units.push(ContentUnit::new(
        "unit-image",
        ContentUnitKind::Chapter,
        "SVG 图片章节",
        "<img src=\"ngy-asset:asset-image\" alt=\"说明\">",
        BlockDocument::new(vec![Block::Image {
            id: "block-image".to_string(),
            asset_id: "asset-image".to_string(),
            alt: "说明".to_string(),
            title: None,
            caption: Vec::new(),
        }]),
    ));
    document.validate().expect("valid SVG image document");
    let source: Arc<dyn VisualDocumentSource> = Arc::new(ImageDocumentSource {
        document: document.clone(),
        payload: VisualAssetPayload {
            media_type: "image/svg+xml".to_string(),
            bytes,
        },
    });

    let pages = StructuralPngRenderer
        .render_units(
            RenderUnitsRequest {
                document: Arc::new(document),
                source_id: "source-image".to_string(),
                unit_ids: Vec::new(),
                profile: RenderProfile::default(),
                asset_source: Some(source),
            },
            RenderControl::default(),
        )
        .await
        .expect("rasterize SVG asset");

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1].media_type, "image/png");
    assert!(pages[1].bytes.starts_with(PNG_SIGNATURE));
    assert_eq!((pages[1].width, pages[1].height), (64, 32));
}

#[test]
fn svg_rasterization_disables_external_resources_and_rejects_size_bypasses() {
    let external = br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image width="32" height="32" href="https://example.invalid/tracker.png"/></svg>"#;
    let rendered = rasterize_svg(
        external,
        &RenderProfile::default(),
        false,
        &RenderControl::default(),
    )
    .expect("ignore an external image without fetching it");
    let decoded = image::load_from_memory_with_format(&rendered.bytes, image::ImageFormat::Png)
        .expect("valid resource-isolated PNG")
        .to_rgba8();
    assert!(decoded.pixels().all(|pixel| pixel.0[3] == 0));

    let oversized = vec![b' '; MAX_STRUCTURAL_SVG_BYTES + 1];
    assert!(
        rasterize_svg(
            &oversized,
            &RenderProfile::default(),
            false,
            &RenderControl::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("字节上限")
    );
    assert!(
        rasterize_svg(
            &[0x1f, 0x8b, 0x08, 0x00],
            &RenderProfile::default(),
            false,
            &RenderControl::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("压缩 SVG")
    );
}

#[test]
fn raster_limits_bound_dimensions_pixels_and_total_output() {
    let profile = RenderProfile {
        viewport_width: 16_384,
        viewport_height: 16_384,
        scale_milli: 8_000,
        ..RenderProfile::default()
    };
    let (width, height) = fitted_raster_dimensions(16_384.0, 16_384.0, &profile, true)
        .expect("fit an extreme valid profile");
    assert!(width <= MAX_RASTER_DIMENSION);
    assert!(height <= MAX_RASTER_DIMENSION);
    assert!(u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS);
    assert!(validate_raster_dimensions(MAX_RASTER_DIMENSION + 1, 1).is_err());

    let mut total = MAX_VISUAL_BATCH_BYTES;
    assert!(add_visual_page_bytes(&mut total, 1).is_err());

    let oversized_raster = png_fixture(MAX_RASTER_DIMENSION + 1, 1);
    assert!(
        prepare_visual_image(
            VisualAssetPayload {
                media_type: "image/png".to_string(),
                bytes: oversized_raster,
            },
            "image/png",
            &RenderProfile::default(),
            &RenderControl::default(),
        )
        .is_err()
    );
}
