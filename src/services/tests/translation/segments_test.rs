use super::*;

/// 手工译文优先于机器译文，并且不受机器行的模型 / 端点身份影响：读者自己写下的
/// 文字不能因为换模型或换端点而消失，机器行仍然留在它背后作为「恢复机器译文」
/// 的来源。身份不匹配的机器行照旧不显示。
#[test]
fn a_manual_translation_is_shown_independently_of_the_machine_identity() {
    let temp = tempfile::tempdir().unwrap();
    let services =
        AppServices::open_with_credentials(temp.path(), Arc::new(MemoryCredentialStore::default()))
            .unwrap();
    let settings = ProviderSettings {
        base_url: "https://models.example.test/v1".to_string(),
        chat_model: "chat-test".to_string(),
        // 译文行按翻译模型过滤，所以这里必须与上面写入行的模型一致。
        translation_model: "chat-test".to_string(),
        default_language: Some("zh-Hans".to_string()),
        remote_content_confirmed: true,
        confirmed_remote_endpoint: "https://models.example.test/v1/".to_string(),
        auto_run_background_jobs: false,
        ..Default::default()
    };
    let identity = translation_execution_identity(&settings).unwrap();
    let (book_id, unit_id, document_revision, unit_revision) = services
        .runtime()
        .block_on(async {
            services
                .configure_providers(settings.clone(), BTreeMap::new())
                .await?;
            let outcome = services
                .spawn_library(|library| Ok(library.create_book("Manual book", "Author")?.id))
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
            Ok::<_, anyhow::Error>((book_id, unit.id.clone(), book.revision, unit.revision))
        })
        .unwrap();

    let stored = |identity: &str, block_id: &str, translated: String| {
        serde_json::to_string(&crate::ai::translation::StoredTranslation {
            execution_identity: identity.to_string(),
            segments: vec![crate::ai::translation::TranslationSegment {
                source: format!("source-{block_id}"),
                translated,
            }],
        })
        .unwrap()
    };
    let write =
        |block_id: &str, ordinal: u64, model: &str, row_identity: &str, manual: Option<String>| {
            let conn = db::open_conn(&services.db_path).unwrap();
            db::translations::upsert(
                &conn,
                &db::translations::NewTranslation {
                    book_id: book_id.clone(),
                    content_unit_id: unit_id.clone(),
                    block_id: block_id.to_string(),
                    ordinal,
                    document_revision,
                    unit_revision,
                    target_language: "zh-Hans".to_string(),
                    source_language: None,
                    model: model.to_string(),
                    source_text: format!("source-{block_id}"),
                    translated_text: stored(row_identity, block_id, format!("machine-{block_id}")),
                    created_at: 10,
                    updated_at: 10,
                },
            )
            .unwrap();
            if let Some(manual) = manual {
                db::translations::set_manual_text(
                    &conn,
                    &book_id,
                    &unit_id,
                    block_id,
                    "zh-Hans",
                    document_revision,
                    unit_revision,
                    Some(&manual),
                    11,
                )
                .unwrap();
            }
        };
    let stale = format!(
        "{}:stale",
        crate::ai::translation::EXECUTION_IDENTITY_PROTOCOL
    );

    // 同一模型同一身份：手工与机器各行其是。
    write(
        "block-a",
        0,
        "chat-test",
        &identity,
        Some(stored(&identity, "block-a", "manual-a".into())),
    );
    write("block-b", 1, "chat-test", &identity, None);
    // 换了模型：手工译文照旧显示，机器译文按既有规则不显示。
    write(
        "block-c",
        2,
        "other-model",
        &identity,
        Some(stored(&identity, "block-c", "manual-c".into())),
    );
    write("block-d", 3, "other-model", &identity, None);
    // 同名模型换了端点（身份不同）：同上。
    write(
        "block-e",
        4,
        "chat-test",
        &stale,
        Some(stored(&stale, "block-e", "manual-e".into())),
    );
    write("block-f", 5, "chat-test", &stale, None);
    // 手工译文不可解析时退回机器译文，不丢整章。
    write(
        "block-g",
        6,
        "chat-test",
        &identity,
        Some("not json".to_string()),
    );
    // 章节版本已经变化的手工译文同样不显示。
    {
        let conn = db::open_conn(&services.db_path).unwrap();
        db::translations::upsert(
            &conn,
            &db::translations::NewTranslation {
                book_id: book_id.clone(),
                content_unit_id: unit_id.clone(),
                block_id: "block-h".to_string(),
                ordinal: 7,
                document_revision,
                unit_revision: unit_revision + 1,
                target_language: "zh-Hans".to_string(),
                source_language: None,
                model: "chat-test".to_string(),
                source_text: "source-block-h".to_string(),
                translated_text: stored(&identity, "block-h", "machine-block-h".into()),
                created_at: 10,
                updated_at: 10,
            },
        )
        .unwrap();
    }

    let blocks = block_on_without_tokio(
        services.translation_blocks_for_unit(book_id.clone(), unit_id.clone()),
    )
    .unwrap();
    let shape = blocks
        .iter()
        .map(|block| {
            (
                block.key.clone(),
                block.manual,
                block
                    .segments
                    .first()
                    .map(|segment| segment.translated.clone()),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shape,
        vec![
            ("block-a".to_string(), true, Some("manual-a".to_string())),
            (
                "block-b".to_string(),
                false,
                Some("machine-block-b".to_string())
            ),
            ("block-c".to_string(), true, Some("manual-c".to_string())),
            ("block-e".to_string(), true, Some("manual-e".to_string())),
            (
                "block-g".to_string(),
                false,
                Some("machine-block-g".to_string())
            ),
        ],
        "a manual edit outlives the model identity; a stale machine row still does not"
    );
    assert_eq!(
        db::translations::list_for_unit(
            &db::open_conn(&services.db_path).unwrap(),
            &unit_id,
            "zh-Hans",
        )
        .unwrap()
        .iter()
        .filter(|row| row.manual_text.is_some())
        .count(),
        4,
        "displaying a manual edit never rewrites the rows behind it"
    );

    // 恢复机器译文只清掉手工列，机器文本原样返回。
    block_on_without_tokio(services.set_manual_translation(
        book_id.clone(),
        unit_id.clone(),
        "block-e".to_string(),
        None,
    ))
    .unwrap();
    let blocks = block_on_without_tokio(
        services.translation_blocks_for_unit(book_id.clone(), unit_id.clone()),
    )
    .unwrap();
    assert!(
        blocks.iter().all(|block| block.key != "block-e"),
        "the machine text of the wrong identity is not resurrected by a restore"
    );
    block_on_without_tokio(services.set_manual_translation(
        book_id.clone(),
        unit_id.clone(),
        "block-a".to_string(),
        None,
    ))
    .unwrap();
    let blocks = block_on_without_tokio(
        services.translation_blocks_for_unit(book_id.clone(), unit_id.clone()),
    )
    .unwrap();
    assert_eq!(
        blocks
            .iter()
            .find(|block| block.key == "block-a")
            .map(|block| block.manual),
        Some(false),
        "restoring falls back to the machine text of the current identity"
    );
}

/// 手工译文请求与机器行的分段形状：机器行记录的是写入时的章节结构，来源被重新导入后
/// 浏览器投影可能并掉内联包装，页面因此拿着更少的叶子来保存。请求只要把机器片段按顺序
/// 整段铺满就接受，落库的原文始终取自机器行；切开机器片段、改写原文或留下没覆盖的片段
/// 一律拒绝，一个字节都不写。
#[test]
fn manual_segments_tile_the_stored_split_without_ever_rewriting_it() {
    let segment = |source: &str, translated: &str| crate::ai::translation::TranslationSegment {
        source: source.to_string(),
        translated: translated.to_string(),
    };
    let machine = crate::ai::translation::StoredTranslation {
        execution_identity: "translation-v2:test".to_string(),
        segments: vec![
            segment(
                "Grounding: trained on image-text pairs. “The cat is at ",
                "定位：在图文对上训练。“猫在",
            ),
            segment("(112, 204)", "(112, 204)"),
            segment("”.", "”。"),
        ],
    };
    let leaf = "Grounding: trained on image-text pairs. “The cat is at (112, 204)”.";

    // 页面的叶子比机器行更粗：三段拼回一段，原文取自机器行。
    assert_eq!(
        manual_translation_segments(
            &machine,
            &[segment(leaf, "定位：训练完成。“猫在(112, 204)”。")]
        )
        .unwrap(),
        vec![segment(leaf, "定位：训练完成。“猫在(112, 204)”。")],
        "a request that tiles the stored split keeps the machine sources"
    );
    // 一一对应照旧：只有译文被替换，首尾空白也照旧由机器行提供。
    assert_eq!(
        manual_translation_segments(
            &machine,
            &[
                segment(
                    "Grounding: trained on image-text pairs. “The cat is at",
                    "甲 "
                ),
                segment("(112, 204)", "乙"),
                segment("”.", "丙"),
            ],
        )
        .unwrap(),
        vec![
            segment(
                "Grounding: trained on image-text pairs. “The cat is at ",
                "甲"
            ),
            segment("(112, 204)", "乙"),
            segment("”.", "丙"),
        ],
        "a one-to-one request still takes every source from the stored row"
    );
    // 只覆盖了开头一段：后面的机器原文没有人覆盖，正文结构已经变过。
    assert!(
        manual_translation_segments(
            &machine,
            &[segment(
                "Grounding: trained on image-text pairs. “The cat is at ",
                "定位"
            )],
        )
        .is_err(),
        "an uncovered stored segment is refused"
    );
    // 切开了一段机器原文：它的文字无法再被分开保存。
    assert!(
        manual_translation_segments(
            &machine,
            &[
                segment("Grounding: trained on image-text pairs. “", "定位"),
                segment("The cat is at (112, 204)”,", "猫在"),
            ],
        )
        .is_err(),
        "a request cut inside a stored segment is refused"
    );
    // 原文被改写：页面只能描述改动，不能替换它依附的正文。
    assert!(
        manual_translation_segments(&machine, &[segment("Rewritten", "定位")]).is_err(),
        "a rewritten source is refused"
    );
}
