use std::{
    fs,
    io::{Cursor, Read, Write},
    sync::Arc,
};

use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use moye_epub_editor::{
    document::{BookFormat, BookSource, ContentUnitKind, SourceKind, SourceLocator},
    formats::{FormatRegistry, ImportLimits, ImportSource, ProbeConfidence},
    library::{ImportOutcome, LibraryStore},
};
use office_oxide::{DocumentFormat, create::create_from_markdown_to_writer};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Clone)]
struct Fixture {
    extension: &'static str,
    format: BookFormat,
    unit_kind: ContentUnitKind,
    token: &'static str,
    bytes: Vec<u8>,
}

#[test]
fn generated_pdf_and_ooxml_import_into_the_canonical_model() {
    let registry = FormatRegistry::with_builtin_importers();

    for fixture in fixtures() {
        let source = ImportSource {
            file_name: Some(format!("fixture.{}", fixture.extension)),
            bytes: Arc::new(fixture.bytes.clone()),
        };
        let probes = registry.probe(&source);
        assert_eq!(
            probes.first().map(|probe| probe.format),
            Some(fixture.format)
        );
        assert!(
            probes
                .first()
                .is_some_and(|probe| probe.confidence >= ProbeConfidence::Container),
            "{} should be recognized by its contents",
            fixture.extension
        );

        let imported = registry
            .import(&source, &ImportLimits::default())
            .unwrap_or_else(|error| panic!("failed to import {}: {error:#}", fixture.extension));
        assert!(matches!(
            imported.document.source,
            BookSource::Imported { format, .. } if format == fixture.format
        ));
        assert!(!imported.document.units.is_empty());
        assert_eq!(imported.document.units[0].kind, fixture.unit_kind);
        assert!(
            imported
                .document
                .units
                .iter()
                .any(|unit| unit.plain_text().contains(fixture.token)),
            "{} canonical content did not contain the fixture token",
            fixture.extension
        );
        assert_locator(&fixture, &imported.document.units[0].source_locator);
        assert_eq!(
            imported
                .original_asset()
                .expect("original asset")
                .bytes
                .as_slice(),
            fixture.bytes,
            "{} original asset must remain byte exact",
            fixture.extension
        );
    }
}

#[test]
fn each_generated_format_survives_library_edit_search_export_and_reopen() {
    let temp = tempfile::tempdir().expect("temporary test directory");

    for fixture in fixtures() {
        let case_dir = temp.path().join(fixture.extension);
        fs::create_dir_all(&case_dir).expect("create case directory");
        let source_path = case_dir.join(format!("source.{}", fixture.extension));
        fs::write(&source_path, &fixture.bytes).expect("write source fixture");
        let data_dir = case_dir.join("library");
        let mut library = LibraryStore::load_from(data_dir.clone()).expect("open library");

        let record = match library.import(&source_path).unwrap_or_else(|error| {
            panic!("library failed to import {}: {error:#}", fixture.extension)
        }) {
            ImportOutcome::Added(record) => record,
            ImportOutcome::AlreadyExists(_) => panic!("first import unexpectedly deduplicated"),
        };
        let imported_document = library
            .document(&record.id)
            .expect("load canonical document");
        assert_eq!(imported_document.units[0].kind, fixture.unit_kind);
        assert_locator(&fixture, &imported_document.units[0].source_locator);
        assert!(
            library
                .search_book(&record.id, fixture.token, 10)
                .expect("search imported content")
                .iter()
                .any(|hit| hit.book_id == record.id),
            "{} content was not indexed",
            fixture.extension
        );

        let projection = library
            .reader_epub_bytes(&record.id)
            .expect("create EPUB reader projection");
        let projected = rbook::Epub::read(Cursor::new(projection)).expect("open reader projection");
        assert_eq!(
            projected
                .spine()
                .iter()
                .filter(|entry| entry.is_linear())
                .count(),
            imported_document.units.len()
        );

        let edited_token = format!("Edited{}CanonicalToken", fixture.extension);
        let first_unit_id = imported_document.units[0].id.clone();
        let updated = library
            .update_content_unit_source(
                &record.id,
                &first_unit_id,
                SourceKind::Markdown,
                &format!("# Edited\n\n{edited_token}"),
            )
            .expect("publish canonical edit");
        assert_eq!(updated.revision, record.revision + 1);
        assert!(
            library
                .search_book(&record.id, &edited_token, 10)
                .expect("search edited content")
                .iter()
                .any(|hit| hit.book_id == record.id),
            "{} edited content was not re-indexed",
            fixture.extension
        );

        let epub_path = case_dir.join("normalized.epub");
        let pdf_path = case_dir.join("normalized.pdf");
        let original_path = case_dir.join(format!("original.{}", fixture.extension));
        library
            .export_epub(&record.id, &epub_path)
            .expect("export EPUB");
        library
            .export_pdf(&record.id, &pdf_path)
            .expect("export PDF");
        library
            .export_original(&record.id, &original_path)
            .expect("export original");
        assert!(rbook::Epub::read(Cursor::new(fs::read(&epub_path).unwrap())).is_ok());
        assert!(lopdf::Document::load(&pdf_path).is_ok());
        assert_eq!(
            fs::read(&original_path).expect("read original export"),
            fixture.bytes,
            "{} original changed after canonical editing",
            fixture.extension
        );

        drop(library);
        let reopened = LibraryStore::load_from(data_dir).expect("reopen library");
        let reopened_record = reopened
            .book_record(&record.id)
            .expect("restore book record");
        let reopened_document = reopened
            .document(&record.id)
            .expect("restore canonical document");
        assert_eq!(reopened_record.revision, record.revision + 1);
        assert!(
            reopened_document.units[0]
                .plain_text()
                .contains(&edited_token)
        );
        assert_eq!(
            fs::read(&original_path).expect("re-read original export"),
            fixture.bytes
        );
    }
}

