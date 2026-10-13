//! 按 Endpoint 限速：滑动窗口 + 交互优先 + 服务商退回后的整体冷却。
//!
//! 服务商按**端点**计量 RPM，所以额度也按规范化后的 `base_url` 共享：同一台服务器上的
//! 问答、整本翻译、向量索引、视觉理解与模型检测走的是同一份配额，谁都不能把它撑破。
//! 等待发生在请求真正发出之前，因此**不计入**请求超时 —— 那是「端点多久不回话」的上限，
//! 不是本地排队的上限。
//!
//! 交互请求（用户按下发送的问答、Agent、模型检测）排在后台任务前面：翻一本书的后台任务
//! 不该把用户刚提的问题堵在一整分钟之后。两者共用同一份额度，只有排队顺序不同。

use super::*;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// 每分钟请求数上限的最大值。
///
/// 上限本身不花代价，这个值是给手误兜底：一个多打了几个零的数字不该让窗口里堆出毫无意义
/// 的条目。超出的值按它钳制，而不是让设置无法保存。
pub const MAX_ENDPOINT_RPM: u32 = 10_000;

/// 服务商没说 `Retry-After` 时，429 之后默认退避多久。
const RATE_LIMIT_COOLDOWN_DEFAULT: Duration = Duration::from_secs(60);
/// 单次冷却的下限：`Retry-After: 0` 的意思是「马上再来」，而那正是刚被拒绝的那一发。
const RATE_LIMIT_COOLDOWN_MIN: Duration = Duration::from_secs(1);
/// 单次冷却的上限：服务商给再大的数字也只退避这么久，不让一个响应把应用停住。
const RATE_LIMIT_COOLDOWN_MAX: Duration = Duration::from_secs(600);
/// 非队首等待者的复查间隔。队首会拿走刚空出来的那个位置，后面的人很快就能知道自己能不能走。
const WAITER_POLL: Duration = Duration::from_millis(10);
/// 窗口边界上多等一点：少数情况下 `Instant` 的精度会让等待者醒得刚好早一点点。
const WAKE_EPSILON: Duration = Duration::from_millis(1);
/// 超过这个时长的等待写一条日志，好让「任务卡住」在诊断里有据可查。
const WAIT_LOG_THRESHOLD: Duration = Duration::from_secs(1);

/// 一次请求属于哪一类。额度是共用的，只有排队顺序不同。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestClass {
    /// 用户正等着看的请求：AI 问答、Agent、模型检测。
    Interactive,
    /// 后台任务发出的请求：整本翻译、向量索引、视觉理解。
    Background,
}

impl RequestClass {
    fn label(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
        }
    }
}

/// 一个 Endpoint 的额度：`requests` 次请求 / `window`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointRateLimit {
    requests: u32,
    window: Duration,
}

impl EndpointRateLimit {
    /// 用户配置的「每分钟请求数上限」。`0` 表示不限，超上限的值按 [`MAX_ENDPOINT_RPM`] 钳制。
    pub fn per_minute(requests: u32) -> Self {
        Self {
            requests: requests.min(MAX_ENDPOINT_RPM),
            window: Duration::from_secs(60),
        }
    }

    /// 同样的语义，换成别的窗口时长；只给测试用来把窗口缩到几十毫秒。
    #[cfg(test)]
    pub(crate) fn within(requests: u32, window: Duration) -> Self {
        Self { requests, window }
    }

    pub fn is_unlimited(&self) -> bool {
        self.requests == 0
    }

    pub fn requests(&self) -> u32 {
        self.requests
    }
}

/// 进程级注册表：同一个 `base_url` 的所有角色共用一份额度。
///
/// Provider 会随设置保存整批重建，所以额度不能挂在 provider 上。它活在 `AppServices`
/// 的生命周期里，重建后的 provider 拿到的是同一个 [`EndpointLimiter`]。键是规范化后的
/// `base_url`，与 `ProviderSettings::validate` 拒绝重复地址的口径一致。
#[derive(Debug, Default)]
pub struct EndpointRateLimits {
    slots: Mutex<HashMap<String, Arc<EndpointLimiter>>>,
}

impl EndpointRateLimits {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取（必要时建立）这个 Endpoint 的额度，并把上限同步成 `requests_per_minute`。
    ///
    /// 同一个地址的每个角色都经过这里，拿到的因此是**同一个** [`EndpointLimiter`]：
    /// 额度按端点共享，而不是按角色各算一份。保存设置后所有已配置的 Endpoint 都会经过
    /// 这里，上限改动立即生效；已经记下的请求时刻不受影响，只有之后的请求按新上限放行。
    /// 地址被删掉的条目留在表里：它最多占一个窗口的时间戳，而地址一旦被加回来，额度本来
    /// 也该接着算。
    pub fn slot(&self, base_url: &str, requests_per_minute: u32) -> Result<Arc<EndpointLimiter>> {
        let key = normalize_provider_base_url(base_url)?.to_string();
        let limit = EndpointRateLimit::per_minute(requests_per_minute);
        let mut slots = lock(&self.slots);
        Ok(match slots.get(&key) {
            Some(limiter) => {
                limiter.set_limit(limit);
                Arc::clone(limiter)
            }
            None => {
                let limiter = Arc::new(EndpointLimiter::new(limit));
                slots.insert(key, Arc::clone(&limiter));
                limiter
            }
        })
    }

