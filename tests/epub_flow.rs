use std::{
    collections::HashMap,
    fs::File,
    io::{Cursor, Read, Write},
    path::Path,
};

use image::ImageEncoder as _;
use moye_epub_editor::{
    document::{AssetRef, AssetRole, SourceKind},
    library::{CoverDraft, ImportOutcome, LibraryStore},
    media::MediaService,
    reader::{OpenedBook, ReaderResourceAuthorizations, load_resource, load_resource_with_range},
    storage::BlobKey,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

#[test]
fn imports_reads_and_restores_a_real_epub() {
    let temp = tempfile::tempdir().unwrap();
    let epub_path = temp.path().join("sample.epub");
    write_sample_epub(&epub_path);

    let library_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(library_dir.clone()).unwrap();
    let imported = match library.import(&epub_path).unwrap() {
        ImportOutcome::Added(book) => book,
        ImportOutcome::AlreadyExists(_) => panic!("first import must add the book"),
    };

    assert_eq!(imported.title, "山海小记");
    assert_eq!(imported.author, "测试作者");
    // SQLite stores only metadata; source and cover bytes live in the managed
    // content-addressed object directory.
    assert!(!library_dir.join("books").exists());
    assert!(library.epub_bytes(&imported.id).unwrap().len() > 100);
    assert_eq!(imported.cover_mime.as_deref(), Some("image/png"));
    assert!(library.cover_bytes(&imported.id).unwrap().is_some());
    assert!(matches!(
        library.import(&epub_path).unwrap(),
        ImportOutcome::AlreadyExists(_)
    ));

    let epub_bytes = library.epub_bytes(&imported.id).unwrap();
    let opened = OpenedBook::open_bytes(epub_bytes).unwrap();
    assert_eq!(opened.spine.len(), 2);
    assert_eq!(opened.toc.len(), 3);
    assert_eq!(opened.toc[0].label, "上篇");
    assert!(opened.toc[0].href.is_none());
    assert_eq!(opened.toc[1].label, "第一章");
    assert_eq!(opened.toc[2].label, "第二章");

    let chapter = load_resource(&opened.epub, "/EPUB/chapter-1.xhtml").unwrap();
    assert_eq!(chapter.mime, "application/xhtml+xml; charset=utf-8");
    let chapter = String::from_utf8(chapter.bytes).unwrap();
    assert!(chapter.contains("山海之间"));
    assert!(chapter.contains("moye-reader-style"));
    assert!(load_resource(&opened.epub, "/META-INF/container.xml").is_err());
    assert_eq!(
        opened.spine_index_for_url("http://epubreader.book/EPUB/chapter-2.xhtml#end"),
        Some(1)
    );

    let second_unit_id = library.document(&imported.id).unwrap().units[1].id.clone();
    library
        .update_progress_at(&imported.id, 1, &second_unit_id)
        .unwrap();
    drop(library);
    let restored = LibraryStore::load_from(library_dir).unwrap();
    assert_eq!(restored.books()[0].last_spine, 1);
}

#[test]
fn reads_an_epub2_ncx_table_of_contents() {
    let temp = tempfile::tempdir().unwrap();
    let epub_path = temp.path().join("sample-epub2.epub");
    write_sample_epub2(&epub_path);

    let opened = OpenedBook::open(&epub_path).unwrap();
    assert_eq!(opened.title, "旧版小书");
    assert_eq!(opened.spine.len(), 2);
    assert_eq!(opened.toc.len(), 2);
    assert_eq!(opened.toc[0].label, "开篇");
    assert_eq!(opened.toc[1].label, "终章");
}

#[test]
fn canonical_edit_adds_a_cover_without_modifying_the_epub2_original() {
    let temp = tempfile::tempdir().unwrap();
    let epub2_path = temp.path().join("sample-epub2.epub");
    write_sample_epub2(&epub2_path);

    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let cover = CoverDraft::from_bytes(valid_test_png()).unwrap();
    let original_bytes = std::fs::read(&epub2_path).unwrap();
    let epub2 = match library.import(&epub2_path).unwrap() {
        ImportOutcome::Added(book) => book,
        ImportOutcome::AlreadyExists(_) => unreachable!(),
    };
    let mut document = library.document(&epub2.id).unwrap();
    let mut cover_asset = AssetRef::from_bytes(
        AssetRole::Cover,
        cover.mime(),
        Some("cover.png".to_string()),
        cover.bytes().as_slice(),
    );
    cover_asset.add_role(AssetRole::ContentImage);
    document.cover_asset_id = Some(cover_asset.id.clone());
    document.assets.push(cover_asset.clone());
    library
        .apply_document_with_assets(
            document,
            HashMap::from([(cover_asset.id, cover.bytes().clone())]),
        )
        .unwrap();

    let bytes = library.epub_bytes(&epub2.id).unwrap();
    let edited = rbook::Epub::read(Cursor::new(bytes)).unwrap();
    assert!(!edited.package().version().is_epub2());
    let cover = edited
        .manifest()
        .cover_image()
        .expect("normalized EPUB cover metadata");
    assert_eq!("image/png", cover.media_type());
    assert!(!cover.read_bytes().unwrap().is_empty());

    let original_export = temp.path().join("original.epub");
    library
        .export_original(&epub2.id, &original_export)
        .unwrap();
    assert_eq!(std::fs::read(original_export).unwrap(), original_bytes);
}

#[test]
fn imported_media_survives_canonical_edit_epub_export_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let epub_path = temp.path().join("media.epub");
    let media = write_media_epub(&epub_path);
    let original_bytes = std::fs::read(&epub_path).unwrap();
    let library_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(library_dir.clone()).unwrap();
    let imported = match library.import(&epub_path).unwrap() {
        ImportOutcome::Added(book) => book,
        ImportOutcome::AlreadyExists(_) => panic!("first import must add the media book"),
    };

    let document = library.document(&imported.id).unwrap();
    let unit = &document.units[0];
    let mut referenced = unit
        .document
        .referenced_asset_ids()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    referenced.sort_unstable();
    referenced.dedup();
    assert_eq!(referenced.len(), 4, "image, audio, video, and poster refs");
    assert!(unit.source.contains("moye-asset:"));
    assert!(!unit.source.contains("../media/"));
    assert!(!unit.source.contains("example.invalid"));
    for expected in &media {
        let hash = blake3::hash(expected).to_hex().to_string();
        assert!(
            document
                .assets
                .iter()
                .any(|asset| { asset.content_hash == hash && referenced.contains(&asset.id) }),
            "every referenced media payload must have a stable canonical asset"
        );
    }

    let unit_id = unit.id.clone();
    let edited_source = format!("{}<p>EditedMediaRoundTrip</p>", unit.source);
    library
        .update_content_unit_source(&imported.id, &unit_id, SourceKind::Html, &edited_source)
        .unwrap();

    let normalized_path = temp.path().join("normalized.epub");
    let original_path = temp.path().join("original.epub");
    library.export_epub(&imported.id, &normalized_path).unwrap();
    library
        .export_original(&imported.id, &original_path)
        .unwrap();
    assert_eq!(std::fs::read(&original_path).unwrap(), original_bytes);

    let normalized_bytes = std::fs::read(&normalized_path).unwrap();
    let normalized = rbook::Epub::read(Cursor::new(normalized_bytes.clone())).unwrap();
    let chapter = normalized
        .spine()
        .iter()
        .filter(|entry| entry.is_linear())
        .find_map(|entry| entry.manifest_entry())
        .unwrap()
        .read_str()
        .unwrap();
    assert!(chapter.contains("EditedMediaRoundTrip"));
    assert_eq!(chapter.matches("../assets/").count(), 4);
    assert!(!chapter.contains("moye-asset:"));

    let mut archive = ZipArchive::new(Cursor::new(normalized_bytes)).unwrap();
    let mut exported_media = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        if entry.name().starts_with("EPUB/assets/") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            exported_media.push(bytes);
        }
    }
    for expected in &media {
        assert!(
            exported_media.iter().any(|bytes| bytes == expected),
            "normalized EPUB must contain every referenced media payload"
        );
    }

    drop(library);
    let reopened = LibraryStore::load_from(library_dir).unwrap();
    let reopened_document = reopened.document(&imported.id).unwrap();
    let reopened_unit = &reopened_document.units[0];
    let mut reopened_refs = reopened_unit
        .document
        .referenced_asset_ids()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    reopened_refs.sort_unstable();
    reopened_refs.dedup();
    assert_eq!(reopened_refs, referenced);
    assert!(reopened_unit.plain_text().contains("EditedMediaRoundTrip"));
}

