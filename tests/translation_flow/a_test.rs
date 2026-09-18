//! Restore, manual override, chapter editing and incomplete responses.

use super::*;

#[test]
pub(crate) fn structured_translation_survives_restart_and_a_changed_endpoint_keeps_its_text() {
    let first = TranslationEndpoint::start("甲：", ReplyMode::ReverseIds);
    let second = TranslationEndpoint::start("乙：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("formatting.epub");
    write_epub(&path, FORMATTED_BODY);
    let data_dir = temp.path().join("library");
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(&data_dir, credentials.clone()).unwrap();
    let (book_id, unit_id, initial) = services.runtime().block_on(async {
        services
            .configure_providers(settings(&first), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        let requests = first.requests();
        assert_eq!(
            requests.len(),
            10,
            "heading, paragraphs, list items, and table cells"
        );
        assert_translated_segments(&blocks, &requests, "甲：");
        let paragraph = requests
            .iter()
            .find(|request| request["source"].as_str().unwrap().starts_with("Start "))
            .unwrap();
        let texts = paragraph["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment["text"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            [
                "Start ", "bold", " and ", "italic", ".", "Next ", "colored", " and ", " end."
            ]
        );
        for (source, expected) in [
            ("Read this now", vec!["Read ", "this", " now"]),
            ("adjacentparts", vec!["adjacent", "parts"]),
        ] {
            let request = requests
                .iter()
                .find(|request| request["source"] == source)
                .unwrap();
            assert_eq!(
                request["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|segment| segment["text"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected,
                "the actual EPUB source text leaves must survive AST normalization"
            );
        }
        assert!(
            paragraph["source"]
                .as_str()
                .unwrap()
                .contains("do_not_translate()"),
            "code remains in context"
        );
        assert!(
            requests
                .iter()
                .flat_map(|request| request["segments"].as_array().unwrap())
                .all(|segment| !segment["text"]
                    .as_str()
                    .unwrap()
                    .contains("do_not_translate")
                    && !segment["text"]
                        .as_str()
                        .unwrap()
                        .contains("leave this code unchanged"))
        );
        assert_eq!(row_count(&services), 10);
        (book_id, unit_id, blocks)
    });
    drop(services);

    let services = AppServices::open_with_credentials(&data_dir, credentials).unwrap();
    services.runtime().block_on(async {
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap(),
            initial
        );
        wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert_eq!(
            first.requests().len(),
            10,
            "current persisted output must not be requested again"
        );

        services
            .configure_providers(settings(&second), BTreeMap::new())
            .await
            .unwrap();
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        // A changed endpoint or model keeps what is already translated: the rows
        // are restamped with the new identity instead of being discarded, so a
        // reader never loses译文 for a book that was already translated.
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap(),
            initial
        );
        assert_eq!(
            row_count(&services),
            10,
            "existing译文 must survive a changed endpoint"
        );
        assert_eq!(
            second.requests().len(),
            0,
            "already translated blocks are not requested again"
        );

        // The explicit re-translation still discards the stored text and rebuilds
        // the book with the new endpoint.
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Retranslate)
                .await
                .unwrap()
        );
        assert_eq!(row_count(&services), 0);
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_translated_segments(&blocks, &second.requests(), "乙：");
        assert_eq!(second.requests().len(), 10);
        assert_eq!(first.requests().len(), 10);
        assert_eq!(
            row_count(&services),
            10,
            "retranslation replaces rows without duplication"
        );
    });
}

