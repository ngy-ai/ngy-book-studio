//! Answer repair, correction budgets and stream interruption.

use super::*;

/// 响应少写一条片段：整块重发预算用尽，最后一轮的逐片段回退也只问了第一个文字叶并
/// 同样被拒，于是这一块保持原文、跳过 —— 绝不允许保存半段译文。
///
/// 跳过是终态，修复路径是明细里的按块重试，所以整次运行按 `Succeeded` 收尾、游标推进
/// 到书末。现场（2026-09-13，`block 79/80/81`）连续三块被判失败曾让整本图书失败；那个
/// 运行级中止条件已经取消（见 `docs/agent/code-map-ai.md`）。
#[test]
pub(crate) fn an_incomplete_response_is_never_persisted_as_a_partial_block() {
    let endpoint = TranslationEndpoint::start("错误：", ReplyMode::MissingId);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("invalid-response.epub");
    write_epub(&path, "<p>First <strong>second</strong> third.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let finished = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert!(finished.error.is_none());
        assert_eq!(
            finished.progress.completed, 1,
            "跳过的块与缓存命中一样推进游标"
        );
        let requests = endpoint.requests();
        let mut expected = vec![3; EXHAUSTED_BLOCK_ATTEMPTS];
        expected.push(1);
        assert_eq!(
            request_segment_counts(&requests),
            expected,
            "整块重发用满预算后，逐片段回退只问第一个文字叶，被拒即停"
        );
        assert_corrected_main_path(&endpoint);
        assert_eq!(row_count(&services), 0, "少一条片段的响应绝不能落库");
        assert!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id)
                .await
                .unwrap()
                .is_empty()
        );
        // 跳过必须留在明细里：它是用户之后按块重试的唯一入口。
        let logs = services
            .background_job_logs(finished.id, vec![book_id])
            .await
            .unwrap();
        let skipped: Vec<_> = logs
            .entries
            .iter()
            .filter(|entry| entry.event == JobLogEvent::ProtocolSkipped)
            .collect();
        assert_eq!(skipped.len(), 1, "跳过的文本块必须留下可读记录");
        assert_eq!(skipped[0].metrics.ordinal, Some(0));
    });
}

/// 片段 ID 重复的响应同属「模型答了、但片段不完整」：整块重发把预算用满都不能通过后，
/// 逐片段回退把三个文字叶各问一次，全部通过并按原顺序拼回一条完整的 `StoredTranslation`。
///
/// 回退是唯一一条「弱模型也能翻完」的路径，这条回归把它固定下来：请求形状、落库行数与
/// 片段边界空白都必须与主路径一致。整块重发排在前面是刻意的（重发带着上一轮的失败分类，
/// 换的是请求形状而不是只赌运气），回退只留给最后一轮。
#[test]
pub(crate) fn a_duplicate_id_response_is_repaired_one_fragment_at_a_time() {
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::DuplicateId);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("duplicate-id-response.epub");
    write_epub(&path, "<p>First <strong>second</strong> third.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let succeeded = translate(&services, &book_id).await;
        assert!(succeeded.error.is_none());
        let requests = endpoint.requests();
        let mut expected = vec![3; EXHAUSTED_BLOCK_ATTEMPTS];
        expected.extend([1, 1, 1]);
        assert_eq!(
            request_segment_counts(&requests),
            expected,
            "整块重发用满预算后，回退才改为每个文字叶各问一次"
        );
        assert_corrected_main_path(&endpoint);
        assert_eq!(row_count(&services), 1);
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].segments.len(), 3, "回退结果按原顺序拼回整块");
        let fragments = &requests[requests.len() - 3..];
        for (segment, request) in blocks[0].segments.iter().zip(fragments) {
            let leaf = request["segments"][0]["text"].as_str().unwrap();
            assert_eq!(segment.source, leaf, "回退按文字叶切分，顺序不变");
            let core = leaf.trim();
            assert_eq!(
                segment.translated,
                segment.source.replacen(core, &format!("译：{core}"), 1),
                "片段边界空白仍由原文恢复"
            );
        }
    });
}