#[test]
fn reader_media_ranges_are_manifest_scoped_and_read_from_the_owned_object() {
    let temp = tempfile::tempdir().unwrap();
    let epub_path = temp.path().join("media-range.epub");
    let media = write_media_epub(&epub_path);
    let audio = &media[2];
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let imported = match library.import(&epub_path).unwrap() {
        ImportOutcome::Added(book) => book,
        ImportOutcome::AlreadyExists(_) => unreachable!(),
    };
    let opened = OpenedBook::open_bytes(library.reader_epub_bytes(&imported.id).unwrap()).unwrap();
    let document = library.document(&imported.id).unwrap();
    let authorizations =
        ReaderResourceAuthorizations::for_document(&opened.epub, &document).unwrap();
    let authorized = authorizations
        .asset_for_path("/OPS/media/sound.mp3")
        .unwrap()
        .expect("manifest audio must map to its persisted canonical asset");
    assert_eq!(authorized.media_type, "audio/mpeg");
    assert_eq!(authorized.byte_len, audio.len() as u64);
    assert_eq!(
        authorized.content_hash,
        blake3::hash(audio).to_hex().to_string()
    );

    let partial = MediaService::new(library.clone())
        .serve(&imported.id, &authorized.asset_id, Some("bytes=4-9"))
        .unwrap();
    assert_eq!(partial.status, 206);
    assert_eq!(partial.media_type, "audio/mpeg");
    assert_eq!(partial.accept_ranges, "bytes");
    assert_eq!(partial.content_length, 6);
    assert_eq!(
        partial.content_range.as_deref(),
        Some(format!("bytes 4-9/{}", audio.len()).as_str())
    );
    assert_eq!(partial.body, audio[4..10]);
    assert!(
        MediaService::new(library.clone())
            .serve("another-book", &authorized.asset_id, Some("bytes=0-1"))
            .is_err(),
        "an asset ID must not bypass book ownership"
    );

    let archive_partial =
        load_resource_with_range(&opened.epub, "/OPS/media/sound.mp3", Some("bytes=-5")).unwrap();
    assert_eq!(archive_partial.status, 206);
    assert_eq!(archive_partial.mime, "audio/mpeg");
    assert_eq!(archive_partial.accept_ranges, Some("bytes"));
    assert_eq!(
        archive_partial.content_range.as_deref(),
        Some(
            format!(
                "bytes {}-{}/{}",
                audio.len() - 5,
                audio.len() - 1,
                audio.len()
            )
            .as_str()
        )
    );
    assert_eq!(archive_partial.bytes, audio[audio.len() - 5..]);

    let unsatisfiable =
        load_resource_with_range(&opened.epub, "/OPS/media/sound.mp3", Some("bytes=999-1000"))
            .unwrap();
    assert_eq!(unsatisfiable.status, 416);
    assert_eq!(unsatisfiable.content_length, 0);
    assert_eq!(
        unsatisfiable.content_range.as_deref(),
        Some(format!("bytes */{}", audio.len()).as_str())
    );
    assert!(
        load_resource_with_range(
            &opened.epub,
            "/OPS/%2e%2e/META-INF/container.xml",
            Some("bytes=0-1")
        )
        .is_err(),
        "Range handling must not weaken encoded traversal rejection"
    );
    assert!(
        authorizations
            .asset_for_path("/META-INF/container.xml")
            .unwrap()
            .is_none(),
        "non-manifest application internals must not become media routes"
    );

    let unit = &document.units[0];
    let edited_source = format!("{}<p>range route revision</p>", unit.source);
    library
        .update_content_unit_source(&imported.id, &unit.id, SourceKind::Html, &edited_source)
        .unwrap();
    let normalized =
        OpenedBook::open_bytes(library.reader_epub_bytes(&imported.id).unwrap()).unwrap();
    let normalized_document = library.document(&imported.id).unwrap();
    let normalized_audio_href = normalized
        .epub
        .manifest()
        .iter()
        .find(|entry| entry.media_type() == "audio/mpeg")
        .expect("normalized EPUB audio manifest entry")
        .href()
        .as_str()
        .to_string();
    let normalized_audio_path = format!("/{}", normalized_audio_href.trim_start_matches('/'));
    let normalized_authorizations =
        ReaderResourceAuthorizations::for_document(&normalized.epub, &normalized_document).unwrap();
    let normalized_route = normalized_authorizations
        .asset_for_path(&normalized_audio_path)
        .unwrap()
        .expect("normalized export path must retain the canonical object route");
    assert_eq!(normalized_route.asset_id, authorized.asset_id);
    let normalized_partial = MediaService::new(library)
        .serve(&imported.id, &normalized_route.asset_id, Some("bytes=0-3"))
        .unwrap();
    assert_eq!(normalized_partial.status, 206);
    assert_eq!(
        normalized_partial.content_range.as_deref(),
        Some(format!("bytes 0-3/{}", audio.len()).as_str())
    );
    assert_eq!(normalized_partial.body, audio[..4]);
}