#[test]
fn malformed_encrypted_and_over_limit_inputs_are_rejected() {
    let registry = FormatRegistry::with_builtin_importers();

    let malformed_pdf = ImportSource {
        file_name: Some("broken.pdf".into()),
        bytes: Arc::new(b"%PDF-1.7\nnot a PDF".to_vec()),
    };
    assert!(
        registry
            .import(&malformed_pdf, &ImportLimits::default())
            .is_err()
    );

    let encrypted_pdf = make_pdf("EncryptedPdfToken", true);
    let encrypted_pdf = ImportSource {
        file_name: Some("encrypted.pdf".into()),
        bytes: Arc::new(encrypted_pdf),
    };
    let error = registry
        .import(&encrypted_pdf, &ImportLimits::default())
        .expect_err("encrypted PDFs must be rejected");
    assert!(error.to_string().to_ascii_lowercase().contains("encrypted"));

    let malformed_docx = ImportSource {
        file_name: Some("broken.docx".into()),
        bytes: Arc::new(malformed_ooxml("word/document.xml")),
    };
    assert!(
        registry
            .import(&malformed_docx, &ImportLimits::default())
            .is_err()
    );

    let fake_doc = ImportSource {
        file_name: Some("broken.doc".into()),
        bytes: Arc::new({
            let mut bytes = vec![0_u8; 512];
            bytes[..4].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0]);
            bytes
        }),
    };
    assert!(
        registry
            .import(&fake_doc, &ImportLimits::default())
            .is_err()
    );

    let fake_kindle = ImportSource {
        file_name: Some("broken.azw3".into()),
        bytes: Arc::new(fake_kindle_header(8)),
    };
    assert_eq!(registry.probe(&fake_kindle)[0].format, BookFormat::Azw3);
    assert!(
        registry
            .import(&fake_kindle, &ImportLimits::default())
            .is_err()
    );

    let pdf = make_pdf("PdfLimitToken", false);
    let pdf_source = ImportSource {
        file_name: Some("limit.pdf".into()),
        bytes: Arc::new(pdf.clone()),
    };
    assert!(
        registry
            .import(
                &pdf_source,
                &ImportLimits {
                    max_source_bytes: (pdf.len() - 1) as u64,
                    ..ImportLimits::default()
                },
            )
            .is_err()
    );
    assert!(
        registry
            .import(
                &pdf_source,
                &ImportLimits {
                    max_units: 0,
                    ..ImportLimits::default()
                },
            )
            .is_err()
    );
    assert!(
        registry
            .import(
                &pdf_source,
                &ImportLimits {
                    max_unit_text_bytes: 3,
                    max_total_text_bytes: 3,
                    ..ImportLimits::default()
                },
            )
            .is_err()
    );
}