    /// 这个地址当前生效的上限，`None` 表示还没有为它建立条目。
    ///
    /// 只读：诊断与设置界面用它确认「保存之后到底生效了没有」，不会顺手建立条目。
    pub fn limit_for(&self, base_url: &str) -> Result<Option<EndpointRateLimit>> {
        let key = normalize_provider_base_url(base_url)?.to_string();
        Ok(lock(&self.slots).get(&key).map(|limiter| limiter.limit()))
    }
}

/// Provider 与「某个 Endpoint 的额度 + 自己属于哪一类请求」的绑定。
#[derive(Clone, Debug)]
pub struct EndpointBudget {
    limiter: Arc<EndpointLimiter>,
    class: RequestClass,
}

impl EndpointBudget {
    pub fn new(limiter: Arc<EndpointLimiter>, class: RequestClass) -> Self {
        Self { limiter, class }
    }

    /// 等到这一次请求可以发出为止。
    pub async fn acquire(&self) {
        self.limiter.acquire(self.class).await;
    }

    /// 服务商明确说额度用尽（HTTP 429）：整个 Endpoint 一起退避。
    pub fn note_rate_limit(&self, retry_after: Option<Duration>) {
        self.limiter.cool_down_after_rate_limit(retry_after);
    }

    /// 当前生效的上限。
    pub fn limit(&self) -> EndpointRateLimit {
        self.limiter.limit()
    }

    /// 这一次请求算哪一类：额度共用，只有排队顺序不同。
    pub fn class(&self) -> RequestClass {
        self.class
    }

    /// 额度闸门本身。同一个 Endpoint 上的每个角色拿到的都是同一个（见
    /// [`EndpointRateLimits::slot`]），装配测试用它确认「共享」不是靠约定。
    #[cfg(test)]
    pub(crate) fn limiter(&self) -> &Arc<EndpointLimiter> {
        &self.limiter
    }
}

/// 一个 Endpoint 的额度闸门。
#[derive(Debug)]
pub struct EndpointLimiter {
    state: Mutex<LimiterState>,
}

#[derive(Debug)]
struct LimiterState {
    limit: EndpointRateLimit,
    /// 最近一个窗口内已经放行的请求时刻，升序。
    window: VecDeque<Instant>,
    /// 正在等额度的请求，按到达顺序编号；交互请求整体排在后台任务前面。
    interactive: VecDeque<u64>,
    background: VecDeque<u64>,
    next_ticket: u64,
    /// 服务商退回时写下的整体退避截止时刻。
    cooldown_until: Option<Instant>,
}

impl EndpointLimiter {
    pub(crate) fn new(limit: EndpointRateLimit) -> Self {
        Self {
            state: Mutex::new(LimiterState {
                limit,
                window: VecDeque::new(),
                interactive: VecDeque::new(),
                background: VecDeque::new(),
                next_ticket: 0,
                cooldown_until: None,
            }),
        }
    }

    pub fn limit(&self) -> EndpointRateLimit {
        self.lock().limit
    }

    pub fn set_limit(&self, limit: EndpointRateLimit) {
        self.lock().limit = limit;
    }

    /// 服务商报告额度用尽：之后所有请求先等过这段冷却。
    ///
    /// 冷却只延长、不缩短，所以连着的两发 429 不会把已经记下的退避改小。
    pub fn cool_down_after_rate_limit(&self, retry_after: Option<Duration>) {
        let cooldown = retry_after
            .unwrap_or(RATE_LIMIT_COOLDOWN_DEFAULT)
            .clamp(RATE_LIMIT_COOLDOWN_MIN, RATE_LIMIT_COOLDOWN_MAX);
        let until = Instant::now() + cooldown;
        let mut state = self.lock();
        state.cooldown_until = Some(
            state
                .cooldown_until
                .map_or(until, |current| current.max(until)),
        );
        tracing::warn!(
            target: "ngy_ai",
            stage = "endpoint_rate_limit_cooldown",
            retry_after_ms = retry_after.map(|value| value.as_millis() as u64),
            cooldown_ms = cooldown.as_millis() as u64,
            "服务商报告额度用尽，整个 Endpoint 退避"
        );
    }

