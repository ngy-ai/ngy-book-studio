//! 端点限速：额度、排队顺序、429 之后的冷却。
//!
//! 全部用缩短的窗口（[`EndpointRateLimit::within`]）跑：真正的窗口是 60 秒，等它一轮
//! 会让单元测试慢到没人愿意跑。判定标准是「等了多久」与「谁先走」，不是内部字段。

use std::sync::{Arc, Mutex};

use super::*;

/// 测试用的窗口长度。所有下限断言都留了一半余量，给调度抖动留位置。
const WINDOW: Duration = Duration::from_millis(200);

fn limiter(requests: u32) -> Arc<EndpointLimiter> {
    Arc::new(EndpointLimiter::new(EndpointRateLimit::within(
        requests, WINDOW,
    )))
}

/// 上限不限、窗口很长的闸门：用它可以保证测出来的等待只可能来自冷却。
fn unlimited_limiter() -> Arc<EndpointLimiter> {
    Arc::new(EndpointLimiter::new(EndpointRateLimit::within(
        10,
        Duration::from_secs(60),
    )))
}

#[tokio::test]
async fn an_endpoint_without_a_limit_never_makes_a_request_wait() {
    let limiter = Arc::new(EndpointLimiter::new(EndpointRateLimit::per_minute(0)));
    tokio::time::timeout(Duration::from_secs(1), async {
        for _ in 0..32 {
            limiter.acquire(RequestClass::Background).await;
            limiter.acquire(RequestClass::Interactive).await;
        }
    })
    .await
    .expect("不限的端点不该排队");
}

#[tokio::test]
async fn an_endpoint_hands_out_one_window_of_requests_and_refills_after_it() {
    let limiter = limiter(2);
    let started = Instant::now();
    limiter.acquire(RequestClass::Background).await;
    limiter.acquire(RequestClass::Background).await;
    let inside = started.elapsed();
    assert!(inside < WINDOW / 2, "窗口里还有位置时不该等待：{inside:?}");

    let third = Instant::now();
    limiter.acquire(RequestClass::Background).await;
    let waited = third.elapsed();
    assert!(
        waited >= WINDOW / 2,
        "超出额度的请求必须等到窗口放行：{waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "等的是下一个窗口，不是别的什么东西：{waited:?}"
    );
}

#[tokio::test]
async fn an_interactive_request_overtakes_queued_background_work() {
    let limiter = limiter(1);
    // 窗口里唯一的位置先被一发后台请求用掉。
    limiter.acquire(RequestClass::Background).await;

    let order = Arc::new(Mutex::new(Vec::new()));
    let mut background = Vec::new();
    for label in ["background-1", "background-2"] {
        let budget = EndpointBudget::new(Arc::clone(&limiter), RequestClass::Background);
        let order = Arc::clone(&order);
        background.push(tokio::spawn(async move {
            budget.acquire().await;
            order.lock().unwrap().push(label);
        }));
    }
    // 先让两发后台请求真的排上队，否则「插队」测的是别的东西。
    tokio::time::sleep(Duration::from_millis(20)).await;

    let interactive = EndpointBudget::new(Arc::clone(&limiter), RequestClass::Interactive);
    let interactive_order = Arc::clone(&order);
    let interactive_task = tokio::spawn(async move {
        interactive.acquire().await;
        interactive_order.lock().unwrap().push("interactive");
    });

    for waiter in background {
        waiter.await.unwrap();
    }
    interactive_task.await.unwrap();
    assert_eq!(
        *order.lock().unwrap(),
        ["interactive", "background-1", "background-2"],
        "问答要先于已经在等的后台任务拿到额度"
    );
}

