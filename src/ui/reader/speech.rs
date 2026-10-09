//! 阅读窗口的朗读：会话状态、跨窗口互斥与页面高亮。
//!
//! 页面负责「读什么」：它把当前显示模式下的段落列表（id + 显示文字）随「开始朗读」
//! 一起发过来，宿主只负责按顺序合成、播放，并把「正在读哪一段」推回页面做高亮。
//! 这么做有两个好处：朗读文字天然跟随译文/原文显示方式，而且宿主不需要重新解析
//! 章节 DOM。
//!
//! 朗读是**进程级**资源（系统语音与音频输出各只有一份）：同一时刻只允许一个阅读
//! 窗口在朗读。新的会话开始前，[`stop_other_read_aloud_windows`] 会先停掉登记表里
//! 的旧窗口；切换章节、切换译文显示方式与关闭窗口都会停止本窗口的会话。

use super::*;
use ngy_book_studio::speech::{
    ReadAloudEvent, ReadAloudRequest, ReadAloudSession, ReadAloudState, platform_engine,
};
use serde::Deserialize;

/// 页面发来的一个待朗读段落。`id` 由页面生成（段落序号），`text` 是当前显示
/// 模式下的文字：译文模式是译文，原文模式是原文。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct ReadAloudPageBlock {
    pub(crate) id: String,
    pub(crate) text: String,
}

/// 一次「开始朗读」请求的容量上限。页面是可信桥接，但消息仍按不可信输入校验：
/// 一段文字按合成引擎的单段上限收紧，整章列表再按总字节兜住。
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

/// 正在朗读的阅读窗口。朗读是进程级资源，这张表用来在开始新会话前停掉旧窗口。
#[derive(Default)]
pub(crate) struct ReadAloudRegistry {
    owner: Option<WeakEntity<ReaderApp>>,
}

impl gpui_kit::Global for ReadAloudRegistry {}

/// 登记本窗口为朗读窗口（开始朗读时调用）。
pub(crate) fn ensure_read_aloud_registry(cx: &mut App) {
    if !cx.has_global::<ReadAloudRegistry>() {
        cx.set_global(ReadAloudRegistry::default());
    }
}

/// 停掉其它窗口正在进行的朗读，并把登记表指向 `owner`。
pub(crate) fn claim_read_aloud_ownership(owner: WeakEntity<ReaderApp>, cx: &mut App) {
    ensure_read_aloud_registry(cx);
    let previous = cx.global_mut::<ReadAloudRegistry>().owner.replace(owner);
    if let Some(previous) = previous
        && let Some(view) = previous.upgrade()
    {
        view.update(cx, |this, cx| this.stop_read_aloud(cx));
    }
}

/// 清掉登记表（会话结束或窗口关闭时调用）。
pub(crate) fn release_read_aloud_ownership(cx: &mut App) {
    if cx.has_global::<ReadAloudRegistry>() {
        cx.global_mut::<ReadAloudRegistry>().owner = None;
    }
}

/// 阅读窗口的朗读状态。会话在工作线程上跑，事件经 `async_channel` 回到窗口线程。
#[derive(Default)]
pub(crate) struct ReaderReadAloud {
    session: Option<ReadAloudSession>,
    events: Option<async_channel::Receiver<ReadAloudEvent>>,
    current_block: Option<String>,
    state: Option<ReadAloudState>,
}

impl ReaderReadAloud {
    pub(crate) fn is_active(&self) -> bool {
        self.session.is_some()
    }

    pub(crate) fn state(&self) -> Option<ReadAloudState> {
        self.state
    }
}

impl ReaderApp {
    /// 从页面发来的段落列表开始朗读（或从 `start_block` 指向的那一段开始）。
    pub(crate) fn start_read_aloud(
        &mut self,
        blocks: Vec<ReadAloudPageBlock>,
        start_block: &str,
        cx: &mut Context<Self>,
    ) {
        if self.closing || !read_aloud_request_is_valid(&blocks) {
            return;
        }
        let settings = self.services.provider_settings().ok();
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
        claim_read_aloud_ownership(cx.entity().downgrade(), cx);

        let (sender, receiver) = async_channel::bounded::<ReadAloudEvent>(64);
        let session = match ReadAloudSession::start(
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
        ) {
            Ok(session) => session,
            Err(error) => {
                release_read_aloud_ownership(cx);
                self.set_error(format!("无法开始朗读：{error:#}"), cx);
                return;
            }
        };
        self.read_aloud.session = Some(session);
        self.read_aloud.events = Some(receiver);
        self.read_aloud.state = Some(ReadAloudState::Preparing);
        self.notice = None;
        self.start_read_aloud_poll(cx);
        cx.notify();
    }

