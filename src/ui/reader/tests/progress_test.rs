//! Tests for the progress writer and the reference hints.

use super::*;

#[test]
pub(crate) fn progress_writer_updates_the_shared_store_and_flushes_the_final_position() {
    let data_dir = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let book = runtime.block_on(async {
        services
            .spawn_library(|library| {
                let created = library.create_book("测试图书", "作者")?;
                let mut editor =
                    ngy_book_studio::editing::DocumentEditor::new(library.document(&created.id)?)?;
                for index in 1..=3 {
                    editor.add_unit(ngy_book_studio::editing::NewContentUnit::html_chapter(
                        format!("第 {} 章", index + 1),
                        index,
                    ))?;
                }
                library.apply_document(editor.into_document())
            })
            .await
            .unwrap()
            .unwrap()
    });
    let stale_window_snapshot = services.library_snapshot().unwrap();
    let document = stale_window_snapshot.document(&book.id).unwrap();
    let progress = |spine_index: usize| ReadingProgressWrite::Unit {
        spine_index,
        unit_id: document.units[spine_index].id.clone(),
        anchor: None,
    };
    let incarnation = stale_window_snapshot
        .progress_incarnation(&book.id)
        .unwrap();
    let (writer, events) =
        ReadingProgressWriter::start(book.id.clone(), incarnation, Arc::clone(&services));

    writer.enqueue(progress(1)).unwrap();
    writer.enqueue(progress(2)).unwrap();
    let final_progress = progress(3);
    let (queued, completion) = writer.finish(final_progress.clone());
    queued.unwrap();

    let completed = runtime.block_on(async {
        let final_projection = completion.recv().await.unwrap().unwrap();
        assert!(final_projection.is_some());
        let mut completed = Vec::new();
        while let Ok(event) = events.recv().await {
            completed.push(event);
        }
        completed
    });
    assert!(!completed.is_empty());
    assert!(completed.iter().all(|event| event.error.is_none()));
    assert!(completed.iter().all(|event| event.projection.is_some()));
    assert_eq!(
        completed.last().map(|event| &event.progress),
        Some(&final_progress)
    );
    assert_eq!(stale_window_snapshot.books()[0].last_spine, 0);
    assert_eq!(
        services
            .library_snapshot()
            .unwrap()
            .book_record(&book.id)
            .unwrap()
            .last_spine,
        3
    );
}

#[test]
pub(crate) fn progress_writer_persists_the_chapter_reading_position() {
    let data_dir = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let book = runtime.block_on(async {
        services
            .spawn_library(|library| library.create_book("测试图书", "作者"))
            .await
            .unwrap()
            .unwrap()
    });
    let snapshot = services.library_snapshot().unwrap();
    let document = snapshot.document(&book.id).unwrap();
    let unit_id = document.units[0].id.clone();
    let anchor = TextAnchor {
        quote: "段落文字".to_string(),
        start: 24,
        end: 28,
    };
    let incarnation = snapshot.progress_incarnation(&book.id).unwrap();
    let (writer, _events) =
        ReadingProgressWriter::start(book.id.clone(), incarnation, Arc::clone(&services));
    let (queued, completion) = writer.finish(ReadingProgressWrite::Unit {
        spine_index: 0,
        unit_id: unit_id.clone(),
        anchor: Some(anchor.clone()),
    });
    queued.unwrap();
    runtime.block_on(async {
        assert!(completion.recv().await.unwrap().unwrap().is_some());
    });

    let stored = services
        .library_snapshot()
        .unwrap()
        .reading_position(&book.id)
        .unwrap();
    assert_eq!(stored, Some((unit_id, anchor)));
}