#[tokio::test]
async fn a_cancelled_waiter_does_not_block_the_queue_behind_it() {
    let limiter = limiter(1);
    limiter.acquire(RequestClass::Background).await;

    let abandoned = EndpointBudget::new(Arc::clone(&limiter), RequestClass::Interactive);
    let task = tokio::spawn(async move { abandoned.acquire().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    // 用户取消、窗口关闭或任务被中止都会让这个 future 直接掉在地上：排队时留下的号
    // 必须跟着消失，否则队首会永远停在一个已经没人等的请求上。
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    let next = EndpointBudget::new(Arc::clone(&limiter), RequestClass::Interactive);
    tokio::time::timeout(Duration::from_secs(2), next.acquire())
        .await
        .expect("被取消的等待者不该挡住后面的请求");
}

#[tokio::test]
async fn a_rate_limited_response_cools_the_whole_endpoint_down() {
    let limiter = unlimited_limiter();
    limiter.cool_down_after_rate_limit(Some(Duration::from_millis(300)));
    // 冷却只延长、不缩短：后到的「Retry-After: 0.001 秒」不能把已经记下的退避改小。
    limiter.cool_down_after_rate_limit(Some(Duration::from_millis(1)));

    let started = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(2),
        limiter.acquire(RequestClass::Interactive),
    )
    .await
    .expect("冷却是有头的等待，不该一直等下去");
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(250),
        "额度充足时，等待只应该来自 429 之后的冷却：{waited:?}"
    );
}

#[tokio::test]
async fn a_cooled_down_endpoint_waits_even_without_a_local_quota() {
    // 上限不限不等于「可以硬撞服务商的 429」：冷却与本地额度是两件事。
    let limiter = Arc::new(EndpointLimiter::new(EndpointRateLimit::per_minute(0)));
    limiter.cool_down_after_rate_limit(Some(Duration::from_millis(300)));

    let started = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(2),
        limiter.acquire(RequestClass::Background),
    )
    .await
    .expect("冷却是有头的等待，不该一直等下去");
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(250),
        "冷却被跳过了：{waited:?}"
    );
}

#[tokio::test]
async fn a_rate_limit_without_retry_after_still_backs_off() {
    let limiter = unlimited_limiter();
    limiter.cool_down_after_rate_limit(None);
    let outcome = tokio::time::timeout(
        Duration::from_millis(300),
        limiter.acquire(RequestClass::Interactive),
    )
    .await;
    assert!(
        outcome.is_err(),
        "读不到 Retry-After 也要退避，而不是立刻重发那一发被拒绝的请求"
    );
}

#[test]
fn one_budget_is_shared_by_every_role_of_the_same_endpoint() {
    let limits = EndpointRateLimits::new();
    let chat = limits.slot("http://127.0.0.1:11434/v1/", 4).unwrap();
    let translation = limits.slot("http://127.0.0.1:11434/v1", 4).unwrap();
    assert!(
        Arc::ptr_eq(&chat, &translation),
        "同一个地址（含补上的尾斜杠）必须共用一份额度"
    );

    let other = limits.slot("http://127.0.0.1:11435/v1/", 4).unwrap();
    assert!(!Arc::ptr_eq(&chat, &other), "不同端口是两份额度");
    assert_eq!(
        limits.limit_for("http://127.0.0.1:11435/v1/").unwrap(),
        Some(other.limit())
    );
    assert_eq!(
        limits.limit_for("http://127.0.0.1:11433/v1/").unwrap(),
        None
    );

    // 保存设置会把新的上限同步进已经存在的条目，已经记下的窗口不受影响。
    limits.slot("http://127.0.0.1:11434/v1/", 12).unwrap();
    assert_eq!(chat.limit().requests(), 12);
    assert_eq!(
        limits.limit_for("http://127.0.0.1:11434/v1/").unwrap(),
        Some(chat.limit())
    );

    // 手误写下的大数字按上限钳制，而不是让这一行设置变得无法保存。
    limits
        .slot("http://127.0.0.1:11434/v1/", MAX_ENDPOINT_RPM + 5)
        .unwrap();
    assert_eq!(chat.limit().requests(), MAX_ENDPOINT_RPM);
}

#[test]
fn retry_after_is_read_only_in_seconds() {
    let mut headers = reqwest::header::HeaderMap::new();
    assert_eq!(parse_retry_after(&headers), None);

    for (value, expected) in [
        ("12", Some(Duration::from_secs(12))),
        ("0", Some(Duration::ZERO)),
    ] {
        headers.insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
        assert_eq!(parse_retry_after(&headers), expected, "{value:?}");
    }

    // 秒数之外的一切写法都读不懂：HTTP-date 要日期解析，负数、小数与空值不是时长。
    for value in [
        "",
        "soon",
        "-3",
        "0.5",
        "1e3",
        "Wed, 21 Oct 2015 07:28:00 GMT",
    ] {
        headers.insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
        assert_eq!(parse_retry_after(&headers), None, "{value:?}");
    }
}
