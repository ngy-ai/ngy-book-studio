//! Tests for the pane layout, the cover placeholder and AI references.

use super::*;

#[test]
fn empty_cover_placeholder_uses_the_document_format() {
    let formats = [
        (BookFormat::Epub, "EPUB"),
        (BookFormat::Pdf, "PDF"),
        (BookFormat::Doc, "DOC"),
        (BookFormat::Docx, "DOCX"),
        (BookFormat::Pptx, "PPTX"),
        (BookFormat::Xlsx, "XLSX"),
        (BookFormat::Mobi, "MOBI"),
        (BookFormat::Azw, "AZW"),
        (BookFormat::Azw3, "AZW3"),
        (BookFormat::Kfx, "KFX"),
        (BookFormat::Djvu, "DJVU"),
    ];
    for (format, expected) in formats {
        let document = BookDocument::new(
            "book-a",
            "测试",
            BookSource::imported(format, "original-a", Some("sample.bin".to_string())),
        );
        assert_eq!(editor_book_format_label(&document), expected);
    }
    assert_eq!(
        editor_book_format_label(&BookDocument::created("book-a", "测试")),
        "EPUB"
    );
}

#[test]
fn clearing_cover_removes_the_assignment_and_only_retains_content_use() {
    let bytes = b"cover-bytes";
    let mut cover = AssetRef::from_bytes(AssetRole::Cover, "image/png", None, bytes);
    cover.add_role(AssetRole::ContentImage);
    let cover_id = cover.id.clone();
    let mut document = BookDocument::created("book-a", "封面测试");
    document.cover_asset_id = Some(cover_id.clone());
    document.assets.push(cover);

    clear_editor_cover_assignment(&mut document);
    prune_editor_assets(&mut document);
    assert!(document.cover_asset_id.is_none());
    assert!(document.assets.is_empty());
    document.validate().unwrap();

    let mut referenced = AssetRef::from_bytes(AssetRole::Cover, "image/png", None, bytes);
    referenced.add_role(AssetRole::ContentImage);
    let referenced_id = referenced.id.clone();
    document.cover_asset_id = Some(referenced_id.clone());
    document.assets.push(referenced);
    document.units.push(ContentUnit::new(
        "unit-a",
        ContentUnitKind::Chapter,
        "第一章",
        "<img src=\"ngy-asset:image\" />",
        BlockDocument::new(vec![Block::Image {
            id: "image-block".to_string(),
            asset_id: referenced_id.clone(),
            alt: "封面插图".to_string(),
            title: None,
            caption: Vec::new(),
        }]),
    ));

    clear_editor_cover_assignment(&mut document);
    prune_editor_assets(&mut document);
    let retained = document.find_asset(&referenced_id).unwrap();
    assert!(!retained.has_role(AssetRole::Cover));
    assert!(retained.has_role(AssetRole::ContentImage));
    document.validate().unwrap();
}

#[test]
fn clear_cover_control_requires_a_cover_and_an_idle_editor() {
    assert!(editor_clear_cover_enabled(true, false));
    assert!(!editor_clear_cover_enabled(false, false));
    assert!(!editor_clear_cover_enabled(true, true));
}

#[test]
fn editor_ai_references_use_stable_unit_ids_and_freeze_multiple_unsaved_chapters() {
    let chapters = vec![
        EditorChapter {
            title: "第一章".to_string(),
            href: "Text/one.xhtml".to_string(),
            spine_index: Some(0),
            html: "<body><p>未保存的一</p></body>".to_string(),
        },
        EditorChapter {
            title: "第二章".to_string(),
            href: "Text/two.xhtml".to_string(),
            spine_index: Some(1),
            html: "<body><p>未保存的二</p></body>".to_string(),
        },
    ];
    let units = vec![
        EditorUnitState {
            id: "unit-one".to_string(),
            kind: ContentUnitKind::Chapter,
            source: chapters[0].html.clone(),
        },
        EditorUnitState {
            id: "unit-two".to_string(),
            kind: ContentUnitKind::Chapter,
            source: chapters[1].html.clone(),
        },
    ];

    let options = editor_reference_hints(
        "book-a",
        &chapters,
        &units,
        1,
        Some("第二章（改）"),
        Some("精确高亮"),
        7,
    );
    assert_eq!(options[0].unit_id, "unit-two");
    assert_eq!(options[1].unit_id, "unit-one");
    assert!(options[0].label.contains("第二章（改）"));

    let mut selected = options;
    selected.push(AiReferenceHint::chapter(
        "book-other",
        "unit-other",
        0,
        "其它书",
    ));
    freeze_editor_reference_hints(
        "book-a",
        &chapters,
        &units,
        1,
        Some("精确高亮"),
        9,
        &mut selected,
    );
    assert_eq!(selected[0].frozen_text.as_deref(), Some("精确高亮"));
    assert_eq!(selected[1].frozen_text.as_deref(), Some("未保存的一"));
    assert!(selected[0].label.starts_with("当前章节高亮"));
    assert_eq!(selected[0].revision, Some(9));
    assert!(selected[2].frozen_text.is_none());
}
