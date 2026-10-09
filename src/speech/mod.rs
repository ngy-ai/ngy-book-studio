//! 朗读（TTS）的引擎边界与朗读会话。
//!
//! 界面只依赖 [`SpeechEngine`] 边界：当前实现是 Windows 系统语音（WinRT
//! `SpeechSynthesizer`），零模型、零新依赖、离线、首字延迟低于 100ms。以后要换成
//! 更自然的本地模型（Piper / Kokoro）或云端 TTS，只需另写一个实现，界面与朗读
//! 会话不改。
//!
//! 约束：
//! - 朗读是**进程级**资源（系统语音与音频输出各只有一份）：同一时刻只允许一个
//!   朗读会话，新的会话开始前必须停掉旧的（见 [`ReadAloudSession`]）。
//! - 合成与播放都是阻塞操作，必须在专用线程上跑，绝不占用 GPUI 回调线程。
//! - 朗读不写数据库、不改正文：当前朗读段只通过页面覆盖层高亮，原文、笔记锚点
//!   与选区文本都不受影响。

mod engine;

mod player;

mod session;

pub use engine::*;

pub use session::*;

/// 试听一句话：用系统语音合成后同步播放到底。供设置窗口的「试听」按钮使用。
///
/// 会占用应用唯一的音频输出：试听与朗读不能同时出声，调用方要在界面上说明这一点。
pub fn preview(voice_id: &str, rate: f32) -> anyhow::Result<()> {
    const PREVIEW_TEXT: &str = "这是朗读的试听效果。";
    let engine = platform_engine();
    let wav = engine.synthesize(&SpeechRequest::new(PREVIEW_TEXT, voice_id, rate))?;
    let mut player = player::AudioPlayer::new()?;
    player.play_wav(&wav)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !player.finished()? {
        anyhow::ensure!(std::time::Instant::now() < deadline, "试听播放超时");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    player.stop();
    Ok(())
}

#[cfg(test)]
mod tests;
