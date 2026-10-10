//! 朗读的宿主侧共享层：会话状态机、进程级单会话约束与输入校验。
//!
//! 三个阅读窗口都用它：EPUB 阅读窗口（章节页面收集段落、脚本推高亮）、
//! PDF 阅读窗口（PDF.js 文字层收集段落、脚本推高亮）与页面图片窗口
//! （宿主从本页文字切段落、`<mark>` 推高亮）。窗口自己负责「读什么」与
//! 「怎么呈现」，共享的是合成/播放会话、互斥与校验。
//!
//! 朗读是**进程级**资源（系统语音与音频输出各只有一份）：同一时刻只允许
//! 一个朗读会话。新的会话开始前，[`claim_read_aloud_ownership`] 会先停掉
//! 登记表里的旧窗口；系统配置窗口的「试听」同样占用音频输出。

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, Context, EntityId, Global, WeakEntity};
use ngy_book_studio::{
    services::AppServices,
    speech::{ReadAloudEvent, ReadAloudRequest, ReadAloudSession, ReadAloudState, platform_engine},
};
use serde::Deserialize;

/// 收集方给出的一个待朗读段落。`id` 由收集方生成（段落序号），`text` 是当前
/// 显示模式下的文字：EPUB 的译文模式是译文、原文模式是原文；PDF 与页面图片
/// 窗口用文字层里当前可见的文字。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct ReadAloudPageBlock {
    pub(crate) id: String,
    pub(crate) text: String,
}

/// 一次「开始朗读」请求的容量上限。收集方可能是页面（可信桥接）或宿主自己，
/// 但消息仍按不可信输入校验：一段文字按合成引擎的单段上限收紧，整章列表再按
/// 总字节兜住。
pub(crate) const MAX_READ_ALOUD_BLOCKS: usize = 2_000;
pub(crate) const MAX_READ_ALOUD_BLOCK_BYTES: usize = 8 * 1024;
pub(crate) const MAX_READ_ALOUD_TOTAL_BYTES: usize = 512 * 1024;

impl ReadAloudPageBlock {
    pub(crate) fn valid(&self) -> bool {
        !self.id.trim().is_empty()
            && self.id.len() <= 64
            && !self.text.trim().is_empty()
            && self.text.len() <= MAX_READ_ALOUD_BLOCK_BYTES
    }
}

/// 页面列表是否可以作为一次朗读请求。
pub(crate) fn read_aloud_request_is_valid(blocks: &[ReadAloudPageBlock]) -> bool {
    if blocks.is_empty() || blocks.len() > MAX_READ_ALOUD_BLOCKS {
        return false;
    }
    let mut total = 0usize;
    for block in blocks {
        if !block.valid() {
            return false;
        }
        total = total.saturating_add(block.text.len());
        if total > MAX_READ_ALOUD_TOTAL_BYTES {
            return false;
        }
    }
    true
}

/// 一个窗口的朗读会话。会话在工作线程上跑，事件经 `async_channel` 回到窗口线程。
#[derive(Default)]
pub(crate) struct ReadAloudHost {
    session: Option<ReadAloudSession>,
    events: Option<async_channel::Receiver<ReadAloudEvent>>,
    current_block: Option<String>,
    state: Option<ReadAloudState>,
}

/// 一次轮询的结果：窗口据此更新工具栏、高亮与错误提示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadAloudPoll {
    /// 没有会话，也没有变化。
    Idle,
    /// 会话仍在进行，且当前朗读段变成了 `block_id`（首次开始或换段）。
    Highlight(String),
    /// 会话仍在进行，没有新的段落变化。
    Active,
    /// 会话刚结束；`error` 是失败原因（读完或被停止时为 None）。
    Finished { error: Option<String> },
}

impl ReadAloudHost {
    pub(crate) fn is_active(&self) -> bool {
        self.session.is_some() || self.events.is_some()
    }

    pub(crate) fn state(&self) -> Option<ReadAloudState> {
        self.state
    }

    /// 当前朗读段；侧栏用它判断「正在读的是哪条消息」（动作条按钮据此换成
    /// 停止图标）。
    pub(crate) fn current_block(&self) -> Option<&str> {
        self.current_block.as_deref()
    }

