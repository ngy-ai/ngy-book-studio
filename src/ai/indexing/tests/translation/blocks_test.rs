use super::*;

#[test]
pub(crate) fn translation_blocks_flatten_text_blocks_in_document_order() {
    use crate::document::{Block, ListItem, TableCell, TableRow};

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'h', 1);
             INSERT INTO books(id, title, author, language, format, revision,
                               source_object_key, added_at, updated_at)
                 VALUES ('book', 'Book', '', 'en', 'epub', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'epub', 'original', 'objects/source', 1);",
    )
    .unwrap();
    let document = BlockDocument {
        schema_version: 1,
        blocks: vec![
            Block::paragraph("p1", "First paragraph"),
            Block::heading("h1", 2, "Heading"),
            Block::BlockQuote {
                id: "q1".into(),
                blocks: vec![Block::paragraph("q1p", "Quote")],
            },
            Block::BulletList {
                id: "l1".into(),
                items: vec![ListItem::new(vec![Block::paragraph("li1", "Item")])],
            },
            Block::CodeBlock {
                id: "c1".into(),
                language: None,
                code: "let x = 1;".into(),
            },
            Block::Table {
                id: "t1".into(),
                header: Some(TableRow::new(vec![TableCell::text("H")])),
                rows: vec![TableRow::new(vec![TableCell::text("C")])],
            },
            Block::ThematicBreak { id: "hr".into() },
        ],
    };
    conn.execute(
        "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                 source_locator_json, title, block_json, revision, created_at, updated_at)
             VALUES ('unit', 'book', 'source', 0, 'chapter', '{}', '第一章', ?1, 1, 1, 1)",
        rusqlite::params![serde_json::to_string(&document).unwrap()],
    )
    .unwrap();

    let blocks = translation_blocks(&conn, "book", "source").unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "unit::h0", "unit::h1", "unit::h2", "unit::h3", "unit::h4", "unit::h5"
        ],
    );
    assert!(blocks.iter().all(|block| block.unit_title == "第一章"));
    assert!(blocks.iter().all(|block| block.unit_ordinal == 0));
    // 每个文本块都冻结了书名；它与章节标题一起进入模型请求的用户消息。
    assert!(blocks.iter().all(|block| block.book_title == "Book"));
    let input: serde_json::Value = serde_json::from_str(
        &blocks[0]
            .source
            .request_input(&blocks[0].translation_context()),
    )
    .unwrap();
    assert_eq!(input["book"], "Book");
    assert_eq!(input["chapter"], "第一章");
    assert_eq!(
        blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5],
    );
    assert_eq!(blocks[0].source.text, "First paragraph");
    assert_eq!(blocks[2].unit_id, "unit");

    // Every chapter of the source stays in the list. A chapter whose own
    // revision is older than the document is still translated once: its
    // revision only decides whether a stored译文 may be reused, which the
    // per-block cache check does, not whether the chapter is listed.
    assert!(blocks.iter().all(|block| block.unit_revision == 1));
}

#[test]
pub(crate) fn translation_blocks_parse_preserved_epub_html_when_the_ast_is_raw() {
    use crate::document::Block;
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    let conn = db::open_or_recreate(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO blobs(object_key, media_type, byte_len, hash, created_at)
                 VALUES ('objects/source', 'application/epub+zip', 1, 'h', 1);
             INSERT INTO books(id, title, author, language, format, revision,
                               source_object_key, added_at, updated_at)
                 VALUES ('book', 'Book', '', 'en', 'epub', 1, 'objects/source', 1, 1);
             INSERT INTO book_sources(id, book_id, revision, format, source_kind,
                                      object_key, created_at)
                 VALUES ('source', 'book', 1, 'epub', 'original', 'objects/source', 1);",
    )
    .unwrap();
    // EPUB chapters are stored as one preserved RawHtml subtree; the visible
    // paragraph structure only exists in the canonical HTML source.
    let html = "<div id=\"sbo-rt-content\"><h1>Rust Brain Teasers</h1>
                    <p>Copyright 2022</p>
                    <p>Hello <b>world</b></p></div>";
    let document = BlockDocument {
        schema_version: 1,
        blocks: vec![Block::RawHtml {
            id: "raw".into(),
            source: html.into(),
            plain_text: crate::document::raw_html_plain_text(html).unwrap(),
        }],
    };
    conn.execute(
        "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                 source_locator_json, source_text, block_json, revision, created_at, updated_at)
             VALUES ('unit', 'book', 'source', 0, 'chapter', '{}', ?1, ?2, 1, 1, 1)",
        rusqlite::params![html, serde_json::to_string(&document).unwrap()],
    )
    .unwrap();

    let blocks = translation_blocks(&conn, "book", "source").unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.source.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Rust Brain Teasers", "Copyright 2022", "Hello world"],
    );
    assert!(blocks.iter().all(|block| block.unit_id == "unit"));

    // Deterministic IDs keep repeated runs idempotent for the same source.
    let again = translation_blocks(&conn, "book", "source").unwrap();
    assert_eq!(
        again
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
    );
}