#[test]
pub(crate) fn fenced_and_prefaced_structured_responses_succeed_without_correction() {
    for mode in [ReplyMode::Fenced, ReplyMode::Prefaced] {
        let endpoint = TranslationEndpoint::start("译：", mode);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wrapped-response.epub");
        write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            translate(&services, &book_id).await;
            let blocks = services
                .translation_blocks_for_unit(book_id, unit_id)
                .await
                .unwrap();
            assert_eq!(
                endpoint.requests().len(),
                1,
                "{mode:?} is a valid wrapped structured response"
            );
            assert_eq!(row_count(&services), 1);
            assert_translated_segments(&blocks, &endpoint.requests(), "译：");
        });
    }
}

#[test]
pub(crate) fn a_bare_segment_array_answer_is_accepted_without_a_correction() {
    // 现场（2026-09-12，qwen3.5:0.8b）：小模型丢掉了 `translations` 外壳，逐块返回被
    // 围栏包裹的顶层数组。旧协议把每个块都判成 invalid_schema，连续跳过 3 块后整本书
    // 的翻译任务直接失败（translation_run_finish result="failed"）。
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::BareSegmentArray);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bare-array-response.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(
            endpoint.requests().len(),
            1,
            "a bare segment array is a complete answer and must not need a correction"
        );
        assert_eq!(row_count(&services), 1);
        assert_translated_segments(&blocks, &endpoint.requests(), "译：");
    });
}

/// 手工门禁（默认 ignored）：把现场日志里的三个文本块原样交给真实本地模型，复跑一次
/// 真实的整本翻译。现场（2026-09-12 13:19，`qwen3.5:0.8b`）：模型丢掉了 `translations`
/// 外壳、逐块返回被围栏包裹的顶层数组，整本书的块全部判 `invalid_schema`，连续 3 块未译
/// 后 `translation_run_finish result="failed"`。
///
/// ```text
/// cargo test --test translation_flow -- --ignored --nocapture a_local_model
/// ```
///
/// 需要 127.0.0.1:11434 上的 Ollama（`NGY_REPLAY_ENDPOINT` / `NGY_REPLAY_MODEL`
/// 可覆盖）；`NGY_REPLAY_LOG` 调诊断级别。默认测试不运行它，也不触碰用户图书库。
#[test]
#[ignore = "manual gate: needs a local OpenAI-compatible model endpoint"]
pub(crate) fn a_local_model_replays_the_previously_rejected_blocks() {
    // 诊断行必须能直接在输出里看到，才能和现场日志逐条比对。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("NGY_REPLAY_LOG")
                .map(tracing_subscriber::EnvFilter::new)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ngy_ai=debug")),
        )
        .try_init();
    let base_url = std::env::var("NGY_REPLAY_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:11434/v1/".to_string());
    let model = std::env::var("NGY_REPLAY_MODEL").unwrap_or_else(|_| "qwen3.5:0.8b".to_string());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local-model-replay.epub");
    // 与现场日志逐字一致：连片段切分方式（inline 元素边界）都照抄。
    write_epub(
        &path,
        "<h1>Praise for <em>Head First Agile</em></h1>\
         <p>Praise for other <em>Head First books</em></p>\
         <p>Your name could be here! We’re looking for early praise from project managers, \
         developers, business anaylsts, and anyone else who’s read the early release of our book. \
         Contact us at <a href=\"mailto:info@stellman-greene.com\">info@stellman-greene.com</a>.</p>",
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        let listing: Value = reqwest::get(format!("{base_url}models"))
            .await
            .unwrap_or_else(|error| {
                panic!("本机没有可用的 OpenAI 兼容端点 {base_url}（先启动模型服务）：{error}")
            })
            .json()
            .await
            .unwrap();
        assert!(
            listing["data"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["id"] == model)),
            "端点没有 {model}：{listing}"
        );
        services
            .configure_providers(
                ProviderSettings {
                    base_url: base_url.clone(),
                    chat_model: model.clone(),
                    default_language: Some("zh-Hans".into()),
                    auto_run_background_jobs: false,
                    background_job_interval_ms: 0,
                    // 与现场一致：120 秒只是静默上限，不是整段请求的总时长。
                    request_timeout_secs: 120,
                    ..ProviderSettings::default()
                },
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id.clone(), unit_id)
            .await
            .unwrap();
        for block in &blocks {
            println!(
                "{} => {:?}",
                block.source,
                block
                    .segments
                    .iter()
                    .map(|segment| segment.translated.as_str())
                    .collect::<Vec<_>>()
            );
        }
        // 关键回归：现场那批块被拒的原因只有外壳。持久化任务日志里不允许再出现
        // `invalid_schema`；剩下的不足只能是「模型把多段合并成一段」的数量检查失败，
        // 那是协议该做的正当拒绝。
        let logs = services
            .background_job_logs(job.id.clone(), vec![book_id.clone()])
            .await
            .unwrap();
        let rejected = logs
            .entries
            .iter()
            .filter(|entry| entry.metrics.error_kind == Some(JobLogErrorKind::InvalidSchema))
            .map(|entry| entry.format_line())
            .collect::<Vec<_>>();
        assert!(
            rejected.is_empty(),
            "顶层数组外壳仍被判 invalid_schema：{rejected:?}"
        );
        // 两个 2 段块在旧协议下只会回 invalid_schema，而模型的 id 是齐的，必须落库。
        for source in [
            "Praise for Head First Agile",
            "Praise for other Head First books",
        ] {
            assert!(
                blocks.iter().any(|block| block.source == source),
                "{source:?} 必须有译文行，实际拿到：{:?}",
                blocks
                    .iter()
                    .map(|block| block.source.as_str())
                    .collect::<Vec<_>>()
            );
        }
        assert!(row_count(&services) >= 2);
    });
}

