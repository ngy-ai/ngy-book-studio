//! Tests for the import limits, registry and asset rewriting.

use super::*;

struct NeverImporter;

impl DocumentImporter for NeverImporter {
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            importer: "never",
            formats: &[BookFormat::Epub],
            parser_version: "test",
        }
    }

    fn probe(&self, _: &ImportSource) -> ProbeResult {
        ProbeResult::no_match(BookFormat::Epub)
    }

    fn import(&self, _: &ImportSource, _: &ImportLimits) -> Result<ImportedBook> {
        unreachable!()
    }
}

#[test]
fn registry_rejects_unknown_bytes() {
    let mut registry = FormatRegistry::default();
    registry.register(NeverImporter);
    let source = ImportSource {
        file_name: Some("unknown.bin".into()),
        bytes: Arc::new(vec![1, 2, 3]),
    };
    assert!(registry.import(&source, &ImportLimits::default()).is_err());
}

#[test]
fn original_asset_is_content_addressed_and_byte_exact() {
    let source = ImportSource {
        file_name: Some("book.epub".into()),
        bytes: Arc::new(b"source bytes".to_vec()),
    };
    let asset = original_asset(&source, BookFormat::Epub, "application/epub+zip");
    assert_eq!(asset.bytes.as_slice(), b"source bytes");
    assert!(asset.metadata.roles.contains(&AssetRole::OriginalSource));
    assert_eq!(asset.metadata.byte_len, 12);
}

fn imported_book_with_asset_sizes(sizes: &[usize]) -> ImportedBook {
    let assets = sizes
        .iter()
        .enumerate()
        .map(|(index, size)| {
            asset_from_bytes(
                &format!("budget-asset-{index}"),
                vec![AssetRole::ContentImage],
                "image/png",
                Some(format!("asset-{index}.png")),
                Arc::new(vec![u8::try_from(index).unwrap_or(u8::MAX); *size]),
            )
        })
        .collect::<Vec<_>>();
    let mut document = BookDocument::created("budget-book", "Budget book");
    document.assets = assets.iter().map(|asset| asset.metadata.clone()).collect();
    ImportedBook { document, assets }
}

fn asset_limits(max_assets: usize, max_asset_bytes: u64, total_bytes: u64) -> ImportLimits {
    ImportLimits {
        max_assets,
        max_asset_bytes,
        max_total_asset_bytes: total_bytes,
        ..ImportLimits::default()
    }
}

#[test]
fn imported_asset_budget_accepts_exact_boundaries() {
    imported_book_with_asset_sizes(&[2, 3])
        .validate(&asset_limits(2, 3, 5))
        .expect("exact asset limits should be accepted");
}

#[test]
fn imported_asset_budget_rejects_too_many_assets() {
    let error = imported_book_with_asset_sizes(&[1, 1, 1])
        .validate(&asset_limits(2, 8, 8))
        .expect_err("asset count above the limit must be rejected");
    assert!(error.to_string().contains("asset count safety limit"));
}

#[test]
fn imported_asset_budget_rejects_one_oversized_asset() {
    let error = imported_book_with_asset_sizes(&[4])
        .validate(&asset_limits(1, 3, 8))
        .expect_err("single asset above the limit must be rejected");
    assert!(error.to_string().contains("single-asset safety limit"));
}

#[test]
fn imported_asset_budget_rejects_aggregate_overflow() {
    let error = imported_book_with_asset_sizes(&[2, 3])
        .validate(&asset_limits(2, 3, 4))
        .expect_err("aggregate asset bytes above the limit must be rejected");
    assert!(error.to_string().contains("aggregate asset safety limit"));
}

#[test]
fn import_path_applies_the_size_limit_before_and_during_read() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("oversized.bin");
    fs::write(&path, b"123456789").unwrap();
    let limits = ImportLimits {
        max_source_bytes: 8,
        ..ImportLimits::default()
    };
    let error = ImportSource::from_path(&path, &limits).unwrap_err();
    assert!(error.to_string().contains("8 byte safety limit"));
}

#[test]
fn import_path_applies_original_asset_limits_before_reading() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("oversized-asset.bin");
    fs::write(&path, b"123456789").unwrap();
    let limits = ImportLimits {
        max_source_bytes: 16,
        max_asset_bytes: 8,
        max_total_asset_bytes: 16,
        ..ImportLimits::default()
    };
    let error = ImportSource::from_path(&path, &limits).unwrap_err();
    assert!(error.to_string().contains("single-asset safety limit"));
}

#[test]
fn imported_html_media_urls_resolve_against_the_chapter_and_become_asset_ids() {
    let assets = HashMap::from([
        ("OPS/media/picture.png".to_string(), "image-id".to_string()),
        ("OPS/media/movie.mp4".to_string(), "video-id".to_string()),
        ("OPS/media/poster.png".to_string(), "poster-id".to_string()),
        ("OPS/media/sound.mp3".to_string(), "audio-id".to_string()),
    ]);
    let rewritten = rewrite_imported_html_assets(
        r#"<html><body>
                <img src="../media/picture.png?size=2#part" alt="cover">
                <audio><source src="%2e%2e/media/sound.mp3"></audio>
                <video src="../media/movie.mp4" poster="/OPS/media/poster.png"></video>
                <img src="../../../outside.png">
                <img src="https://example.invalid/tracker.png">
                <img src="../media/not-in-manifest.png">
            </body></html>"#,
        "OPS/text/chapter.xhtml",
        &assets,
    )
    .expect("rewrite imported media");

    assert!(rewritten.contains("ngy-asset:image-id"));
    assert!(rewritten.contains("ngy-asset:audio-id"));
    assert!(rewritten.contains("ngy-asset:video-id"));
    assert!(rewritten.contains("ngy-asset:poster-id"));
    assert!(!rewritten.contains("outside.png"));
    assert!(!rewritten.contains("tracker.png"));
    assert!(!rewritten.contains("not-in-manifest.png"));

    let parsed = crate::markup::parse_source_for_unit(&rewritten, "imported-unit")
        .expect("parse rewritten HTML into the canonical AST");
    let mut referenced = parsed.document.referenced_asset_ids();
    referenced.sort_unstable();
    assert_eq!(
        referenced,
        vec!["audio-id", "image-id", "poster-id", "video-id"]
    );
}

#[test]
fn archive_href_normalization_rejects_encoded_root_escape_and_backslashes() {
    assert_eq!(
        normalized_archive_href("/OPS/images/%70icture.png?x=1#fragment"),
        Some("OPS/images/picture.png".to_string())
    );
    assert_eq!(
        normalized_archive_href("OPS/text/../../cover.png"),
        Some("cover.png".into())
    );
    assert_eq!(normalized_archive_href("%2e%2e/secret.png"), None);
    assert_eq!(normalized_archive_href("OPS\\images\\picture.png"), None);
}