#[test]
fn failed_asset_write_leaves_no_database_rows_and_startup_gc_collects_published_objects() {
    let temp = tempfile::tempdir().unwrap();
    let epub_path = temp.path().join("media-write-failure.epub");
    let media = write_media_epub(&epub_path);
    let source_bytes = std::fs::read(&epub_path).unwrap();
    let source_key = BlobKey::from_bytes(&source_bytes);
    let source_shard = source_key.as_str().split('/').nth(1).unwrap();
    let failing_key = media
        .iter()
        .map(|bytes| BlobKey::from_bytes(bytes))
        .find(|key| key.as_str().split('/').nth(1).unwrap() != source_shard)
        .expect("fixture must contain an asset in a different object shard");
    let failing_shard = failing_key.as_str().split('/').nth(1).unwrap();

    let library_dir = temp.path().join("library");
    let mut library = LibraryStore::load_from(library_dir.clone()).unwrap();
    let object_root = library.blob_store().root().to_path_buf();
    std::fs::create_dir_all(object_root.join("blake3")).unwrap();
    let shard_conflict = object_root.join("blake3").join(failing_shard);
    std::fs::write(&shard_conflict, b"not-a-directory").unwrap();

    let error = library.import(&epub_path).unwrap_err();
    assert!(
        error.to_string().contains("Blob") || error.to_string().contains("object"),
        "unexpected object write failure: {error:#}"
    );
    assert!(
        library.books().is_empty(),
        "failed import must not publish a book"
    );
    let source_path = source_key
        .as_str()
        .split('/')
        .fold(object_root.clone(), |path, component| path.join(component));
    assert!(
        source_path.is_file(),
        "the source object must have been published before the asset failure"
    );

    drop(library);
    std::fs::remove_file(shard_conflict).unwrap();
    let reopened = LibraryStore::load_from(library_dir).unwrap();
    assert!(reopened.books().is_empty());
    assert!(
        !source_path.exists(),
        "startup GC must collect objects left by the aborted import"
    );
}