/// 阅读窗口可以手工改写某一段的机器译文。手工译文必须是读者看到的正文，不能被后续
/// 机器运行覆盖，重启与换端点后仍然有效，并且机器原文行始终保留在它背后，所以
/// 「恢复机器译文」不需要重新调用模型。
#[test]
pub(crate) fn a_manual_translation_overrides_the_model_text_and_survives_a_changed_endpoint() {
    let first = TranslationEndpoint::start("译：", ReplyMode::ReverseIds);
    let second = TranslationEndpoint::start("再：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("manual.epub");
    write_epub(
        &path,
        "<p>Manual <strong>edit</strong> target</p><p>Second paragraph</p>",
    );
    let data_dir = temp.path().join("library");
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(&data_dir, credentials.clone()).unwrap();
    let (book_id, unit_id, key, machine, manual) = services.runtime().block_on(async {
        services
            .configure_providers(settings(&first), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|block| !block.manual));
        let target = blocks
            .iter()
            .find(|block| block.source.starts_with("Manual"))
            .unwrap();
        assert_eq!(
            target.segments.len(),
            3,
            "formatting leaves become separate segments"
        );
        let machine = target.segments.clone();
        let untouched = blocks
            .iter()
            .filter(|block| block.key != target.key)
            .cloned()
            .collect::<Vec<_>>();

        // 页面只能描述改动：与已存片段对不上的请求一律拒绝，且必须一个字节都不写。
        let mut rewritten = machine.clone();
        rewritten[0].source = "Rewritten".into();
        let mut short = machine.clone();
        short.pop();
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(rewritten),
                )
                .await
                .is_err(),
            "a rewritten source must be refused"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(short),
                )
                .await
                .is_err(),
            "a truncated segment list must be refused"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(vec![machine[0].clone(); 513]),
                )
                .await
                .is_err(),
            "an oversized payload must be refused before any database work"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    "missing-block".into(),
                    Some(machine.clone()),
                )
                .await
                .is_err(),
            "a block without a stored row must be refused"
        );
        assert!(
            services
                .set_manual_translation(
                    book_id.clone(),
                    unit_id.clone(),
                    target.key.clone(),
                    Some(Vec::new())
                )
                .await
                .is_err(),
            "an empty edit must be refused"
        );
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
                .await
                .unwrap(),
            blocks,
            "a refused edit leaves the stored translation untouched"
        );

        let manual = machine
            .iter()
            .map(|segment| TranslationSegment {
                source: segment.source.clone(),
                translated: format!("手工：{}", segment.translated.trim()),
            })
            .collect::<Vec<_>>();
        services
            .set_manual_translation(
                book_id.clone(),
                unit_id.clone(),
                target.key.clone(),
                Some(manual.clone()),
            )
            .await
            .unwrap();
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_eq!(
            blocks.iter().filter(|block| block.manual).count(),
            1,
            "only the edited block is manual"
        );
        let edited = blocks.iter().find(|block| block.key == target.key).unwrap();
        assert!(edited.manual);
        assert_eq!(edited.segments, manual);
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.key != target.key)
                .cloned()
                .collect::<Vec<_>>(),
            untouched,
            "the other block keeps its machine text"
        );
        assert_eq!(row_count(&services), 2, "a manual edit adds no row");
        (book_id, unit_id, target.key.clone(), machine, manual)
    });
    drop(services);

    let services = AppServices::open_with_credentials(&data_dir, credentials).unwrap();
    services.runtime().block_on(async {
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        let edited = blocks.iter().find(|block| block.key == key).unwrap();
        assert!(edited.manual, "a manual edit survives a restart");
        assert_eq!(edited.segments, manual);

        services
            .configure_providers(settings(&second), BTreeMap::new())
            .await
            .unwrap();
        wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert_eq!(
            second.requests().len(),
            0,
            "a manually edited block is still a cache hit for the model"
        );
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        assert_eq!(
            blocks
                .iter()
                .find(|block| block.key == key)
                .unwrap()
                .segments,
            manual,
            "a changed endpoint does not hide the reader's own text"
        );

        services
            .set_manual_translation(book_id.clone(), unit_id.clone(), key.clone(), None)
            .await
            .unwrap();
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id.clone())
            .await
            .unwrap();
        let restored = blocks.iter().find(|block| block.key == key).unwrap();
        assert!(!restored.manual, "restoring drops the manual marker");
        assert_eq!(
            restored.segments, machine,
            "the machine text was kept behind the manual edit"
        );
        assert_eq!(
            second.requests().len(),
            0,
            "restoring does not spend a model call"
        );
        assert_eq!(row_count(&services), 2);
    });
}

#[test]
pub(crate) fn editing_one_chapter_keeps_the_other_chapters_translation() {
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::ReverseIds);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("two-chapters.epub");
    write_two_chapter_epub(
        &path,
        "<p>first chapter words</p>",
        "<p>second chapter words</p>",
    );
    let data_dir = temp.path().join("library");
    let services =
        AppServices::open_with_credentials(&data_dir, Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, first_unit_id) = import(&services, &path).await;
        let second_unit_id = services
            .spawn_library_read({
                let book_id = book_id.clone();
                move |library| Ok(library.document(&book_id)?.units[1].id.clone())
            })
            .await
            .unwrap()
            .unwrap();
        translate(&services, &book_id).await;
        assert_eq!(endpoint.requests().len(), 2, "one block per chapter");
        let kept = services
            .translation_blocks_for_unit(book_id.clone(), second_unit_id.clone())
            .await
            .unwrap();
        assert_eq!(kept.len(), 1);

        // Editing the first chapter publishes a new revision, but the untouched
        // chapter keeps its译文 and is not translated again.
        let edited_book = book_id.clone();
        let edited_unit = first_unit_id.clone();
        services
            .spawn_library(move |library| {
                library.update_content_unit_source(
                    &edited_book,
                    &edited_unit,
                    "<p>edited chapter words</p>",
                )
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            services
                .translation_blocks_for_unit(book_id.clone(), second_unit_id.clone())
                .await
                .unwrap(),
            kept,
            "the untouched chapter keeps its译文"
        );

        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        let before_edit_requests = endpoint.requests().len();
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        let requested = endpoint
            .requests()
            .iter()
            .skip(before_edit_requests)
            .map(|request| request["source"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(
            requested
                .iter()
                .any(|source| source == "edited chapter words"),
            "the edited chapter is translated again: {requested:?}"
        );
        assert!(
            !requested
                .iter()
                .any(|source| source == "second chapter words"),
            "an untouched chapter's own text is never translated again: {requested:?}"
        );
        let kept_after = services
            .translation_blocks_for_unit(book_id.clone(), second_unit_id.clone())
            .await
            .unwrap();
        assert!(
            kept_after
                .iter()
                .any(|block| block.source == "second chapter words"),
            "the untouched chapter still reads its译文: {kept_after:?}"
        );
    });
}