#[test]
fn high_compression_ratio_ooxml_entry_is_rejected_before_parsing() {
    let valid = make_office(DocumentFormat::Docx, "ZipBombToken");
    let bomb = add_high_compression_entry(&valid);
    let source = ImportSource {
        file_name: Some("compressed-bomb.docx".into()),
        bytes: Arc::new(bomb),
    };
    let error = FormatRegistry::with_builtin_importers()
        .import(&source, &ImportLimits::default())
        .expect_err("suspicious OOXML compression ratio must be rejected");
    let message = format!("{error:#}").to_ascii_lowercase();
    assert!(message.contains("compression") || message.contains("archive"));
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            extension: "pdf",
            format: BookFormat::Pdf,
            unit_kind: ContentUnitKind::Page,
            token: "MoyePdfUniqueToken",
            bytes: make_pdf("MoyePdfUniqueToken", false),
        },
        Fixture {
            extension: "docx",
            format: BookFormat::Docx,
            unit_kind: ContentUnitKind::Chapter,
            token: "MoyeDocxUniqueToken",
            bytes: make_office(DocumentFormat::Docx, "MoyeDocxUniqueToken"),
        },
        Fixture {
            extension: "pptx",
            format: BookFormat::Pptx,
            unit_kind: ContentUnitKind::Slide,
            token: "MoyePptxUniqueToken",
            bytes: make_office(DocumentFormat::Pptx, "MoyePptxUniqueToken"),
        },
        Fixture {
            extension: "xlsx",
            format: BookFormat::Xlsx,
            unit_kind: ContentUnitKind::Worksheet,
            token: "MoyeXlsxUniqueToken",
            bytes: make_office(DocumentFormat::Xlsx, "MoyeXlsxUniqueToken"),
        },
    ]
}

fn assert_locator(fixture: &Fixture, locator: &Option<SourceLocator>) {
    let matches = match (fixture.format, locator.as_ref()) {
        (BookFormat::Pdf, Some(SourceLocator::PdfPage { page })) => *page == 1,
        (BookFormat::Docx, Some(SourceLocator::OfficeSection { index })) => *index >= 1,
        (BookFormat::Pptx, Some(SourceLocator::Slide { index })) => *index >= 1,
        (BookFormat::Xlsx, Some(SourceLocator::Worksheet { name, .. })) => !name.is_empty(),
        _ => false,
    };
    assert!(
        matches,
        "unexpected {} locator: {locator:?}",
        fixture.extension
    );
}

fn make_office(format: DocumentFormat, token: &str) -> Vec<u8> {
    let markdown = match format {
        DocumentFormat::Docx => format!("# Overview\n\n{token}\n\n# Details\n\nSecond section"),
        DocumentFormat::Pptx => {
            format!("# First slide\n\n{token}\n\n---\n\n# Second slide\n\nMore")
        }
        DocumentFormat::Xlsx => {
            format!("# Data\n\n| Name | Value |\n| --- | --- |\n| marker | {token} |")
        }
        _ => unreachable!("test creates only supported OOXML formats"),
    };
    let mut output = Cursor::new(Vec::new());
    create_from_markdown_to_writer(&markdown, format, &mut output)
        .expect("office_oxide should create a test fixture");
    output.into_inner()
}

fn make_pdf(token: &str, encrypted: bool) -> Vec<u8> {
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
            Operation::new("Tj", vec![Object::string_literal(token)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("encode PDF content"),
    ));
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
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    if encrypted {
        let encryption_id = document.add_object(dictionary! {
            "Filter" => "Standard",
            "V" => 1,
            "R" => 2,
            "O" => Object::string_literal(vec![0_u8; 32]),
            "U" => Object::string_literal(vec![0_u8; 32]),
            "P" => -4,
        });
        document.trailer.set("Encrypt", encryption_id);
    }
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("write PDF fixture");
    bytes
}

fn malformed_ooxml(marker_path: &str) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut archive = ZipWriter::new(&mut output);
        archive
            .start_file(marker_path, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"not XML").unwrap();
        archive.finish().unwrap();
    }
    output.into_inner()
}

fn add_high_compression_entry(source: &[u8]) -> Vec<u8> {
    let mut input = ZipArchive::new(Cursor::new(source)).expect("open OOXML fixture");
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        for index in 0..input.len() {
            let mut entry = input.by_index(index).expect("read OOXML entry");
            if entry.is_dir() {
                writer
                    .add_directory(entry.name(), SimpleFileOptions::default())
                    .unwrap();
                continue;
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            writer
                .start_file(
                    entry.name(),
                    SimpleFileOptions::default().compression_method(entry.compression()),
                )
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer
            .start_file(
                "word/media/suspicious.bin",
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&vec![0_u8; 2 * 1024 * 1024]).unwrap();
        writer.finish().unwrap();
    }
    output.into_inner()
}

fn fake_kindle_header(version: u32) -> Vec<u8> {
    let mut bytes = vec![0_u8; 160];
    bytes[60..68].copy_from_slice(b"BOOKMOBI");
    bytes[76..78].copy_from_slice(&1_u16.to_be_bytes());
    bytes[78..82].copy_from_slice(&100_u32.to_be_bytes());
    bytes[116..120].copy_from_slice(b"MOBI");
    bytes[136..140].copy_from_slice(&version.to_be_bytes());
    bytes
}
