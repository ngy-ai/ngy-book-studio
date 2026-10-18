use super::*;

/// 阅读窗口的目录标签：按「条目指向的那一章 + 归一化后的标签文本」取当前译文，读者自己
/// 改过的那一份优先；换模型/换端点的机器行、其它章节的同名文字都不能顶替本章的标签，
/// 没有匹配就交还书里自己的写法（`None`）。
#[test]
fn toc_labels_prefer_the_entries_own_chapter_and_the_current_engine() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let settings = ProviderSettings {
        base_url: "https://models.example.test/v1".to_string(),
        chat_model: "chat-test".to_string(),
        translation_model: "chat-test".to_string(),
        default_language: Some("zh-Hans".to_string()),
        remote_content_confirmed: true,
        confirmed_remote_endpoint: "https://models.example.test/v1/".to_string(),
        auto_run_background_jobs: false,
        ..Default::default()
    };
    let identity = translation_execution_identity(&settings).unwrap();
    let (book_id, source_id, unit_id, document_revision, unit_revision) = services
        .runtime()
        .block_on(async {
            services
                .configure_providers(settings.clone(), BTreeMap::new())
                .await?;
            let outcome = services
                .spawn_library(|library| Ok(library.create_book("Toc book", "Author")?.id))
                .await
                .context("library test worker stopped")?;
            let book_id = outcome?;
            let conn = db::open_conn(services.database_path())?;
            let book = db::books::get(&conn, &book_id)?.context("the created book disappeared")?;
            let source = db::book_sources::list_for_book(&conn, &book_id)?
                .into_iter()
                .next()
                .context("the created book has no source")?;
            let unit = db::content_units::list_for_source(&conn, &source.id)?
                .into_iter()
                .next()
                .context("the created book has no chapter")?;
            Ok::<_, anyhow::Error>((
                book_id,
                source.id.clone(),
                unit.id.clone(),
                book.revision,
                unit.revision,
            ))
        })
        .unwrap();

    let stored = |identity: &str, segments: Vec<(&str, &str)>| {
        serde_json::to_string(&crate::ai::translation::StoredTranslation {
            execution_identity: identity.to_string(),
            segments: segments
                .into_iter()
                .map(
                    |(source, translated)| crate::ai::translation::TranslationSegment {
                        source: source.to_string(),
                        translated: translated.to_string(),
                    },
                )
                .collect(),
        })
        .unwrap()
    };
    // 第二本书内章节：与第一章同名的小节（「Exercises」在任何书里都会重复）用它来验证
    // 「条目指向哪一章就用哪一章的译文」。
    let second_unit = "unit-toc-second";
    {
        let conn = db::open_conn(&services.db_path).unwrap();
        conn.execute(
            "INSERT INTO content_units(id, book_id, source_id, ordinal, kind,
                source_locator_json, block_json, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, 'chapter', '{}',
                     '{\"schema_version\":1,\"blocks\":[]}', ?4, 1, 1)",
            rusqlite::params![second_unit, &book_id, &source_id, unit_revision as i64],
        )
        .unwrap();
    }
    let write = |unit: &str, block_id: &str, ordinal: u64, source: &str, translated: String| {
        let conn = db::open_conn(&services.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: book_id.clone(),
                content_unit_id: unit.to_string(),
                block_id: block_id.to_string(),
                ordinal,
                document_revision,
                unit_revision,
                target_language: "zh-Hans".to_string(),
                source_language: None,
                model: "chat-test".to_string(),
                source_text: source.to_string(),
                translated_text: translated,
                created_at: 10,
                updated_at: 10,
            },
        )
        .unwrap();
    };
    let stale = format!(
        "{}:stale",
        crate::ai::translation::EXECUTION_IDENTITY_PROTOCOL
    );
    // 本章标题：一个片段的机器译文。
    write(
        &unit_id,
        "block-title",
        0,
        "Linear Algebra Intuition",
        stored(
            &identity,
            vec![("Linear Algebra Intuition", "线性代数的直观理解")],
        ),
    );
    // 带内联样式的标题：两个片段，拼出来必须保留机器行里的叶子空白。
    write(
        &unit_id,
        "block-inline",
        1,
        "Basis and Rank",
        stored(&identity, vec![("Basis ", "基 "), ("and Rank", "与秩")]),
    );
    // 换了端点的机器行：这一条没有可显示的译文。
    write(
        &unit_id,
        "block-stale",
        2,
        "The Concept",
        stored(&stale, vec![("The Concept", "概念")]),
    );
    // 读者自己改过的标题：手工文本优先。
    write(
        &unit_id,
        "block-manual",
        3,
        "Exercises",
        stored(&identity, vec![("Exercises", "练习")]),
    );
    {
        let manual = stored(&identity, vec![("Exercises", "动手练一练")]);
        let conn = db::open_conn(&services.db_path).unwrap();
        db::translations::set_manual_text(
            &conn,
            &book_id,
            &unit_id,
            "block-manual",
            "zh-Hans",
            document_revision,
            unit_revision,
            Some(manual.as_str()),
            20,
        )
        .unwrap();
    }
    // 另一章的同名小节，先写入（全局兜底取最靠前的行）。
    write(
        second_unit,
        "block-second",
        0,
        "Exercises",
        stored(&identity, vec![("Exercises", "另一章的练习")]),
    );

    let labels = vec![
        ReaderTocLabel {
            content_unit_id: Some(unit_id.clone()),
            text: "Linear Algebra Intuition".to_string(),
        },
        // 书内导航的空白写法与正文不同，归一化后必须落到同一把键上。
        ReaderTocLabel {
            content_unit_id: Some(unit_id.clone()),
            text: "  Basis\n  and   Rank ".to_string(),
        },
        ReaderTocLabel {
            content_unit_id: Some(unit_id.clone()),
            text: "The Concept".to_string(),
        },
        ReaderTocLabel {
            content_unit_id: Some(unit_id.clone()),
            text: "Exercises".to_string(),
        },
        ReaderTocLabel {
            content_unit_id: Some(second_unit.to_string()),
            text: "Exercises".to_string(),
        },
        ReaderTocLabel {
            content_unit_id: None,
            text: "Exercises".to_string(),
        },
        ReaderTocLabel {
            content_unit_id: None,
            text: "Not A Block".to_string(),
        },
    ];
    let translated =
        block_on_without_tokio(services.translated_toc_labels(book_id.clone(), labels.clone()))
            .unwrap();
    assert_eq!(
        translated,
        vec![
            Some("线性代数的直观理解".to_string()),
            Some("基 与秩".to_string()),
            None,
            Some("动手练一练".to_string()),
            Some("另一章的练习".to_string()),
            Some("动手练一练".to_string()),
            None,
        ],
        "本章的译文优先，读者自己的改法优先，没有译文的条目保持书里的写法"
    );

    // 目标语言缺失：整本都不显示译文，而不是退回一句猜测。
    let mut without_language = settings.clone();
    without_language.default_language = None;
    block_on_without_tokio(services.configure_providers(without_language, BTreeMap::new()))
        .unwrap();
    let translated =
        block_on_without_tokio(services.translated_toc_labels(book_id.clone(), labels.clone()))
            .unwrap();
    assert!(translated.iter().all(Option::is_none));

    // 图书语言已经是目标语言：这本书不该有任何译文层，目录也不例外。
    block_on_without_tokio(services.configure_providers(settings, BTreeMap::new())).unwrap();
    {
        let conn = db::open_conn(&services.db_path).unwrap();
        conn.execute(
            "UPDATE books SET language = 'zh-Hans' WHERE id = ?1",
            [&book_id],
        )
        .unwrap();
    }
    let translated =
        block_on_without_tokio(services.translated_toc_labels(book_id, labels)).unwrap();
    assert!(translated.iter().all(Option::is_none));
}
