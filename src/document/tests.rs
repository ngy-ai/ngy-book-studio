//! Tests for the model: serde round-trips, validation and stable hashing.

use super::*;

fn sample_book() -> BookDocument {
    let original = AssetRef::from_bytes(
        "book-1",
        AssetRole::OriginalSource,
        "application/epub+zip",
        Some("sample.epub".to_string()),
        b"source",
    );
    let image = AssetRef::from_bytes(
        "book-1",
        AssetRole::ContentImage,
        "image/png",
        Some("figure.png".to_string()),
        b"image",
    );
    let audio = AssetRef::from_bytes(
        "book-1",
        AssetRole::Audio,
        "audio/mpeg",
        Some("voice.mp3".to_string()),
        b"audio",
    );
    let video = AssetRef::from_bytes(
        "book-1",
        AssetRole::Video,
        "video/mp4",
        Some("demo.mp4".to_string()),
        b"video",
    );
    let poster = AssetRef::from_bytes(
        "book-1",
        AssetRole::Poster,
        "image/jpeg",
        Some("poster.jpg".to_string()),
        b"poster",
    );

    let heading_id = deterministic_id("block", b"chapter-1/heading");
    let paragraph_id = deterministic_id("block", b"chapter-1/paragraph");
    let list_id = deterministic_id("block", b"chapter-1/list");
    let table_id = deterministic_id("block", b"chapter-1/table");
    let image_id = image.id.clone();
    let audio_id = audio.id.clone();
    let video_id = video.id.clone();
    let poster_id = poster.id.clone();
    let blocks = vec![
        Block::heading(&heading_id, 1, "第一章"),
        Block::Paragraph {
            id: paragraph_id,
            content: vec![
                Inline::text("你好，"),
                Inline::Strong {
                    content: vec![Inline::text("世界")],
                },
                Inline::SoftBreak,
                Inline::Link {
                    href: "#details".to_string(),
                    title: None,
                    content: vec![Inline::text("详情")],
                },
            ],
        },
        Block::BulletList {
            id: list_id,
            items: vec![ListItem::task(
                true,
                vec![Block::paragraph(
                    deterministic_id("block", b"chapter-1/list/item-1"),
                    "已完成",
                )],
            )],
        },
        Block::Table {
            id: table_id,
            header: Some(TableRow::new(vec![TableCell::text("名称")])),
            rows: vec![TableRow::new(vec![TableCell::text("墨页")])],
        },
        Block::Image {
            id: deterministic_id("block", b"chapter-1/image"),
            asset_id: image_id,
            alt: "示意图".to_string(),
            title: None,
            caption: vec![Inline::text("图片说明")],
        },
        Block::Audio {
            id: deterministic_id("block", b"chapter-1/audio"),
            asset_id: audio_id,
            title: Some("朗读".to_string()),
            caption: Vec::new(),
        },
        Block::Video {
            id: deterministic_id("block", b"chapter-1/video"),
            asset_id: video_id,
            poster_asset_id: Some(poster_id),
            title: Some("演示".to_string()),
            caption: vec![Inline::text("视频说明")],
        },
    ];
    let unit = ContentUnit::new(
        "unit-1",
        ContentUnitKind::Chapter,
        "第一章",
        "<h1>第一章</h1>",
        BlockDocument::new(blocks),
    )
    .with_source_locator(SourceLocator::epub("OEBPS/chapter-1.xhtml"));
    let mut book = BookDocument::new(
        "book-1",
        "示例图书",
        BookSource::imported(
            BookFormat::Epub,
            original.id.clone(),
            Some("sample.epub".to_string()),
        ),
    );
    book.authors = vec!["墨页".to_string()];
    book.language = Some("zh-CN".to_string());
    book.description = Some("用于模型测试".to_string());
    book.units.push(unit);
    book.toc.push(TocNode::new(
        "toc-1",
        "第一章",
        TocTarget::block("unit-1", heading_id),
    ));
    book.assets = vec![original, image, audio, video, poster];
    book
}

