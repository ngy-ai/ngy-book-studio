//! 朗读会话：把一串段落按顺序合成、播放、暂停与跳转。
//!
//! 一次朗读就是一个会话：调用方给出段落列表与起始位置，会话在自己的线程上
//! 「合成下一段 → 播当前段 → 推进」；界面只通过事件回调拿到状态与当前段，
//! 通过 [`ReadAloudSession::pause`] 等方法发指令。会话是进程级资源：调用方必须
//! 保证同一时刻只有一个会话在跑（新会话开始前先 [`ReadAloudSession::stop`] 旧的）。
//!
//! 线程模型：一根会话线程负责播放与状态机（20ms 轮询播放器状态），每段合成在
//! 自己的短线程上跑并把字节送回——合成是百毫秒级的阻塞调用，不能挡住暂停/停止。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context as _, Result};

use super::player::AudioPlayer;
use super::{SpeechEngine, SpeechRequest};

/// 会话里的一段待朗读内容。`id` 由页面给出，界面用它高亮当前段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadAloudBlock {
    pub id: String,
    pub text: String,
}

/// 会话状态，供界面渲染工具栏按钮的可用性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadAloudState {
    /// 正在合成还没有可播的音频（点击后到出声之间的状态）。
    Preparing,
    Speaking,
    Paused,
}

/// 会话推给界面的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadAloudEvent {
    StateChanged(ReadAloudState),
    /// 第 `index` 段开始播放：界面高亮该段并滚动到可视区。
    BlockStarted {
        index: usize,
        block_id: String,
    },
    /// 第 `index` 段播放完成。
    BlockFinished {
        index: usize,
    },
    /// 会话结束（读完、被停止，或出错）。界面统一在这里清理高亮。
    Finished {
        error: Option<String>,
    },
}

/// 一次会话的输入。
pub struct ReadAloudRequest {
    pub blocks: Vec<ReadAloudBlock>,
    /// 从哪一段开始读，越界会被钳到合法范围。
    pub start_index: usize,
    /// 音色 ID；空串表示系统默认语音。
    pub voice_id: String,
    pub rate: f32,
}

enum Command {
    Pause,
    Resume,
    Jump(usize),
    Stop,
}