#[test]
pub(crate) fn reading_position_anchor_requires_a_self_consistent_quote() {
    let valid = TextAnchor {
        quote: "段落文字".to_string(),
        start: 8,
        end: 12,
    };
    assert!(reading_position_anchor_is_valid(&valid));
    for broken in [
        // 空引文
        TextAnchor {
            quote: String::new(),
            start: 0,
            end: 0,
        },
        // 范围倒置
        TextAnchor {
            quote: "文字".to_string(),
            start: 9,
            end: 8,
        },
        // 引文长度与范围不一致
        TextAnchor {
            quote: "文字".to_string(),
            start: 8,
            end: 11,
        },
        // 引文含空白：去空白坐标系里不存在这样的切片
        TextAnchor {
            quote: "段落 文字".to_string(),
            start: 8,
            end: 13,
        },
    ] {
        assert!(
            !reading_position_anchor_is_valid(&broken),
            "accepted {broken:?}"
        );
    }
    let oversized = TextAnchor {
        quote: "字".repeat(MAX_READING_POSITION_QUOTE_BYTES),
        start: 0,
        end: MAX_READING_POSITION_QUOTE_BYTES as u32,
    };
    assert!(!reading_position_anchor_is_valid(&oversized));
}

#[test]
pub(crate) fn progress_writer_reports_a_final_persistence_failure() {
    let data_dir = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let book = runtime.block_on(async {
        services
            .spawn_library(|library| library.create_book("测试图书", "作者"))
            .await
            .unwrap()
            .unwrap()
    });
    let unit_id = services
        .library_snapshot()
        .unwrap()
        .document(&book.id)
        .unwrap()
        .units[0]
        .id
        .clone();
    let db_path = services
        .library_snapshot()
        .unwrap()
        .database_path()
        .to_path_buf();
    let incarnation = services
        .library_snapshot()
        .unwrap()
        .progress_incarnation(&book.id)
        .unwrap();
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute("DELETE FROM progress WHERE book_id = ?1", [&book.id])
        .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_progress_write
             BEFORE INSERT ON progress
             BEGIN
                 SELECT RAISE(ABORT, 'test progress write failure');
             END;",
    )
    .unwrap();
    drop(conn);
    let (writer, events) =
        ReadingProgressWriter::start(book.id.clone(), incarnation, Arc::clone(&services));

    let (queued, completion) = writer.finish(ReadingProgressWrite::Unit {
        spine_index: 0,
        unit_id: unit_id.clone(),
        anchor: None,
    });
    queued.unwrap();

    let (completion_result, event) = runtime.block_on(async {
        let completion_result = completion.recv().await.unwrap();
        (completion_result, events.recv().await.unwrap())
    });
    assert!(
        completion_result
            .as_ref()
            .is_err_and(|error| error.contains("无法保存阅读进度")),
        "unexpected completion result: {completion_result:?}"
    );
    assert_eq!(
        event.progress,
        ReadingProgressWrite::Unit {
            spine_index: 0,
            unit_id,
            anchor: None,
        }
    );
    assert!(
        event
            .error
            .as_deref()
            .is_some_and(|error| error.contains("无法保存阅读进度"))
    );
}

#[test]
pub(crate) fn progress_writer_discards_a_removed_canonical_position() {
    let data_dir = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(data_dir.path()).unwrap());
    let runtime = services.runtime();
    let book = runtime.block_on(async {
        services
            .spawn_library(|library| library.create_book("测试图书", "作者"))
            .await
            .unwrap()
            .unwrap()
    });
    let incarnation = services
        .library_snapshot()
        .unwrap()
        .progress_incarnation(&book.id)
        .unwrap();
    let (writer, events) =
        ReadingProgressWriter::start(book.id, incarnation, Arc::clone(&services));

    let (queued, completion) = writer.finish(ReadingProgressWrite::Unit {
        spine_index: 0,
        unit_id: "removed-unit".to_string(),
        anchor: None,
    });
    queued.unwrap();

    let (completion_result, event) = runtime.block_on(async {
        let completion_result = completion.recv().await.unwrap();
        (completion_result, events.recv().await.unwrap())
    });
    assert!(completion_result.unwrap().is_some());
    assert!(event.error.is_none());
}

#[test]
pub(crate) fn epub_progress_is_derived_from_the_stable_opened_unit_locator() {
    let locators = vec![
        DocumentLocator::unit("book-1", "unit-a"),
        DocumentLocator::unit("book-1", "unit-b"),
    ];
    assert_eq!(
        reading_progress_for_locator(1, &locators, None),
        Some(ReadingProgressWrite::Unit {
            spine_index: 1,
            unit_id: "unit-b".to_string(),
            anchor: None,
        })
    );
    let anchor = TextAnchor {
        quote: "正文".to_string(),
        start: 4,
        end: 6,
    };
    assert_eq!(
        reading_progress_for_locator(0, &locators, Some(anchor.clone())),
        Some(ReadingProgressWrite::Unit {
            spine_index: 0,
            unit_id: "unit-a".to_string(),
            anchor: Some(anchor),
        })
    );
    assert_eq!(reading_progress_for_locator(2, &locators, None), None);
}