#[test]
pub(crate) fn malformed_or_plain_first_response_gets_one_immutable_structured_correction() {
    for mode in [ReplyMode::MalformedThenValid, ReplyMode::PlainThenValid] {
        let endpoint = TranslationEndpoint::start("修正：", mode);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("correctable-response.epub");
        write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
        let services = AppServices::open_with_credentials(
            temp.path().join("library"),
            Arc::new(MemoryCredentialStore::default()),
        )
        .unwrap();
        services.runtime().block_on(async {
            services
                .configure_providers(settings(&endpoint), BTreeMap::new())
                .await
                .unwrap();
            let (book_id, unit_id) = import(&services, &path).await;
            translate(&services, &book_id).await;
            let blocks = services
                .translation_blocks_for_unit(book_id, unit_id)
                .await
                .unwrap();
            assert_one_correction(&endpoint);
            assert_eq!(row_count(&services), 1);
            assert_translated_segments(&blocks, &endpoint.requests()[1..], "修正：");
        });
    }
}

/// 每一次应答都不合协议时：整块重发预算用尽后这一块保持原文、跳过，整次运行仍按
/// `Succeeded` 收尾 —— 修复路径是明细里的按块重试，不是让整本书失败。
///
/// 诊断里只允许出现固定分类：模型的原样输出（这里故意塞了 256 遍私有前缀）绝不能进
/// 任务错误、任务日志或数据库。
#[test]
pub(crate) fn an_exhausted_response_budget_skips_the_block_without_storing_provider_text() {
    let endpoint = TranslationEndpoint::start("无效：", ReplyMode::AlwaysInvalid);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("exhausted-response.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        let job = wait_translation(&services, &book_id, BackgroundJobStatus::Paused).await;
        assert!(
            services
                .control_background_job(job.id, BackgroundJobAction::Resume)
                .await
                .unwrap()
        );
        let finished = wait_translation(&services, &book_id, BackgroundJobStatus::Succeeded).await;
        assert!(
            finished.error.is_none(),
            "协议类失败只让这一块跳过，不该让整次运行失败"
        );
        let requests = endpoint.requests();
        assert_eq!(
            request_segment_counts(&requests),
            vec![3; EXHAUSTED_BLOCK_ATTEMPTS],
            "JSON 层面答不出来时换请求形状无用：只重发整块，不走逐片段回退"
        );
        assert_eq!(finished.progress.completed, 1, "跳过的块与缓存命中一样推进游标");
        assert_eq!(row_count(&services), 0, "不合格的响应绝不能落库");
        assert!(
            services
                .translation_blocks_for_unit(book_id.clone(), unit_id)
                .await
                .unwrap()
                .is_empty()
        );
        // 持久化错误与任务日志都必须与 provider 文本无关。
        let persisted_error: String = open_fixture_conn(services.database_path())
            .query_row(
                "SELECT coalesce(error, '') FROM index_jobs WHERE book_id = ?1 AND kind = 'translation'",
                [&book_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!persisted_error.contains(INVALID_RESPONSE_SENTINEL));
        assert!(
            persisted_error.len() < 1024,
            "job error must remain bounded and independent of provider text"
        );
        let logs = services
            .background_job_logs(finished.id, vec![book_id])
            .await
            .unwrap();
        assert!(
            logs.entries
                .iter()
                .any(|entry| entry.event == JobLogEvent::ProtocolSkipped),
            "跳过的块必须留下记录"
        );
        for entry in &logs.entries {
            let line = entry.format_line();
            assert!(
                !line.contains(INVALID_RESPONSE_SENTINEL)
                    && !line.contains("仅供隔离测试的无效模型前缀"),
                "任务日志不得包含模型原文：{line}"
            );
        }
    });
}