fn valid_test_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&[185, 95, 66, 255], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    bytes
}

fn png_with_pixel(pixel: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&pixel, 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    bytes
}

fn write_media_epub(path: &Path) -> Vec<Vec<u8>> {
    let picture = png_with_pixel([31, 97, 191, 255]);
    let poster = png_with_pixel([197, 81, 43, 255]);
    let audio = b"ID3\x04\0\0moye-audio-fixture".to_vec();
    let video = b"\0\0\0\x18ftypmp42moye-video-fixture".to_vec();

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
    add_file(
        &mut zip,
        "OPS/package.opf",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:uuid:moye-media-test</dc:identifier>
    <dc:title>Media Round Trip</dc:title><dc:language>en</dc:language>
    <meta property="dcterms:modified">2026-09-02T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="picture" href="media/picture.png" media-type="image/png"/>
    <item id="poster" href="media/poster.png" media-type="image/png"/>
    <item id="audio" href="media/sound.mp3" media-type="audio/mpeg"/>
    <item id="video" href="media/movie.mp4" media-type="video/mp4"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        stored,
    );
    add_file(
        &mut zip,
        "OPS/text/chapter.xhtml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Media</title></head><body>
  <h1>Media</h1>
  <img src="../media/picture.png?width=1#pixel" alt="picture"/>
  <audio controls="controls"><source src="../media/sound.mp3"/></audio>
  <video controls="controls" src="../media/movie.mp4" poster="../media/poster.png"></video>
  <img src="https://example.invalid/tracker.png" alt="remote tracker"/>
</body></html>"#,
        stored,
    );
    add_file(&mut zip, "OPS/media/picture.png", &picture, stored);
    add_file(&mut zip, "OPS/media/poster.png", &poster, stored);
    add_file(&mut zip, "OPS/media/sound.mp3", &audio, stored);
    add_file(&mut zip, "OPS/media/movie.mp4", &video, stored);
    zip.finish().unwrap();

    vec![picture, poster, audio, video]
}

