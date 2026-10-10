//! 合成引擎：一段文字 → 一份可播放的音频（WAV 字节）。
//!
//! Windows 实现走 WinRT `SpeechSynthesizer`：系统语音离线合成，合成结果是一份
//! WAV 流。非 Windows 平台没有实现，朗读整体不可用，调用方拿到明确错误而不是 panic。

use std::sync::Arc;

use anyhow::Result;

/// 一个可用的系统语音。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechVoice {
    /// 引擎内稳定的语音 ID（持久化到设置里的就是它）。
    pub id: String,
    /// 面向用户的名称，例如 `Microsoft Huihui`。
    pub label: String,
    /// 主语言子标签，例如 `zh-CN`。
    pub language: String,
}

/// 一次合成请求。
#[derive(Clone, Debug, PartialEq)]
pub struct SpeechRequest {
    pub text: String,
    /// [`SpeechVoice::id`]；空串表示用引擎默认语音。
    pub voice_id: String,
    /// 语速倍率：1.0 是正常速度，0.5 是半速，2.0 是两倍。实现方负责钳制到
    /// 引擎自己的范围。
    pub rate: f32,
}

impl SpeechRequest {
    /// 语速倍率的允许范围。上下界同时用于设置校验与合成钳制，避免两边各写一份。
    /// 界面的预设档位（0.75x–2.0x）与下界都在系统语音的原生表达能力内；上界
    /// 10.0 超出（实测约 5.7 倍处饱和），超出的部分需要重采样补齐，尚未实现。
    pub const RATE_MIN: f32 = 0.5;
    pub const RATE_MAX: f32 = 10.0;

    pub fn new(text: impl Into<String>, voice_id: impl Into<String>, rate: f32) -> Self {
        Self {
            text: text.into(),
            voice_id: voice_id.into(),
            rate: rate.clamp(Self::RATE_MIN, Self::RATE_MAX),
        }
    }
}

/// 引擎边界。实现必须可跨线程共享，且合成是阻塞调用。
pub trait SpeechEngine: Send + Sync {
    /// 当前系统可用的语音，按引擎自己的顺序返回。
    fn voices(&self) -> Result<Vec<SpeechVoice>>;

    /// 合成一段文字，返回完整 WAV 字节。
    fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>>;
}

/// 平台默认引擎。Windows 上是系统语音；其它平台返回一个只会报错的实现。
pub fn platform_engine() -> Arc<dyn SpeechEngine> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(winrt::WinRtSpeechEngine)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Arc::new(UnsupportedSpeechEngine)
    }
}

/// 非 Windows 平台的占位实现：朗读不可用，但错误信息要说清楚原因。
#[derive(Debug, Default)]
pub struct UnsupportedSpeechEngine;

impl SpeechEngine for UnsupportedSpeechEngine {
    fn voices(&self) -> Result<Vec<SpeechVoice>> {
        anyhow::bail!("朗读目前只支持 Windows 系统语音")
    }

    fn synthesize(&self, _request: &SpeechRequest) -> Result<Vec<u8>> {
        anyhow::bail!("朗读目前只支持 Windows 系统语音")
    }
}

#[cfg(target_os = "windows")]
mod winrt {
    use super::*;

    use std::sync::OnceLock;
    use std::sync::mpsc::{Sender, channel};

    use anyhow::Context as _;
    use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
    use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::HSTRING;

    /// WinRT `SpeakingRate` 的实测有效范围与语义（2026-10-10 在 Huihui 与
    /// David 上实测，曲线由引擎决定、与具体语音无关）：
    /// - `0.2..=5.0` 内朗读时长与 `rate` 成反比（`rate` 档的时长 = `1.0` 档
    ///   的时长 / `rate`），即 rate 本身就是倍率，映射是恒等；
    /// - 负值被引擎忽略（与 0.0 输出逐字节一致），0.0 是语音库自己的慢速
    ///   基准（约为 1.0 档的 1/5.7），所以不能用负值表达慢速；
    /// - 大于 5.0 后增益递减，约 5.7 倍处饱和。
    ///
    /// 钳制只用于兜底：`SpeechRequest` 已把 rate 限制在 `0.5..=10.0`。
    const WINRT_RATE_MIN: f64 = 0.2;
    const WINRT_RATE_MAX: f64 = 5.0;

    #[derive(Debug, Default)]
    pub(super) struct WinRtSpeechEngine;

    impl SpeechEngine for WinRtSpeechEngine {
        fn voices(&self) -> Result<Vec<SpeechVoice>> {
            with_winrt(collect_voices)
        }

        fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>> {
            let request = request.clone();
            with_winrt(move || synthesize_wav(&request))
        }
    }

    /// 投递到 WinRT 线程的一次任务。
    type WinrtJob = Box<dyn FnOnce() + Send>;

    static WINRT_WORKER: OnceLock<Sender<WinrtJob>> = OnceLock::new();

