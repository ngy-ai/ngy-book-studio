use super::*;

/// 保存了新的 Endpoint/密钥（视觉模型与执行身份不变）之后，正在跑的 vision 任务必须换用新
/// provider，而不是把领取时的快照用完整本图书。
///
/// 同类现场 2026-09-24：只换 API key 时任务行不会被重置，运行中的执行器一直握着旧值。
#[test]
pub(crate) fn a_running_vision_uses_the_saved_provider_instead_of_its_claim_snapshot() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let inner = fixture.indexing_inner();
    let before = Arc::new(MockProvider::default());
    let after = Arc::new(MockProvider::default());

    let claimed = ModelServices {
        vision_provider: before.clone(),
        ..inner.models_snapshot()
    };
    inner.models.write().unwrap().vision_provider = after.clone();

    let job = running_vision_job(&fixture, "vision-test", "vision-endpoint-a:vision-test");
    let outcome = fixture
        .runtime
        .block_on(run_vision(&inner, &job, &claimed))
        .unwrap();
    assert!(
        matches!(outcome, RunOutcome::Succeeded(_)),
        "换 provider 之后视觉理解照旧走完"
    );
    assert_eq!(
        after.vision_png_calls.load(Ordering::SeqCst),
        1,
        "页面请求必须发给保存后的 provider"
    );
    assert_eq!(
        before.vision_png_calls.load(Ordering::SeqCst),
        0,
        "旧 provider 不该再收到请求"
    );
}

/// 保存的配置属于另一套引擎（换了视觉模型或端点）时，本次运行不换 provider：那次保存会把
/// 任务行整个重置（并清理模型派生的页面描述），本次运行随后会被判成已被替换。
#[test]
pub(crate) fn a_running_vision_ignores_a_saved_provider_from_another_engine() {
    let fixture = Fixture::new();
    fixture.publish_test_visual_page();
    let inner = fixture.indexing_inner();
    let before = Arc::new(MockProvider::default());
    let after = Arc::new(MockProvider::default());

    let claimed = ModelServices {
        vision_provider: before.clone(),
        ..inner.models_snapshot()
    };
    {
        let mut models = inner.models.write().unwrap();
        models.vision_provider = after.clone();
        models.config.vision_execution_identity = "vision-endpoint-b:vision-test".to_string();
    }

    let job = running_vision_job(&fixture, "vision-test", "vision-endpoint-a:vision-test");
    fixture
        .runtime
        .block_on(run_vision(&inner, &job, &claimed))
        .unwrap();
    assert_eq!(
        before.vision_png_calls.load(Ordering::SeqCst),
        1,
        "本次运行仍然只用领取时那一套引擎"
    );
    assert_eq!(
        after.vision_png_calls.load(Ordering::SeqCst),
        0,
        "本次运行不许换用另一套引擎的 provider"
    );
}
