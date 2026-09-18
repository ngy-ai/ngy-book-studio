//! Shared fixtures for the export tests.

mod epub_test;

mod write_test;

use std::{fs, sync::Arc};

use image::{DynamicImage, ImageFormat};

use super::*;
use crate::document::{
    AssetRef, BlockDocument, BookFormat, ContentUnit, ContentUnitKind, TableCell, TocNode,
    TocTarget,
};

#[derive(Default)]
struct MemoryAssets(HashMap<String, Vec<u8>>);

impl AssetResolver for MemoryAssets {
    fn resolve(&self, asset_id: &str) -> Result<Vec<u8>> {
        self.0
            .get(asset_id)
            .cloned()
            .with_context(|| format!("missing test asset {asset_id}"))
    }
}

fn sample() -> (BookDocument, MemoryAssets, Vec<u8>) {
    let original = b"byte-exact original document\0\xff".to_vec();
    let original_ref = AssetRef::from_bytes(
        AssetRole::OriginalSource,
        "application/octet-stream",
        Some("original.docx".to_string()),
        &original,
    );
    let mut image_bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgba8(2, 1)
        .write_to(&mut image_bytes, ImageFormat::Png)
        .expect("encode PNG fixture");
    let image_bytes = image_bytes.into_inner();
    let mut image_ref = AssetRef::from_bytes(
        AssetRole::Cover,
        "image/png",
        Some("cover.png".to_string()),
        &image_bytes,
    );
    image_ref.add_role(AssetRole::ContentImage);

    let paragraph = Block::Paragraph {
        id: "paragraph-1".to_string(),
        content: vec![
            Inline::text("中文测试 & normalized export"),
            Inline::Image {
                asset_id: image_ref.id.clone(),
                alt: "插图".to_string(),
                title: None,
            },
        ],
    };
    let raw = Block::RawHtml {
        id: "raw-1".to_string(),
        source: "<script>alert(1)</script><em>保留文字</em>".to_string(),
        plain_text: "保留文字".to_string(),
    };
    let unit = ContentUnit::new(
        "unit-1",
        ContentUnitKind::Chapter,
        "第一章",
        "<h1>第一章</h1>",
        BlockDocument::new(vec![paragraph, raw]),
    );
    let mut document = BookDocument::new(
        "book-1",
        "导出测试",
        BookSource::imported(
            BookFormat::Docx,
            original_ref.id.clone(),
            Some("original.docx".to_string()),
        ),
    );
    document.authors = vec!["墨页".to_string()];
    document.language = Some("zh-CN".to_string());
    document.cover_asset_id = Some(image_ref.id.clone());
    document.units = vec![unit];
    document.toc = vec![TocNode::new("toc-1", "第一章", TocTarget::unit("unit-1"))];
    document.assets = vec![original_ref.clone(), image_ref.clone()];

    let assets = MemoryAssets(HashMap::from([
        (original_ref.id, original.clone()),
        (image_ref.id, image_bytes),
    ]));
    (document, assets, original)
}

#[test]
fn original_export_is_byte_exact_and_atomically_replaces_target() {
    let (document, assets, original) = sample();
    let directory = tempfile::tempdir().expect("temporary export directory");
    let target = directory.path().join("original.docx");
    fs::write(&target, b"older complete file").expect("seed target");

    BuiltinDocumentExporter
        .export(&document, ExportFormat::Original, &assets, &target)
        .expect("export original");

    assert_eq!(fs::read(target).expect("read export"), original);
}

#[test]
fn failed_asset_integrity_check_leaves_existing_target_unchanged() {
    let (document, mut assets, _) = sample();
    let original_id = match &document.source {
        BookSource::Imported {
            original_asset_id, ..
        } => original_asset_id,
        BookSource::Created => unreachable!(),
    };
    assets.0.insert(original_id.clone(), b"tampered".to_vec());
    let directory = tempfile::tempdir().expect("temporary export directory");
    let target = directory.path().join("original.docx");
    fs::write(&target, b"keep me").expect("seed target");

    let error = BuiltinDocumentExporter
        .export(&document, ExportFormat::Original, &assets, &target)
        .expect_err("integrity mismatch must fail");

    assert!(error.to_string().contains("长度") || error.to_string().contains("BLAKE3"));
    assert_eq!(fs::read(target).expect("read old target"), b"keep me");
}

#[test]
fn resolver_closure_is_supported() {
    let bytes = Arc::new(b"asset".to_vec());
    let captured = Arc::clone(&bytes);
    let resolver = move |_: &str| Ok(captured.as_ref().clone());
    assert_eq!(resolver.resolve("ignored").expect("resolve"), *bytes);
}