/// 一次朗读会话的句柄。Drop 会停止并回收线程。
pub struct ReadAloudSession {
    commands: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl ReadAloudSession {
    /// 启动会话。`on_event` 会在会话线程上被调用，调用方负责把它转到自己的线程
    /// （GPUI 侧用一个 channel 或 `cx.update`）。
    pub fn start(
        engine: Arc<dyn SpeechEngine>,
        request: ReadAloudRequest,
        on_event: impl Fn(ReadAloudEvent) + Send + 'static,
    ) -> Result<Self> {
        anyhow::ensure!(!request.blocks.is_empty(), "没有可朗读的内容");
        let (commands, command_rx) = channel();
        let worker = std::thread::Builder::new()
            .name("ngy-read-aloud".to_string())
            .spawn(move || run_session(engine, request, command_rx, on_event))
            .context("无法启动朗读线程")?;
        Ok(Self {
            commands,
            worker: Some(worker),
        })
    }

    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    pub fn resume(&self) {
        let _ = self.commands.send(Command::Resume);
    }

    /// 跳到某一段（越界会被钳制）。
    pub fn jump(&self, index: usize) {
        let _ = self.commands.send(Command::Jump(index));
    }

    /// 停止并等待线程退出。
    pub fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ReadAloudSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 会话主循环：命令、合成回传、预取与播放状态推进。
fn run_session(
    engine: Arc<dyn SpeechEngine>,
    request: ReadAloudRequest,
    commands: Receiver<Command>,
    on_event: impl Fn(ReadAloudEvent),
) {
    let ReadAloudRequest {
        blocks,
        start_index,
        voice_id,
        rate,
    } = request;
    let last = blocks.len() - 1;
    let mut current = start_index.min(last);
    let mut state = ReadAloudState::Preparing;

    let mut player = match AudioPlayer::new() {
        Ok(player) => player,
        Err(error) => {
            on_event(ReadAloudEvent::Finished {
                error: Some(format!("{error:#}")),
            });
            return;
        }
    };

    let (synth_tx, synth_rx) = channel::<(usize, Result<Vec<u8>>)>();
    let mut ready: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut in_flight: HashSet<usize> = HashSet::new();

    // 合成最多提前两段（当前段与下一段）：再多只是白占内存，再少会在换段时露空。
    let prefetch =
        |index: usize, ready: &mut HashMap<usize, Vec<u8>>, in_flight: &mut HashSet<usize>| {
            if index > last || ready.contains_key(&index) || in_flight.contains(&index) {
                return;
            }
            in_flight.insert(index);
            let tx = synth_tx.clone();
            let engine = Arc::clone(&engine);
            let text = blocks[index].text.clone();
            let voice = voice_id.clone();
            let _ = std::thread::Builder::new()
                .name("ngy-read-aloud-synth".to_string())
                .spawn(move || {
                    let result = engine.synthesize(&SpeechRequest::new(text, voice, rate));
                    let _ = tx.send((index, result));
                });
        };

    let emit_state = |state: ReadAloudState, on_event: &dyn Fn(ReadAloudEvent)| {
        on_event(ReadAloudEvent::StateChanged(state));
    };
    emit_state(state, &on_event);

    loop {
        // 1. 指令优先：停止与暂停必须在合成还没回来时也能立刻生效。
        loop {
            match commands.try_recv() {
                Ok(Command::Pause) => {
                    if state == ReadAloudState::Speaking && player.pause().is_ok() {
                        state = ReadAloudState::Paused;
                        emit_state(state, &on_event);
                    }
                }
                Ok(Command::Resume) => {
                    if state == ReadAloudState::Paused && player.resume().is_ok() {
                        state = ReadAloudState::Speaking;
                        emit_state(state, &on_event);
                    }
                }
                Ok(Command::Jump(index)) => {
                    let index = index.min(last);
                    player.stop();
                    ready.clear();
                    current = index;
                    state = ReadAloudState::Preparing;
                    emit_state(state, &on_event);
                }
                Ok(Command::Stop) => {
                    player.stop();
                    on_event(ReadAloudEvent::Finished { error: None });
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    player.stop();
                    return;
                }
            }
        }

        // 2. 合成回传：失败的一段直接结束会话并把原因交给界面。
        while let Ok((index, result)) = synth_rx.try_recv() {
            in_flight.remove(&index);
            match result {
                Ok(bytes) => {
                    ready.insert(index, bytes);
                }
                Err(error) => {
                    player.stop();
                    on_event(ReadAloudEvent::Finished {
                        error: Some(format!("{error:#}")),
                    });
                    return;
                }
            }
        }

        // 3. 预取当前段与下一段。
        prefetch(current, &mut ready, &mut in_flight);
        prefetch(current + 1, &mut ready, &mut in_flight);

        // 4. 推进播放状态。
        match state {
            ReadAloudState::Preparing => {
                if let Some(bytes) = ready.remove(&current) {
                    if let Err(error) = player.play_wav(&bytes) {
                        on_event(ReadAloudEvent::Finished {
                            error: Some(format!("{error:#}")),
                        });
                        return;
                    }
                    on_event(ReadAloudEvent::BlockStarted {
                        index: current,
                        block_id: blocks[current].id.clone(),
                    });
                    state = ReadAloudState::Speaking;
                    emit_state(state, &on_event);
                }
            }
            ReadAloudState::Speaking => match player.finished() {
                Ok(false) => {}
                Ok(true) => {
                    on_event(ReadAloudEvent::BlockFinished { index: current });
                    if current >= last {
                        on_event(ReadAloudEvent::Finished { error: None });
                        return;
                    }
                    current += 1;
                    // 已经读过的段落不再需要音频。
                    ready.retain(|index, _| *index >= current);
                    state = ReadAloudState::Preparing;
                    emit_state(state, &on_event);
                }
                Err(error) => {
                    on_event(ReadAloudEvent::Finished {
                        error: Some(format!("{error:#}")),
                    });
                    return;
                }
            },
            ReadAloudState::Paused => {}
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}