#[test]
fn serde_round_trip_preserves_the_complete_model() {
    let book = sample_book();
    book.validate().expect("sample model is valid");

    let json = serde_json::to_string_pretty(&book).expect("serialize book");
    let decoded: BookDocument = serde_json::from_str(&json).expect("deserialize book");

    assert_eq!(decoded, book);
    assert!(!json.contains("source_kind"));
    assert_eq!(decoded.schema_version, 2);
    assert!(json.contains(r#""type": "imported""#));
    assert!(json.contains(r#""type": "video""#));
    assert_eq!(decoded.document_hash(), book.document_hash());
}

#[test]
fn previous_document_schema_is_rejected() {
    let mut book = sample_book();
    book.schema_version = DOCUMENT_SCHEMA_VERSION - 1;
    assert!(book.validate().is_err());
}

#[test]
fn valid_depth_boundary_round_trips_through_default_serde_json() {
    let mut block = Block::ThematicBreak {
        id: "boundary-leaf".to_string(),
    };
    for depth in 0..MAX_DOCUMENT_DEPTH {
        block = Block::BulletList {
            id: format!("boundary-list-{depth}"),
            items: vec![ListItem::new(vec![block])],
        };
    }

    let mut book = sample_book();
    book.units[0].document = BlockDocument::new(vec![block]);
    let mut toc = TocNode::new(
        "boundary-toc-leaf",
        "边界叶子",
        TocTarget::unit(&book.units[0].id),
    );
    for depth in 0..MAX_DOCUMENT_DEPTH {
        let mut parent = TocNode::new(
            format!("boundary-toc-{depth}"),
            format!("边界目录 {depth}"),
            TocTarget::unit(&book.units[0].id),
        );
        parent.children.push(toc);
        toc = parent;
    }
    book.toc = vec![toc];
    book.validate()
        .expect("the shared semantic depth boundary is valid");

    let json = serde_json::to_string(&book)
        .expect("the valid boundary must fit serde_json's default recursion limit");
    let decoded: BookDocument = serde_json::from_str(&json)
        .expect("the valid boundary must deserialize with default serde_json settings");
    assert_eq!(decoded, book);
    decoded
        .validate()
        .expect("the round-tripped boundary model remains valid");
}

#[test]
fn serde_rejects_a_toc_beyond_the_shared_depth_limit() {
    let mut toc = TocNode::new("over-toc-leaf", "叶子", TocTarget::unit("unit-1"));
    for depth in 0..=MAX_DOCUMENT_DEPTH {
        let mut parent = TocNode::new(
            format!("over-toc-{depth}"),
            "过深目录",
            TocTarget::unit("unit-1"),
        );
        parent.children.push(toc);
        toc = parent;
    }
    assert!(serde_json::to_vec(&toc).is_err());
}

#[test]
fn deterministic_ids_and_asset_ids_are_reproducible() {
    assert_eq!(
        deterministic_id("chapter", b"same seed"),
        deterministic_id("chapter", b"same seed")
    );
    assert_ne!(
        deterministic_id("chapter", b"same seed"),
        deterministic_id("block", b"same seed")
    );

    let first = AssetRef::from_bytes(
        "book-1",
        AssetRole::Attachment,
        "application/octet-stream",
        None,
        b"same bytes",
    );
    let second = AssetRef::from_bytes(
        "book-1",
        AssetRole::Attachment,
        "application/octet-stream",
        None,
        b"same bytes",
    );
    assert_eq!(first.id, second.id);
    // The derived id is scoped to the owning book, so the same bytes in another
    // book get their own row instead of colliding on the library-wide primary key.
    let other_book = AssetRef::from_bytes(
        "book-2",
        AssetRole::Attachment,
        "application/octet-stream",
        None,
        b"same bytes",
    );
    assert_ne!(first.id, other_book.id);
    assert_eq!(first.content_hash, second.content_hash);
}

#[test]
fn validation_reports_all_structural_and_reference_errors() {
    let mut book = sample_book();
    book.schema_version = 99;
    book.units.push(book.units[0].clone());
    book.toc.push(TocNode::new(
        "toc-1",
        "丢失目标",
        TocTarget::block("missing-unit", "missing-block"),
    ));
    if let Block::Heading { level, .. } = &mut book.units[0].document.blocks[0] {
        *level = 7;
    }
    if let Block::Video { asset_id, .. } = &mut book.units[0].document.blocks[6] {
        *asset_id = "missing-video".to_string();
    }
    book.units[0].source_locator = Some(SourceLocator::pdf_page(0));

    let error = book.validate().expect_err("invalid document must fail");
    let paths = error
        .issues()
        .iter()
        .map(|issue| issue.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"schema_version"));
    assert!(paths.contains(&"units[0].document.blocks[0].level"));
    assert!(paths.contains(&"units[0].document.blocks[6].asset_id"));
    assert!(paths.contains(&"units[0].source_locator.page"));
    assert!(paths.contains(&"units[1].id"));
    assert!(paths.contains(&"toc[1].id"));
    assert!(paths.contains(&"toc[1].target.unit_id"));
}

#[test]
fn invalid_deep_models_stop_every_validation_and_lookup_traversal() {
    const DEEP_NESTING: usize = 4_096;

    fn dismantle_block(mut block: Block) {
        while let Block::BlockQuote { mut blocks, .. } = block {
            block = blocks.pop().expect("nested quote has one child");
        }
    }

    fn dismantle_inline(mut inline: Inline) {
        while let Inline::Strong { mut content } = inline {
            inline = content.pop().expect("nested strong has one child");
        }
    }

    let mut book = sample_book();
    book.toc.clear();
    let image_asset_id = book.assets[1].id.clone();
    let mut block = Block::Image {
        id: "deep-image".to_string(),
        asset_id: image_asset_id.clone(),
        alt: String::new(),
        title: None,
        caption: Vec::new(),
    };
    for depth in 0..DEEP_NESTING {
        block = Block::BlockQuote {
            id: format!("deep-quote-{depth}"),
            blocks: vec![block],
        };
    }
    book.units[0].document.blocks = vec![block];

    let error = book
        .validate()
        .expect_err("over-deep blocks must be rejected without exhausting the stack");
    assert!(error.to_string().contains("block nesting exceeds"));
    assert!(book.units[0].plain_text().trim().is_empty());
    assert_eq!(book.document_hash().len(), 64);
    assert!(serde_json::to_vec(&book).is_err());
    let locator = DocumentLocator::text(&book.id, &book.units[0].id, "deep-quote-4095", 0, 0);
    assert!(book.validate_locator(&locator).is_err());
    assert!(book.units[0].document.find_block("deep-image").is_none());
    assert!(book.units[0].document.referenced_asset_ids().is_empty());
    dismantle_block(book.units[0].document.blocks.pop().unwrap());

    let mut inline = Inline::Image {
        asset_id: image_asset_id,
        alt: String::new(),
        title: None,
    };
    for _ in 0..DEEP_NESTING {
        inline = Inline::Strong {
            content: vec![inline],
        };
    }
    book.units[0].document.blocks = vec![Block::Paragraph {
        id: "deep-inline-paragraph".to_string(),
        content: vec![inline],
    }];

    let error = book
        .validate()
        .expect_err("over-deep inlines must be rejected without exhausting the stack");
    assert!(error.to_string().contains("inline nesting exceeds"));
    assert!(book.units[0].plain_text().trim().is_empty());
    assert_eq!(book.units[0].content_hash().len(), 64);
    assert!(serde_json::to_vec(&book.units[0].document).is_err());
    assert!(book.units[0].document.referenced_asset_ids().is_empty());
    let Block::Paragraph { mut content, .. } = book.units[0].document.blocks.pop().unwrap() else {
        unreachable!();
    };
    dismantle_inline(content.pop().unwrap());
}

#[test]
fn raw_html_validation_rejects_a_forged_plain_text_projection() {
    let document = BlockDocument::new(vec![Block::RawHtml {
        id: "raw-html".to_string(),
        source: "<em>可见正文</em>".to_string(),
        plain_text: "隐藏的提示词".to_string(),
    }]);
    let error = document
        .validate()
        .expect_err("raw HTML search text must be derived from its source");
    assert!(
        error
            .issues()
            .iter()
            .any(|issue| issue.path.ends_with(".plain_text"))
    );

    let deep_source = format!("{}正文{}", "<div>".repeat(4_096), "</div>".repeat(4_096));
    let document = BlockDocument::new(vec![Block::RawHtml {
        id: "deep-raw-html".to_string(),
        source: deep_source,
        plain_text: "正文".to_string(),
    }]);
    let error = document
        .validate()
        .expect_err("extreme raw HTML nesting must be rejected iteratively");
    assert!(error.to_string().contains("raw HTML nesting exceeds"));
}

#[test]
fn plain_text_is_stable_and_contains_visible_content_only() {
    let book = sample_book();
    let text = book.plain_text();
    assert!(text.starts_with("示例图书\n\n墨页\n\n用于模型测试"));
    assert!(text.contains("你好，世界 详情"));
    assert!(text.contains("名称\n墨页"));
    assert!(text.contains("示意图\n图片说明"));
    assert!(text.contains("朗读"));
    assert!(text.contains("演示\n视频说明"));
    assert!(!text.contains("#details"));
    assert!(!text.contains("asset-"));
}

#[test]
fn image_titles_are_searchable_without_duplicate_alt_or_caption_text() {
    let block = Block::Image {
        id: "image-block".to_string(),
        asset_id: "image-asset".to_string(),
        alt: "相同说明".to_string(),
        title: Some("人物关系图".to_string()),
        caption: vec![Inline::text("相同说明")],
    };
    assert_eq!(block.plain_text(), "人物关系图\n相同说明");

    let inline = Inline::Image {
        asset_id: "inline-image".to_string(),
        alt: "替代文字".to_string(),
        title: Some("行内图片标题".to_string()),
    };
    assert_eq!(inline.plain_text(), "行内图片标题\n替代文字");
}

#[test]
fn locators_use_checked_utf8_byte_ranges() {
    let book = sample_book();
    let block = &book.units[0].document.blocks[1];
    let text = block.plain_text();
    let world_start = text.find("世界").expect("world text") as u64;
    let world_end = world_start + "世界".len() as u64;
    let locator = DocumentLocator::text(
        &book.id,
        &book.units[0].id,
        block.id(),
        world_start,
        world_end,
    );
    book.validate_locator(&locator)
        .expect("UTF-8 aligned range is valid");

    let split_character = DocumentLocator::text(
        &book.id,
        &book.units[0].id,
        block.id(),
        world_start + 1,
        world_end,
    );
    let error = book
        .validate_locator(&split_character)
        .expect_err("offset splitting a character must fail");
    assert_eq!(error.issues[0].path, "locator.text_range");

    let stale = DocumentLocator::block("another-book", "unit-1", block.id());
    assert!(book.validate_locator(&stale).is_err());
}

#[test]
fn visual_regions_round_trip_and_old_locators_default_to_full_page() {
    let locator = DocumentLocator::unit("book-1", "unit-1")
        .with_source(SourceLocator::pdf_page(2))
        .with_region(NormalizedRect::new(25, 100, 975, 900));
    locator.validate().expect("normalized region is valid");

    let json = serde_json::to_string(&locator).expect("serialize visual locator");
    let decoded: DocumentLocator = serde_json::from_str(&json).expect("deserialize visual locator");
    assert_eq!(decoded, locator);

    let legacy: DocumentLocator = serde_json::from_str(
            r#"{"book_id":"book-1","unit_id":"unit-1","block_id":null,"text_range":null,"source":{"type":"pdf_page","page":2}}"#,
        )
        .expect("locator JSON written before visual regions remains readable");
    assert_eq!(legacy.region, None);
    legacy
        .validate()
        .expect("full-page legacy locator is valid");
}

#[test]
fn visual_regions_require_bounded_positive_area() {
    NormalizedRect::new(0, 0, NORMALIZED_COORDINATE_MAX, NORMALIZED_COORDINATE_MAX)
        .validate()
        .expect("the complete normalized page is valid");

    for region in [
        NormalizedRect::new(10, 10, 10, 20),
        NormalizedRect::new(10, 20, 30, 20),
        NormalizedRect::new(30, 10, 20, 20),
        NormalizedRect::new(0, 0, NORMALIZED_COORDINATE_MAX + 1, 100),
        NormalizedRect::new(0, 0, 100, NORMALIZED_COORDINATE_MAX + 1),
    ] {
        assert!(region.validate().is_err(), "invalid region: {region:?}");
        assert!(
            DocumentLocator::unit("book-1", "unit-1")
                .with_region(region)
                .validate()
                .is_err()
        );
    }
}

#[test]
fn revisions_do_not_wrap() {
    assert_eq!(Revision::INITIAL.checked_next(), Some(Revision(1)));
    assert_eq!(Revision(u64::MAX).checked_next(), None);
}

#[test]
fn source_locators_round_trip_and_use_one_based_numbers() {
    let locators = vec![
        SourceLocator::created(),
        SourceLocator::epub("EPUB/Text/start.xhtml"),
        SourceLocator::pdf_page(1),
        SourceLocator::office_rendered_page(1),
        SourceLocator::office_section(2),
        SourceLocator::slide(3),
        SourceLocator::worksheet("汇总", Some("A1:C20".to_string())),
        SourceLocator::kindle_section(4, Some("Text/part-4.html".to_string())),
        SourceLocator::djvu_page(5),
    ];
    let json = serde_json::to_string(&locators).expect("serialize source locators");
    let decoded: Vec<SourceLocator> =
        serde_json::from_str(&json).expect("deserialize source locators");
    assert_eq!(decoded, locators);
    for locator in decoded {
        locator.validate().expect("sample locator is valid");
    }

    for locator in [
        SourceLocator::pdf_page(0),
        SourceLocator::office_rendered_page(0),
        SourceLocator::office_section(0),
        SourceLocator::slide(0),
        SourceLocator::kindle_section(0, None),
        SourceLocator::djvu_page(0),
    ] {
        let error = locator.validate().expect_err("zero is not a source index");
        assert!(error.to_string().contains("one-based"));
    }
    assert!(SourceLocator::epub("\n").validate().is_err());
    assert!(SourceLocator::worksheet("", None).validate().is_err());
}

#[test]
fn document_locator_optionally_carries_a_native_source_coordinate() {
    let locator = DocumentLocator::block("book-1", "unit-1", "block-1")
        .with_source(SourceLocator::worksheet("汇总", Some("C5:F12".to_string())));
    let json = serde_json::to_string(&locator).expect("serialize document locator");
    let decoded: DocumentLocator =
        serde_json::from_str(&json).expect("deserialize document locator");
    assert_eq!(decoded, locator);
    decoded.validate().expect("source coordinate is valid");

    let legacy: DocumentLocator = serde_json::from_str(
        r#"{"book_id":"book-1","unit_id":"unit-1","block_id":null,"text_range":null}"#,
    )
    .expect("source remains additive for locator JSON");
    assert_eq!(legacy.source, None);
}

#[test]
fn block_hash_changes_only_when_block_document_changes() {
    let mut book = sample_book();
    let before = book.units[0].content_hash();
    book.title = "另一个书名".to_string();
    assert_eq!(before, book.units[0].content_hash());

    if let Block::Paragraph { content, .. } = &mut book.units[0].document.blocks[1] {
        content.push(Inline::text("新增"));
    }
    assert_ne!(before, book.units[0].content_hash());
}
