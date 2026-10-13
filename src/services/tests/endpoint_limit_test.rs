//! 端点限速在服务层的装配与发布。

use super::*;

/// 每个角色报出来的上限必须与它绑定的那处地址一致；同一条地址的额度是同一份。
#[test]
fn every_role_reports_the_limit_of_the_endpoint_it_is_bound_to() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join(db::DATABASE_FILE);
    db::open_or_recreate(&db_path).unwrap();

    // 三个角色两处地址：默认 Endpoint 上跑问答与翻译（12 RPM），embedding 那台配 5，
    // vision 那一台不限。
    let mut settings = routed_settings();
    settings.requests_per_minute = 12;
    settings.endpoint_routing.additional_endpoints[0].requests_per_minute = 5;
    settings.validate().unwrap();

    let limits = Arc::new(EndpointRateLimits::new());
    let services = build_ai_services(
        &db_path,
        settings.clone(),
        &BTreeMap::new(),
        Arc::clone(&limits),
    )
    .unwrap();

    let budget = |provider: &Arc<dyn OpenAiCompatibleProvider>| {
        provider
            .endpoint_budget()
            .expect("每个角色都必须接额度闸门")
    };
    assert_eq!(budget(&services.provider).limit().requests(), 12);
    assert_eq!(
        budget(&services.translation_provider).limit().requests(),
        12
    );
    assert_eq!(budget(&services.embedding_provider).limit().requests(), 5);
    assert_eq!(
        budget(&services.vision_provider).limit().requests(),
        0,
        "不限也是「接了额度闸门、上限为 0」，而不是没有闸门"
    );

    // 同一处地址的角色共用**同一份**额度（同一份额度、不同的排队类别）。
    assert!(Arc::ptr_eq(
        budget(&services.provider).limiter(),
        budget(&services.translation_provider).limiter()
    ));
    assert!(!Arc::ptr_eq(
        budget(&services.provider).limiter(),
        budget(&services.embedding_provider).limiter()
    ));
    assert_eq!(
        budget(&services.provider).class(),
        RequestClass::Interactive
    );
    assert_eq!(
        budget(&services.translation_provider).class(),
        RequestClass::Background
    );

    // 检索的查询向量是问答链路的前半段：它与后台索引同属 embedding 端点、共用同一份额度，
    // 但排队算交互请求 —— 用户提问时不该排在整本翻译的索引请求后面。
    let search = budget(services.search.embedding_provider_for_test());
    assert_eq!(search.class(), RequestClass::Interactive);
    assert!(Arc::ptr_eq(
        search.limiter(),
        budget(&services.embedding_provider).limiter()
    ));

    // 地址到上限的对应关系写在注册表里，同一条地址只有一份额度。
    for (url, expected) in [
        (&settings.base_url, 12),
        (
            &settings.endpoint_routing.additional_endpoints[0].base_url,
            5,
        ),
    ] {
        assert_eq!(
            limits.limit_for(url).unwrap().map(|limit| limit.requests()),
            Some(expected),
            "{url}"
        );
    }
}

/// 保存设置后上限立即生效，不需要重启；列表里没被任何模型绑定的地址也在发布范围内。
#[test]
fn saving_settings_publishes_every_endpoint_limit_including_unbound_ones() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let services = AppServices::open_with_credentials(temp.path(), credentials).unwrap();

    let mut settings = routed_settings();
    settings.requests_per_minute = 30;
    settings.endpoint_routing.additional_endpoints[0].requests_per_minute = 7;
    let mut unbound = endpoint_fixture("spare", 21437);
    unbound.requests_per_minute = 3;
    settings.endpoint_routing.additional_endpoints.push(unbound);

    block_on_without_tokio(services.configure_providers(settings.clone(), BTreeMap::new()))
        .unwrap();

    let limits = services.endpoint_limits();
    for (url, expected) in [
        (&settings.base_url, 30),
        (
            &settings.endpoint_routing.additional_endpoints[0].base_url,
            7,
        ),
        // vision 那一台没配上限，仍然要有条目（值就是「不限」）。
        (
            &settings.endpoint_routing.additional_endpoints[1].base_url,
            0,
        ),
        (
            &settings.endpoint_routing.additional_endpoints[2].base_url,
            3,
        ),
    ] {
        assert_eq!(
            limits.limit_for(url).unwrap().map(|limit| limit.requests()),
            Some(expected),
            "{url}"
        );
    }
}