    /// 等到这一次请求可以发出为止。
    pub async fn acquire(&self, class: RequestClass) {
        {
            // 不限额度又没有冷却时不必排队 —— 这是默认配置下每一次请求走的路。
            // 冷却必须一起看：上限不限不等于可以硬撞服务商已经给出的 429。
            let state = self.lock();
            if state.limit.is_unlimited()
                && state
                    .cooldown_until
                    .is_none_or(|until| until <= Instant::now())
            {
                return;
            }
        }
        let ticket = {
            let mut state = self.lock();
            state.next_ticket += 1;
            let ticket = state.next_ticket;
            state.queue_mut(class).push_back(ticket);
            ticket
        };
        // 请求被取消（任务中止、窗口关闭）时把队里那一条带走：一个已经没人等的号留在队首
        // 会让后面的请求永远拿不到额度。
        let _waiter = Waiter {
            limiter: self,
            ticket,
            class,
        };
        let queued_at = Instant::now();
        let mut logged = false;
        loop {
            let wait = {
                let mut state = self.lock();
                let now = Instant::now();
                state.trim(now);
                if state.grantable(class, ticket, now) {
                    state.window.push_back(now);
                    state.dequeue(class);
                    if queued_at.elapsed() >= WAIT_LOG_THRESHOLD {
                        tracing::info!(
                            target: "ngy_ai",
                            stage = "endpoint_rate_limit_released",
                            request_class = class.label(),
                            queued_ms = queued_at.elapsed().as_millis() as u64,
                            "AI 请求等到 Endpoint 额度"
                        );
                    }
                    return;
                }
                state.wait_for(class, ticket, now)
            };
            if !logged && wait >= WAIT_LOG_THRESHOLD {
                logged = true;
                tracing::info!(
                    target: "ngy_ai",
                    stage = "endpoint_rate_limit_wait",
                    request_class = class.label(),
                    wait_ms = wait.as_millis() as u64,
                    limit_per_minute = self.limit().requests(),
                    "AI 请求等待 Endpoint 额度"
                );
            }
            tokio::time::sleep(wait).await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, LimiterState> {
        lock(&self.state)
    }
}

impl LimiterState {
    fn queue_mut(&mut self, class: RequestClass) -> &mut VecDeque<u64> {
        match class {
            RequestClass::Interactive => &mut self.interactive,
            RequestClass::Background => &mut self.background,
        }
    }

    /// 丢掉已经离开窗口的请求时刻。
    fn trim(&mut self, now: Instant) {
        let window = self.limit.window;
        while self
            .window
            .front()
            .is_some_and(|front| now.saturating_duration_since(*front) >= window)
        {
            self.window.pop_front();
        }
    }

    /// 轮到它、没有冷却、窗口里还有位置。
    fn grantable(&self, class: RequestClass, ticket: u64, now: Instant) -> bool {
        if !self.is_turn(class, ticket) {
            return false;
        }
        if self.cooldown_until.is_some_and(|until| until > now) {
            return false;
        }
        self.limit.is_unlimited() || self.window.len() < self.limit.requests as usize
    }

    fn is_turn(&self, class: RequestClass, ticket: u64) -> bool {
        match class {
            RequestClass::Interactive => self.interactive.front() == Some(&ticket),
            // 后台任务必须让交互请求先走：空出来的位置先给还等着的问答。
            RequestClass::Background => {
                self.background.front() == Some(&ticket) && self.interactive.is_empty()
            }
        }
    }

    /// 下一次可能放行的时刻距离现在还有多久。
    fn wait_for(&self, class: RequestClass, ticket: u64, now: Instant) -> Duration {
        let mut wait = Duration::ZERO;
        if let Some(until) = self.cooldown_until {
            wait = wait.max(until.saturating_duration_since(now));
        }
        if !self.limit.is_unlimited() && self.window.len() >= self.limit.requests as usize {
            // 窗口满了：队首那条请求离开窗口的时刻就是下一个位置。
            if let Some(front) = self.window.front() {
                wait = wait.max(
                    (*front + self.limit.window + WAKE_EPSILON).saturating_duration_since(now),
                );
            }
        } else if !self.is_turn(class, ticket) {
            // 还有位置，只是不是我的：能拿到它的请求就在前面，很快。
            wait = wait.max(WAITER_POLL);
        }
        if wait.is_zero() { WAITER_POLL } else { wait }
    }

    fn dequeue(&mut self, class: RequestClass) {
        self.queue_mut(class).pop_front();
    }

    /// 把一个还等着的号从队里去掉（可能不在队首：等待者不一定是队首）。
    fn withdraw(&mut self, class: RequestClass, ticket: u64) {
        let queue = self.queue_mut(class);
        if let Some(index) = queue.iter().position(|queued| *queued == ticket) {
            queue.remove(index);
        }
    }
}

/// 排队期间的凭证。放行时已经出队，离开作用域再做一次「出队」是空操作。
struct Waiter<'a> {
    limiter: &'a EndpointLimiter,
    ticket: u64,
    class: RequestClass,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.limiter.lock().withdraw(self.class, self.ticket);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// `Retry-After` 只认「秒数」这一种写法。
///
/// HTTP-date 形式（`Wed, 21 Oct 2015 07:28:00 GMT`）要日期解析，而 OpenAI 兼容端点几乎
/// 都发秒数；读不懂的写法一律回落到默认退避，绝不因为看不懂就把请求立刻重发出去。
pub(crate) fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if value.is_empty() || value.len() > 20 {
        return None;
    }
    value.parse::<u64>().ok().map(Duration::from_secs)
}