fn write_sample_epub(path: &Path) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    add_file(&mut zip, "mimetype", b"application/epub+zip", stored);
    add_file(
        &mut zip,
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
        stored,
    );
    add_file(
        &mut zip,
        "EPUB/package.opf",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:uuid:moye-test-book</dc:identifier>
    <dc:title>山海小记</dc:title>
    <dc:creator>测试作者</dc:creator>
    <dc:language>zh-CN</dc:language>
    <meta property="dcterms:modified">2026-08-31T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter-1" href="chapter-1.xhtml" media-type="application/xhtml+xml"/>
    <item id="chapter-2" href="chapter-2.xhtml" media-type="application/xhtml+xml"/>
    <item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>
  </manifest>
  <spine>
    <itemref idref="chapter-1"/>
    <itemref idref="chapter-2"/>
  </spine>
</package>"#
            .as_bytes(),
        stored,
    );
    add_file(
        &mut zip,
        "EPUB/nav.xhtml",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
  <head><title>目录</title></head>
  <body><nav epub:type="toc"><ol>
    <li><span>上篇</span><ol>
      <li><a href="chapter-1.xhtml">第一章</a></li>
      <li><a href="chapter-2.xhtml">第二章</a></li>
    </ol></li>
  </ol></nav></body>
</html>"#
            .as_bytes(),
        stored,
    );
    add_file(
        &mut zip,
        "EPUB/chapter-1.xhtml",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>第一章</title></head>
<body><h1>山海之间</h1><p>这是第一章。</p></body></html>"#
            .as_bytes(),
        stored,
    );
    add_file(
        &mut zip,
        "EPUB/chapter-2.xhtml",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>第二章</title></head>
<body><h1>归途</h1><p>这是第二章。</p></body></html>"#
            .as_bytes(),
        stored,
    );
    add_file(
        &mut zip,
        "EPUB/cover.png",
        &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, b'I', b'D', b'A', b'T', 0x08,
            0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99,
            0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
        ],
        stored,
    );

    zip.finish().unwrap();
}

fn write_sample_epub2(path: &Path) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    add_file(&mut zip, "mimetype", b"application/epub+zip", stored);
    add_file(
        &mut zip,
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        stored,
    );
    add_file(
        &mut zip,
        "OEBPS/content.opf",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:uuid:moye-test-epub2</dc:identifier>
    <dc:title>旧版小书</dc:title><dc:creator>测试作者</dc:creator><dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="chapter-1" href="chapter-1.xhtml" media-type="application/xhtml+xml"/>
    <item id="chapter-2" href="chapter-2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="chapter-1"/><itemref idref="chapter-2"/></spine>
</package>"#
            .as_bytes(),
        stored,
    );
    add_file(
        &mut zip,
        "OEBPS/toc.ncx",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head><meta name="dtb:uid" content="urn:uuid:moye-test-epub2"/></head>
  <docTitle><text>旧版小书</text></docTitle>
  <navMap>
    <navPoint id="nav-1" playOrder="1"><navLabel><text>开篇</text></navLabel><content src="chapter-1.xhtml"/></navPoint>
    <navPoint id="nav-2" playOrder="2"><navLabel><text>终章</text></navLabel><content src="chapter-2.xhtml"/></navPoint>
  </navMap>
</ncx>"#
            .as_bytes(),
        stored,
    );
    for (path, title) in [
        ("OEBPS/chapter-1.xhtml", "开篇"),
        ("OEBPS/chapter-2.xhtml", "终章"),
    ] {
        let chapter = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title></head>
<body><h1>{title}</h1><p>EPUB 2 正文。</p></body></html>"#
        );
        add_file(&mut zip, path, chapter.as_bytes(), stored);
    }
    zip.finish().unwrap();
}

fn add_file(zip: &mut ZipWriter<File>, path: &str, bytes: &[u8], options: SimpleFileOptions) {
    zip.start_file(path, options).unwrap();
    zip.write_all(bytes).unwrap();
}