#[test]
pub(crate) fn ai_reference_options_include_current_then_all_chapters() {
    let spine = vec![
        ngy_book_studio::reader::SpineItem {
            href: "one.xhtml".to_string(),
            title: "One".to_string(),
        },
        ngy_book_studio::reader::SpineItem {
            href: "two.xhtml".to_string(),
            title: "Two".to_string(),
        },
        ngy_book_studio::reader::SpineItem {
            href: "three.xhtml".to_string(),
            title: "Three".to_string(),
        },
    ];
    let locators = vec![
        DocumentLocator::unit("book-a", "unit-1"),
        DocumentLocator::unit("book-a", "unit-2"),
        DocumentLocator::unit("book-a", "unit-3"),
    ];

    let references = reader_reference_hints("book-a", &spine, &locators, 1, None);

    assert_eq!(
        references
            .iter()
            .map(|reference| reference.unit_id.as_str())
            .collect::<Vec<_>>(),
        vec!["unit-2", "unit-1", "unit-3"]
    );
    assert!(references[0].label.starts_with("当前章节"));
    assert_eq!(references[0].locator.as_ref(), Some(&locators[1]));
}

#[test]
pub(crate) fn explanation_reference_requires_current_chapter_and_captures_only_selected_text() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(temp.path().join("library")).unwrap();
    let record = library.create_book("解释测试", "").unwrap();
    let book = OpenedBook::open_bytes(library.reader_epub_bytes(&record.id).unwrap()).unwrap();
    let document = library.document(&record.id).unwrap();
    let locators = document
        .units
        .iter()
        .map(|unit| DocumentLocator::unit(&record.id, &unit.id))
        .collect::<Vec<_>>();
    let url = OpenedBook::navigation_url_for_href(&book.spine[0].href);
    let reference = reader_explanation_reference(
        &record.id,
        &book,
        &locators,
        0,
        &url,
        "  所选\n\t文本  ",
        None,
    )
    .unwrap();
    assert_eq!(reference.frozen_text.as_deref(), Some("所选 文本"));
    assert_eq!(reference.locator.as_ref(), Some(&locators[0]));
    assert_eq!(reference.displayed_text, None);
    // A reading-time translation travels with the reference: the question
    // explains what the reader saw while the frozen quote stays the original.
    let translated = reader_explanation_reference(
        &record.id,
        &book,
        &locators,
        0,
        &url,
        "  所选\n\t文本  ",
        Some("  译文\n\t段落  "),
    )
    .unwrap();
    assert_eq!(translated.frozen_text.as_deref(), Some("所选 文本"));
    assert_eq!(translated.displayed_text.as_deref(), Some("译文 段落"));
    // An empty or oversized report must not reject the explanation.
    for displayed in [" \n\t ", &"x".repeat(MAX_READER_SELECTION_BYTES + 1)] {
        assert_eq!(
            reader_explanation_reference(
                &record.id,
                &book,
                &locators,
                0,
                &url,
                "  所选\n\t文本  ",
                Some(displayed),
            )
            .unwrap()
            .displayed_text,
            None
        );
    }
    for (spine_index, source_url, selection) in [
        (1, url.as_str(), "过期章节"),
        (0, "https://example.com/chapter.xhtml", "其它来源"),
        (0, "http://epubreader.book/missing.xhtml", "缺失章节"),
        (0, url.as_str(), " \n\t "),
    ] {
        assert!(
            reader_explanation_reference(
                &record.id,
                &book,
                &locators,
                spine_index,
                source_url,
                selection,
                None,
            )
            .is_none()
        );
    }
    assert!(
        reader_explanation_reference("another-book", &book, &locators, 0, &url, "越权文本", None)
            .is_none()
    );
}
