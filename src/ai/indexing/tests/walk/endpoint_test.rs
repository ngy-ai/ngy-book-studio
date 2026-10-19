use super::*;

/// 运行期间保存了新的 Endpoint/密钥（模型与执行身份不变）时，剩下的块必须换用新 provider，
/// 而不是用领取时的快照把整本书翻完。
///
/// 现场 2026-09-24：改完 Endpoint 与 API key 后，正在跑的任务仍用旧值翻完剩下的块 ——
/// `configure_translation` 写的是 `models.translation`，而一次运行手里握的是领取时的快照。
#[test]
pub(crate) fn a_running_translation_switches_to_the_saved_endpoint_and_key() {
    let before = Arc::new(MockMergeProvider::default());
    // 第一发请求慢一点：好在它还在飞的时候「保存」新配置，观察后面几块走了谁。
    before.delay_ms.store(300, Ordering::SeqCst);
    let after = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&before, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    let mut stats = TranslationRunStats::default();

    // 「保存设置」走真实入口：`configure_providers` → `configure_translation`。模型与执行
    // 身份不变（只是换了密钥/地址之外的连接参数），任务行因此不会被重置。
    let swapped = {
        let inner = Arc::clone(&inner);
        let coordinator = IndexingCoordinator {
            inner: Arc::clone(&inner),
            workers: Mutex::new(Vec::new()),
        };
        let waiting_on = Arc::clone(&before);
        let saved = Arc::clone(&after);
        let model = translation.model.clone();
        let execution_identity = translation.execution_identity.clone();
        async move {
            while waiting_on.requests.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            coordinator
                .configure_translation(
                    saved,
                    model,
                    execution_identity,
                    TranslationGenerationSettings::default(),
                    NO_BATCHING,
                    Some("zh-Hans".to_string()),
                    false,
                )
                .await
                .unwrap();
        }
    };
    let outcome = fixture.runtime.block_on(async {
        let run = run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        );
        let (outcome, ()) = futures_util::join!(run, swapped);
        outcome.expect("换 provider 不该让运行失败")
    });

    let sent = before.requests.load(Ordering::SeqCst) + after.requests.load(Ordering::SeqCst);
    assert!(sent >= 2, "这条用例需要至少两个文本块，实际 {sent}");
    assert_eq!(
        before.requests.load(Ordering::SeqCst),
        1,
        "只有换配置时已经在飞的那一块走旧 provider"
    );
    assert_eq!(
        after.requests.load(Ordering::SeqCst),
        sent - 1,
        "剩下的块必须走保存后的新 provider"
    );
    assert_eq!(stats.saved, sent, "每一块都要翻出来");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("换 provider 之后运行照旧按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, sent);
    assert_eq!(stats.untranslated, 0);
}

/// 保存的配置属于另一套引擎（模型、执行身份或目标语言变了）时，本次运行不换 provider：
/// 那种重配置会把任务行整个重置，本次运行在下一个控制检查里就被判成已被替换，重新领取时
/// 才按新引擎继续 —— 半路换身份只会把两种来源的译文混进同一次运行。
#[test]
pub(crate) fn a_running_translation_ignores_a_saved_service_with_another_identity() {
    let before = Arc::new(MockMergeProvider::default());
    before.delay_ms.store(300, Ordering::SeqCst);
    let after = Arc::new(MockMergeProvider::default());
    let (fixture, job, mut cursor, translation) =
        translation_block_fixture(&before, "translation-v2:chat-test");
    let inner = fixture.indexing_inner();
    IndexingCoordinator {
        inner: Arc::clone(&inner),
        workers: Mutex::new(Vec::new()),
    }
    .configure_scheduling(1, Duration::ZERO);
    // 换了地址（执行身份随之改变）的那次保存。
    let other_engine = TranslationServices {
        provider: after.clone(),
        execution_identity: "translation-v2:another-endpoint".to_string(),
        ..translation.clone()
    };
    let mut stats = TranslationRunStats::default();

    let swapped = {
        let inner = Arc::clone(&inner);
        let waiting_on = Arc::clone(&before);
        async move {
            while waiting_on.requests.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let mut models = inner
                .models
                .write()
                .unwrap_or_else(|error| error.into_inner());
            models.translation = Some(other_engine);
        }
    };
    let outcome = fixture.runtime.block_on(async {
        let run = run_translation_blocks_inner(
            &inner,
            &job,
            &translation,
            "zh-Hans",
            &mut cursor,
            &mut stats,
        );
        let (outcome, ()) = futures_util::join!(run, swapped);
        outcome.expect("另一套引擎的服务不该让运行失败")
    });

    let sent = before.requests.load(Ordering::SeqCst);
    assert!(sent >= 2, "这条用例需要至少两个文本块，实际 {sent}");
    assert_eq!(
        after.requests.load(Ordering::SeqCst),
        0,
        "本次运行不许换用另一套引擎"
    );
    assert_eq!(stats.saved, sent, "整本书照旧由领取时的 provider 翻完");
    let RunOutcome::Succeeded(finished) = outcome else {
        panic!("换用另一套引擎的请求被忽略之后仍要按 Succeeded 收尾");
    };
    assert_eq!(finished.next_ordinal, sent);
}