/// 2026-09-11 现场：本地模型的流在 120 秒请求超时里已经输出了完整译文，却一直没有
/// 结束（1579 个正文块、7024 字节正文、没有 `[DONE]`），旧的整体请求超时把整本图书
/// 判成失败。请求超时改为「静默」上限后，已经收到的完整答案必须被采用。
#[test]
pub(crate) fn a_complete_answer_survives_a_stream_that_never_finishes() {
    let endpoint = TranslationEndpoint::start("译：", ReplyMode::AnswerThenSilence);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unfinished-stream.epub");
    write_epub(&path, "<p>Read <strong>carefully</strong>.</p>");
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings_with_timeout(&endpoint, 1), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(
            endpoint.requests().len(),
            1,
            "a salvaged answer must not be requested a second time"
        );
        assert_eq!(row_count(&services), 1);
        assert_translated_segments(&blocks, &endpoint.requests(), "译：");
    });
}

#[test]
pub(crate) fn full_width_structure_punctuation_is_repaired_without_a_correction() {
    // 现场成因：模型把 JSON 的结构冒号、逗号写成全角标点。归一化只改结构位置，
    // 因此不需要纠正请求，译文内容逐字保持模型给出的文本。
    let endpoint = TranslationEndpoint::start("全角：", ReplyMode::FullWidthStructure);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("full-width-structure.epub");
    write_epub(
        &path,
        "<p>Read <strong>carefully</strong>.</p><p>Second <em>block</em>.</p>",
    );
    let services = AppServices::open_with_credentials(
        temp.path().join("library"),
        Arc::new(MemoryCredentialStore::default()),
    )
    .unwrap();
    services.runtime().block_on(async {
        services
            .configure_providers(settings(&endpoint), BTreeMap::new())
            .await
            .unwrap();
        let (book_id, unit_id) = import(&services, &path).await;
        translate(&services, &book_id).await;
        let blocks = services
            .translation_blocks_for_unit(book_id, unit_id)
            .await
            .unwrap();
        assert_eq!(
            endpoint.requests().len(),
            2,
            "结构标点归一后不需要额外纠正请求"
        );
        assert_eq!(row_count(&services), 2);
        assert_translated_segments(&blocks, &endpoint.requests(), "全角：");
    });
}