    /// 开始会话。调用方负责先停掉旧会话与其它窗口的会话。
    fn begin(
        &mut self,
        blocks: Vec<ngy_book_studio::speech::ReadAloudBlock>,
        start_index: usize,
        voice_id: String,
        rate: f32,
    ) -> anyhow::Result<()> {
        let (sender, receiver) = async_channel::bounded::<ReadAloudEvent>(64);
        let session = ReadAloudSession::start(
            platform_engine(),
            ReadAloudRequest {
                blocks,
                start_index,
                voice_id,
                rate,
            },
            move |event| {
                let _ = sender.try_send(event);
            },
        )?;
        self.session = Some(session);
        self.events = Some(receiver);
        self.current_block = None;
        self.state = Some(ReadAloudState::Preparing);
        Ok(())
    }

    /// 处理积压的会话事件；返回窗口需要响应的结果。
    fn poll(&mut self) -> ReadAloudPoll {
        let Some(events) = self.events.clone() else {
            return ReadAloudPoll::Idle;
        };
        let mut finished = None;
        let mut highlight = None;
        while let Ok(event) = events.try_recv() {
            match event {
                ReadAloudEvent::StateChanged(state) => {
                    self.state = Some(state);
                }
                ReadAloudEvent::BlockStarted { block_id, .. } => {
                    self.current_block = Some(block_id.clone());
                    highlight = Some(block_id);
                }
                ReadAloudEvent::BlockFinished { .. } => {}
                ReadAloudEvent::Finished { error } => {
                    finished = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = finished {
            self.session = None;
            self.events = None;
            self.current_block = None;
            self.state = None;
            return ReadAloudPoll::Finished { error };
        }
        match highlight {
            Some(block_id) => ReadAloudPoll::Highlight(block_id),
            None => ReadAloudPoll::Active,
        }
    }

    fn pause(&self) {
        if let Some(session) = self.session.as_ref() {
            session.pause();
        }
    }

    fn resume(&self) {
        if let Some(session) = self.session.as_ref() {
            session.resume();
        }
    }

    /// 停止会话并清掉本窗口的朗读状态；返回之前是否活跃。
    fn stop(&mut self) -> bool {
        let active = self.is_active();
        if let Some(mut session) = self.session.take() {
            session.stop();
        }
        self.events = None;
        self.current_block = None;
        self.state = None;
        active
    }
}

/// 朗读窗口的登记项：窗口身份与它的「停止朗读」入口。闭包捕获弱引用，
/// 窗口释放后自然失效。
type ReadAloudOwner = (EntityId, Box<dyn Fn(&mut App)>);

/// 正在朗读的窗口。朗读是进程级资源，这张表用来在开始新会话前停掉旧窗口。
#[derive(Default)]
pub(crate) struct ReadAloudRegistry {
    owner: Option<ReadAloudOwner>,
}

impl Global for ReadAloudRegistry {}

fn ensure_read_aloud_registry(cx: &mut App) {
    if !cx.has_global::<ReadAloudRegistry>() {
        cx.set_global(ReadAloudRegistry::default());
    }
}

/// 把登记表指向 `owner`，并停掉其它窗口的朗读。登记的是同一个窗口时不重复
/// 停止：调用方在开始新会话前已经停过自己的旧会话，重入会碰上正在更新的视图。
pub(crate) fn claim_read_aloud_ownership<T: ReadAloudWindow>(cx: &mut App, owner: WeakEntity<T>) {
    ensure_read_aloud_registry(cx);
    let id = owner.entity_id();
    let stop: Box<dyn Fn(&mut App)> = Box::new(move |cx: &mut App| {
        if let Some(view) = owner.upgrade() {
            view.update(cx, |this, cx| this.stop_read_aloud(cx));
        }
    });
    let previous = cx
        .global_mut::<ReadAloudRegistry>()
        .owner
        .replace((id, stop));
    if let Some((previous_id, previous)) = previous
        && previous_id != id
    {
        previous(cx);
    }
}

/// 清掉登记表（会话结束或窗口关闭时调用，幂等）。
pub(crate) fn release_read_aloud_ownership(cx: &mut App) {
    if cx.has_global::<ReadAloudRegistry>() {
        cx.global_mut::<ReadAloudRegistry>().owner = None;
    }
}

/// 一个能朗读的阅读窗口。默认实现覆盖「读设置 → 起会话 → 轮询 → 呈现 →
/// 停止」整条链路；窗口只需要说明：状态放哪、设置从哪读、高亮怎么呈现。
pub(crate) trait ReadAloudWindow: Sized + 'static {
    /// 本窗口的朗读会话状态。
    fn read_aloud(&mut self) -> &mut ReadAloudHost;

    /// 读语音设置（音色与语速）的服务入口。
    fn read_aloud_services(&self) -> &Arc<AppServices>;

    /// 呈现当前朗读段；`None` 表示清除高亮。实现不得让失败的呈现升级成错误。
    fn apply_read_aloud_highlight(&mut self, block_id: Option<&str>, cx: &mut Context<Self>);

    /// 提示朗读错误；默认丢弃。
    fn report_read_aloud_error(&mut self, _message: String, _cx: &mut Context<Self>) {}

    /// 从一段段落列表开始（或从 `start_block` 指向的那段开始）朗读。
    fn start_read_aloud_blocks(
        &mut self,
        blocks: Vec<ReadAloudPageBlock>,
        start_block: &str,
        cx: &mut Context<Self>,
    ) {
        if !read_aloud_request_is_valid(&blocks) {
            return;
        }
        let settings = self.read_aloud_services().provider_settings().ok();
        let voice_id = settings
            .as_ref()
            .map(|settings| settings.speech_voice_id.clone())
            .unwrap_or_default();
        let rate = settings
            .as_ref()
            .map(|settings| settings.speech_rate)
            .unwrap_or(1.0);
        let start_index = blocks
            .iter()
            .position(|block| block.id == start_block)
            .unwrap_or(0);
        let blocks = blocks
            .into_iter()
            .map(|block| ngy_book_studio::speech::ReadAloudBlock {
                id: block.id,
                text: block.text,
            })
            .collect::<Vec<_>>();

        // 先停本窗口的旧会话与其它窗口的会话：音频输出只有一份。
        self.stop_read_aloud(cx);
        let owner = cx.entity().downgrade();
        claim_read_aloud_ownership(cx, owner);

        if let Err(error) = self.read_aloud().begin(blocks, start_index, voice_id, rate) {
            release_read_aloud_ownership(cx);
            self.report_read_aloud_error(format!("无法开始朗读：{error:#}"), cx);
            return;
        }
        self.spawn_read_aloud_poll(cx);
        cx.notify();
    }

    fn pause_read_aloud(&mut self, cx: &mut Context<Self>) {
        self.read_aloud().pause();
        cx.notify();
    }

    fn resume_read_aloud(&mut self, cx: &mut Context<Self>) {
        self.read_aloud().resume();
        cx.notify();
    }

    /// 停止朗读并清除呈现。幂等：没有会话时只清登记。
    fn stop_read_aloud(&mut self, cx: &mut Context<Self>) {
        let was_active = self.read_aloud().stop();
        release_read_aloud_ownership(cx);
        if was_active {
            self.apply_read_aloud_highlight(None, cx);
            cx.notify();
        }
    }

    /// 轮询会话事件并把结果同步到界面；返回会话是否仍在继续。
    fn poll_read_aloud(&mut self, cx: &mut Context<Self>) -> bool {
        match self.read_aloud().poll() {
            ReadAloudPoll::Idle => false,
            ReadAloudPoll::Active => true,
            ReadAloudPoll::Highlight(block_id) => {
                self.apply_read_aloud_highlight(Some(&block_id), cx);
                cx.notify();
                true
            }
            ReadAloudPoll::Finished { error } => {
                release_read_aloud_ownership(cx);
                self.apply_read_aloud_highlight(None, cx);
                if let Some(error) = error {
                    self.report_read_aloud_error(format!("朗读已停止：{error}"), cx);
                }
                cx.notify();
                false
            }
        }
    }

    /// 会话自己的线程上跑；窗口按固定间隔取事件，避免跨线程直接借用 GPUI。
    fn spawn_read_aloud_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                match view.update(cx, |this, cx| this.poll_read_aloud(cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_aloud_request_is_bounded_by_blocks_and_bytes() {
        assert!(!read_aloud_request_is_valid(&[]));
        let block = |id: &str, text: &str| ReadAloudPageBlock {
            id: id.to_string(),
            text: text.to_string(),
        };
        assert!(read_aloud_request_is_valid(&[block("0", "一段正文")]));
        assert!(!read_aloud_request_is_valid(&[block("", "一段正文")]));
        assert!(!read_aloud_request_is_valid(&[block("0", "   ")]));
        assert!(!read_aloud_request_is_valid(&[block(
            "0",
            &"字".repeat(MAX_READ_ALOUD_BLOCK_BYTES + 1)
        )]));
        let many = (0..MAX_READ_ALOUD_BLOCKS + 1)
            .map(|index| block(&index.to_string(), "正文"))
            .collect::<Vec<_>>();
        assert!(!read_aloud_request_is_valid(&many));
    }
}