    /// 所有 WinRT 调用都投递到这一个常驻线程上执行。
    ///
    /// 这样同时绕开两个坑：
    /// 1. `windows` crate 的工厂激活缓存把接口指针缓存在进程里，**跨公寓**使用会
    ///    打出访问违例（0xC0000005；并行跑整套测试时在 `AllVoices` 上随机复现，
    ///    加锁并不能修——问题不在并发，而在缓存对象被别的公寓的线程使用）；
    /// 2. `RoInitialize` 在已经是 STA 的线程上会以 `RPC_E_CHANGED_MODE` 失败，
    ///    静默忽略就把 WinRT 调用留在了没有正确公寓的线程上。
    ///
    /// 工作线程只初始化一次 MTA 公寓，之后所有调用都在同一个正确的公寓里；朗读
    /// 本身是串行资源（同一时刻只允许一个会话，试听也会打断朗读），一次线程切换
    /// 的开销可以忽略。
    fn with_winrt<T: Send + 'static>(
        job: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = channel();
        winrt_worker()
            .send(Box::new(move || {
                let _ = sender.send(job());
            }))
            .map_err(|_| anyhow::anyhow!("WinRT 工作线程已退出"))?;
        receiver
            .recv()
            .map_err(|_| anyhow::anyhow!("WinRT 工作线程已退出"))?
    }

    fn winrt_worker() -> &'static Sender<WinrtJob> {
        WINRT_WORKER.get_or_init(|| {
            let (sender, receiver) = channel::<WinrtJob>();
            std::thread::Builder::new()
                .name("ngy-winrt".to_string())
                .spawn(move || {
                    // 失败（例如线程已经在别的公寓）不在这里拦，交给真正的 WinRT
                    // 调用去报错如何应对。
                    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
                    while let Ok(job) = receiver.recv() {
                        job();
                    }
                })
                .expect("启动 WinRT 工作线程");
            sender
        })
    }

    fn collect_voices() -> Result<Vec<SpeechVoice>> {
        let all = SpeechSynthesizer::AllVoices().context("无法枚举系统语音")?;
        let count = all.Size().context("无法读取系统语音数量")?;
        let mut voices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let voice = all.GetAt(index).context("无法读取系统语音")?;
            let id = voice.Id().context("无法读取系统语音 ID")?.to_string();
            let label = voice
                .DisplayName()
                .context("无法读取系统语音名称")?
                .to_string();
            let language = voice
                .Language()
                .context("无法读取系统语音语言")?
                .to_string();
            // 没有 ID 的语音无法持久化，也就无法再次选中：跳过。
            if id.trim().is_empty() {
                continue;
            }
            voices.push(SpeechVoice {
                id,
                label,
                language,
            });
        }
        Ok(voices)
    }

    fn synthesize_wav(request: &SpeechRequest) -> Result<Vec<u8>> {
        let synthesizer = SpeechSynthesizer::new().context("无法创建系统语音合成器")?;
        if !request.voice_id.is_empty() {
            let voice = find_voice(&request.voice_id)?;
            synthesizer
                .SetVoice(&voice)
                .with_context(|| format!("无法选用系统语音 {}", request.voice_id))?;
        }
        let options = synthesizer.Options().context("无法读取语音合成选项")?;
        // 恒等映射：WinRT 的 rate 实测就是倍率（见 `WINRT_RATE_MIN` 的注释）。
        options
            .SetSpeakingRate(f64::from(request.rate).clamp(WINRT_RATE_MIN, WINRT_RATE_MAX))
            .context("无法设置朗读语速")?;
        let stream = synthesizer
            .SynthesizeTextToStreamAsync(&HSTRING::from(request.text.as_str()))
            .context("无法开始语音合成")?
            .get()
            .context("语音合成失败")?;
        read_stream(&stream)
    }

    fn find_voice(voice_id: &str) -> Result<VoiceInformation> {
        let all = SpeechSynthesizer::AllVoices().context("无法枚举系统语音")?;
        let count = all.Size().context("无法读取系统语音数量")?;
        for index in 0..count {
            let voice = all.GetAt(index).context("无法读取系统语音")?;
            if let Ok(id) = voice.Id()
                && id == voice_id
            {
                return Ok(voice);
            }
        }
        anyhow::bail!("系统语音 {voice_id} 已不存在")
    }

    /// 把合成流整段读成字节。朗读按段落合成，一段 WAV 远小于 `u32` 上限。
    fn read_stream(
        stream: &windows::Media::SpeechSynthesis::SpeechSynthesisStream,
    ) -> Result<Vec<u8>> {
        let size = stream.Size().context("无法读取合成音频长度")?;
        anyhow::ensure!(size > 0, "语音合成没有产生音频");
        let size = u32::try_from(size).context("单段合成音频过大")?;
        let buffer = Buffer::Create(size).context("无法创建合成音频缓冲")?;
        let filled = stream
            .ReadAsync(&buffer, size, InputStreamOptions::None)
            .context("无法读取合成音频")?
            .get()
            .context("读取合成音频失败")?;
        let reader = DataReader::FromBuffer(&filled).context("无法创建合成音频读取器")?;
        let mut bytes = vec![0u8; size as usize];
        reader
            .ReadBytes(&mut bytes)
            .context("无法取出合成音频字节")?;
        anyhow::ensure!(!bytes.is_empty(), "语音合成没有产生音频");
        Ok(bytes)
    }
}
