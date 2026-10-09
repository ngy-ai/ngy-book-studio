//! Asset identity belongs to the book that owns the bytes.
//!
//! `assets.id` is the primary key of a library-wide table, so two books may
//! legitimately hold the very same bytes — the volumes of one series reusing a
//! stylesheet or a cover — and each still needs its own row.

use std::{
    fs::File,
    io::{Cursor, Write as _},
    path::Path,
};

use super::*;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// The stylesheet every volume of the fixture series carries at the same href
/// with byte-identical content.
const SHARED_STYLESHEET: &[u8] = b"body { font-family: serif; }";

#[test]
fn two_books_that_share_a_resource_keep_their_own_asset_rows() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("series-vol1.epub");
    let second_path = temp.path().join("series-vol2.epub");
    write_shared_stylesheet_epub(&first_path, "系列第一卷", "第一卷的正文。");
    write_shared_stylesheet_epub(&second_path, "系列第二卷", "第二卷的正文。");

    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let first = added(&mut library, &first_path);
    let second = added(&mut library, &second_path);
    assert_ne!(first.id, second.id, "the fixture must be two books");

    let first_asset = stylesheet_asset_id(&library, &first.id);
    let second_asset = stylesheet_asset_id(&library, &second.id);
    assert_ne!(
        first_asset, second_asset,
        "the same resource in two books must not share one asset row"
    );
    for (book_id, asset_id) in [(&first.id, &first_asset), (&second.id, &second_asset)] {
        assert_eq!(
            library.asset_bytes(book_id, asset_id).unwrap(),
            SHARED_STYLESHEET
        );
    }
}

#[test]
fn the_same_cover_bytes_can_belong_to_two_books() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let first = library.create_book("封面甲", "作者").unwrap();
    let second = library.create_book("封面乙", "作者").unwrap();

    let cover = cover_png();
    for book_id in [&first.id, &second.id] {
        library
            .set_cover(book_id, CoverDraft::from_bytes(cover.clone()).unwrap())
            .unwrap();
    }
    for book_id in [&first.id, &second.id] {
        assert_eq!(
            library.cover_bytes(book_id).unwrap().unwrap().as_ref(),
            &cover
        );
    }
}

fn added(library: &mut LibraryStore, path: &Path) -> BookRecord {
    match library.import(path).unwrap() {
        ImportOutcome::Added(book) => book,
        ImportOutcome::AlreadyExists(book) => panic!(
            "{} must import as a new book instead of reusing {}",
            path.display(),
            book.id
        ),
    }
}

fn stylesheet_asset_id(library: &LibraryStore, book_id: &str) -> String {
    library
        .document(book_id)
        .unwrap()
        .assets
        .iter()
        .find(|asset| asset.roles.contains(&AssetRole::Stylesheet))
        .expect("the fixture declares exactly one stylesheet asset")
        .id
        .clone()
}

fn cover_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([185, 95, 66, 255]),
    ))
    .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
    .unwrap();
    bytes
}

/// Writes a minimal EPUB whose only resource is a stylesheet at a fixed href.
/// Two of these files are two books that share those exact resource bytes.
fn write_shared_stylesheet_epub(path: &Path, title: &str, paragraph: &str) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    add_file(&mut zip, "mimetype", b"application/epub+zip", stored);
    add_file(
        &mut zip,
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        stored,
    );
    let package = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:uuid:{title}</dc:identifier>
    <dc:title>{title}</dc:title><dc:language>zh-CN</dc:language>
    <meta property="dcterms:modified">2026-09-24T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="style" href="styles/shared.css" media-type="text/css"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#
    );
    add_file(&mut zip, "OPS/package.opf", package.as_bytes(), stored);
    let chapter = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title>
<link rel="stylesheet" type="text/css" href="../styles/shared.css"/></head>
<body><h1>{title}</h1><p>{paragraph}</p></body></html>"#
    );
    add_file(
        &mut zip,
        "OPS/text/chapter.xhtml",
        chapter.as_bytes(),
        stored,
    );
    add_file(&mut zip, "OPS/styles/shared.css", SHARED_STYLESHEET, stored);
    zip.finish().unwrap();
}

fn add_file(zip: &mut ZipWriter<File>, path: &str, bytes: &[u8], options: SimpleFileOptions) {
    zip.start_file(path, options).unwrap();
    zip.write_all(bytes).unwrap();
}