    /// 轮询会话事件。与译文轮询同一思路：会话在自己的线程上跑，窗口按固定间隔
    /// 取事件，避免跨线程直接借用 GPUI。
    fn start_read_aloud_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let alive = view.update(cx, |this, cx| this.drain_read_aloud_events(cx));
                match alive {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// 处理积压的会话事件；返回会话是否还在继续。
    fn drain_read_aloud_events(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(events) = self.read_aloud.events.clone() else {
            return false;
        };
        let mut finished = None;
        while let Ok(event) = events.try_recv() {
            match event {
                ReadAloudEvent::StateChanged(state) => {
                    self.read_aloud.state = Some(state);
                }
                ReadAloudEvent::BlockStarted { block_id, .. } => {
                    self.read_aloud.current_block = Some(block_id.clone());
                    self.push_read_aloud_highlight(Some(block_id.as_str()), cx);
                }
                ReadAloudEvent::BlockFinished { .. } => {}
                ReadAloudEvent::Finished { error } => {
                    finished = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = finished {
            self.read_aloud.session = None;
            self.read_aloud.events = None;
            self.read_aloud.current_block = None;
            self.read_aloud.state = None;
            release_read_aloud_ownership(cx);
            self.push_read_aloud_highlight(None, cx);
            if let Some(error) = error {
                self.set_error(format!("朗读已停止：{error}"), cx);
            }
            cx.notify();
            return false;
        }
        cx.notify();
        true
    }

    pub(crate) fn pause_read_aloud(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.read_aloud.session.as_ref() {
            session.pause();
        }
        cx.notify();
    }

    pub(crate) fn resume_read_aloud(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.read_aloud.session.as_ref() {
            session.resume();
        }
        cx.notify();
    }

    /// 停止朗读并清掉页面高亮。幂等：没有会话时只清高亮。
    pub(crate) fn stop_read_aloud(&mut self, cx: &mut Context<Self>) {
        if let Some(mut session) = self.read_aloud.session.take() {
            session.stop();
        }
        let was_active = self.read_aloud.events.is_some() || self.read_aloud.state.is_some();
        self.read_aloud.events = None;
        self.read_aloud.current_block = None;
        self.read_aloud.state = None;
        if was_active {
            release_read_aloud_ownership(cx);
            self.push_read_aloud_highlight(None, cx);
            cx.notify();
        }
    }

    /// 工具栏的「朗读」：请页面把它**当前视口的第一段**作为起点发起朗读。
    ///
    /// 视口只有页面知道，所以这里只发一条脚本；页面收集段落与文字后走
    /// `read_aloud_start` 消息回来，宿主再启动会话。
    pub(crate) fn start_read_aloud_from_viewport(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        if let Err(error) = webview
            .read(cx)
            .raw()
            .evaluate_script("window.ngyReadAloud && window.ngyReadAloud.startFromViewport();")
        {
            tracing::warn!(%error, "cannot start read-aloud from the viewport");
        }
    }

    /// 让页面高亮（或清除）当前朗读段。高亮只走页面覆盖层，不动正文节点，
    /// 因此不会进入笔记锚点、选区文本或 AI 引用。
    pub(crate) fn push_read_aloud_highlight(
        &mut self,
        block_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        let script = read_aloud_highlight_script(block_id);
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            // 高亮只是呈现：丢一次更新不该升级成阅读器错误。
            tracing::warn!(%error, "cannot apply the read-aloud highlight");
        }
    }
}

/// 页面高亮脚本。页面侧对象可能还不存在（旧页面或脚本未注入完），调用要判空。
pub(crate) fn read_aloud_highlight_script(block_id: Option<&str>) -> String {
    match block_id {
        Some(id) => format!(
            "window.ngyReadAloud && window.ngyReadAloud.highlight({});",
            serde_json::to_string(id).unwrap_or_else(|_| "\"\"".to_string())
        ),
        None => "window.ngyReadAloud && window.ngyReadAloud.clear();".to_string(),
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

    #[test]
    fn the_highlight_script_never_injects_raw_page_input() {
        // 引号被 JSON 转义后，整串 id 仍然落在字符串字面量内部，不会提前闭合。
        let script = read_aloud_highlight_script(Some("block\");alert(1);//"));
        assert!(
            script.contains(r#"highlight("block\");alert(1);//");"#),
            "{script}"
        );
        assert!(script.starts_with("window.ngyReadAloud && "));
        assert_eq!(
            read_aloud_highlight_script(None),
            "window.ngyReadAloud && window.ngyReadAloud.clear();"
        );
    }
}
