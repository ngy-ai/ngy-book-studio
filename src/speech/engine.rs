//! 合成引擎：一段文字 → 一份可播放的音频（WAV 字节）。
//!
//! Windows 实现走 WinRT `SpeechSynthesizer`，macOS 实现走 AVFAudio 的
//! `AVSpeechSynthesizer`：都是系统语音离线合成，合成结果是一份 WAV 流。其它
//! 平台没有实现，朗读整体不可用，调用方拿到明确错误而不是 panic。
//!
//! macOS 分支已通过 `aarch64-apple-darwin` 的交叉编译检查（2026-10-10），但
//! 尚未在 macOS 上实际运行验证；语速换算也还没有实测标定。

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

/// 平台默认引擎。Windows 与 macOS 上是系统语音；其它平台返回一个只会报错的实现。
pub fn platform_engine() -> Arc<dyn SpeechEngine> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(winrt::WinRtSpeechEngine)
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(avf::AvfSpeechEngine)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Arc::new(UnsupportedSpeechEngine)
    }
}

/// 没有系统语音实现时的占位：朗读不可用，但错误信息要说清楚原因。
#[derive(Debug, Default)]
pub struct UnsupportedSpeechEngine;

impl SpeechEngine for UnsupportedSpeechEngine {
    fn voices(&self) -> Result<Vec<SpeechVoice>> {
        anyhow::bail!("朗读目前只支持 Windows 与 macOS 的系统语音")
    }

    fn synthesize(&self, _request: &SpeechRequest) -> Result<Vec<u8>> {
        anyhow::bail!("朗读目前只支持 Windows 与 macOS 的系统语音")
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

/// macOS 实现：AVFAudio 的 `AVSpeechSynthesizer` 合成到内存缓冲，再编码成
/// 16-bit 单声道 PCM 的 WAV。
///
/// 与 `winrt` 模块的差别：这里没有常驻工作线程——每个 `AVSpeechSynthesizer`
/// 实例自带内部队列，回调经 `std::sync::mpsc` 桥回调用线程，因此可以并发合成
/// （朗读会话的预取会同时跑两段）。
#[cfg(target_os = "macos")]
mod avf {
    use super::*;

    use std::ptr::NonNull;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use anyhow::{anyhow, bail, ensure};
    use block2::RcBlock;
    use objc2_avf_audio::{
        AVAudioBuffer, AVAudioCommonFormat, AVAudioPCMBuffer, AVSpeechSynthesisVoice,
        AVSpeechSynthesizer, AVSpeechUtterance,
    };
    use objc2_foundation::NSString;

    /// 合成回调送回的一块数据。
    enum Chunk {
        /// 一段单声道 PCM 与它的采样率。
        Data { sample_rate: f64, samples: Vec<f32> },
        /// 回调报告 `frameLength == 0`：合成结束。
        Done,
    }

    #[derive(Debug, Default)]
    pub(super) struct AvfSpeechEngine;

    impl SpeechEngine for AvfSpeechEngine {
        fn voices(&self) -> Result<Vec<SpeechVoice>> {
            collect_voices()
        }

        fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>> {
            synthesize_wav(request)
        }
    }

    fn collect_voices() -> Result<Vec<SpeechVoice>> {
        let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
        let mut result = Vec::new();
        for voice in voices.to_vec() {
            // SAFETY: 访问器只读系统语音的属性。
            let (id, label, language) = unsafe {
                (
                    voice.identifier().to_string(),
                    voice.name().to_string(),
                    voice.language().to_string(),
                )
            };
            if id.trim().is_empty() {
                continue;
            }
            result.push(SpeechVoice {
                id,
                label,
                language,
            });
        }
        Ok(result)
    }

    /// `AVSpeechUtterance.rate` 是 0.0..=1.0 的绝对值（默认 0.5），不是倍率：
    /// 按「默认档 = 应用层 1.0」线性换算，`RATE_MAX` 因此会被钳到区间内。
    ///
    /// 注意：macOS 侧尚未实测标定（Windows 的倍率曲线见 `WINRT_RATE_MIN` 的
    /// 注释）；首次在 macOS 上人工验证时要核对 0.75x–2.0x 的手感是否接近倍率。
    fn utterance_rate(rate: f32) -> f32 {
        (0.5 * rate).clamp(0.0, 1.0)
    }

    fn synthesize_wav(request: &SpeechRequest) -> Result<Vec<u8>> {
        let text = NSString::from_str(&request.text);
        let utterance = unsafe { AVSpeechUtterance::speechUtteranceWithString(&text) };
        if !request.voice_id.is_empty() {
            let identifier = NSString::from_str(&request.voice_id);
            let voice = unsafe { AVSpeechSynthesisVoice::voiceWithIdentifier(&identifier) }
                .ok_or_else(|| anyhow!("找不到系统语音 {}", request.voice_id))?;
            unsafe { utterance.setVoice(Some(&voice)) };
        }
        unsafe { utterance.setRate(utterance_rate(request.rate)) };

        let synthesizer = unsafe { AVSpeechSynthesizer::new() };
        let (sender, receiver) = channel::<Chunk>();
        let done = sender.clone();
        let callback = RcBlock::new(move |buffer: NonNull<AVAudioBuffer>| {
            // SAFETY: 回调保证 buffer 非空且在回调期间有效。
            let buffer = unsafe { buffer.as_ref() };
            let Some(pcm) = buffer.downcast_ref::<AVAudioPCMBuffer>() else {
                return;
            };
            // SAFETY: 下面的访问器都只在回调期间使用对象，满足前置条件。
            let frames = unsafe { pcm.frameLength() };
            if frames == 0 {
                let _ = done.send(Chunk::Done);
                return;
            }
            let format = unsafe { pcm.format() };
            let sample_rate = unsafe { format.sampleRate() };
            let channels = unsafe { format.channelCount() } as usize;
            if channels == 0 {
                return;
            }
            // 系统语音是单声道；多声道时只读第一条（不做交织处理）。
            let samples = match unsafe { format.commonFormat() } {
                AVAudioCommonFormat::PCMFormatFloat32 => {
                    let data = unsafe { pcm.floatChannelData() };
                    if data.is_null() {
                        return;
                    }
                    // SAFETY: 指针数组至少有一条声道（channels > 0），前 frames 个
                    // 样本由缓冲保证有效。
                    let first = unsafe { (*data).as_ptr() };
                    unsafe { std::slice::from_raw_parts(first, frames as usize) }
                        .iter()
                        .copied()
                        .collect::<Vec<f32>>()
                }
                AVAudioCommonFormat::PCMFormatInt16 => {
                    let data = unsafe { pcm.int16ChannelData() };
                    if data.is_null() {
                        return;
                    }
                    // SAFETY: 同上（int16 视图）。
                    let first = unsafe { (*data).as_ptr() };
                    unsafe { std::slice::from_raw_parts(first, frames as usize) }
                        .iter()
                        .map(|sample| f32::from(*sample) / 32768.0)
                        .collect::<Vec<f32>>()
                }
                _ => return,
            };
            let _ = sender.send(Chunk::Data {
                sample_rate,
                samples,
            });
        });

        // SAFETY: 回调指针来自仍然存活的 RcBlock；在收到 Done 或超时之前这里一直
        // 等待，block 不会在回调仍在进行时被释放。
        unsafe {
            synthesizer
                .writeUtterance_toBufferCallback(&utterance, RcBlock::as_ptr(&callback).cast());
        }

        let (mut samples, mut sample_rate) = (Vec::new(), 0.0_f64);
        loop {
            match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(Chunk::Data {
                    sample_rate: chunk_rate,
                    samples: chunk,
                }) => {
                    if samples.is_empty() {
                        sample_rate = chunk_rate;
                    }
                    samples.extend(chunk);
                }
                Ok(Chunk::Done) => break,
                Err(error) => {
                    // 超时或通道断开：已有音频就按结束处理，否则报错。
                    if samples.is_empty() {
                        bail!("语音合成没有产出音频：{error}");
                    }
                    break;
                }
            }
        }
        ensure!(!samples.is_empty(), "语音合成没有产出音频");
        Ok(wav_from_f32_mono(&samples, sample_rate))
    }

    /// 把单声道 f32 样本编码成 16-bit PCM 的 WAV（标准的 44 字节头）。
    fn wav_from_f32_mono(samples: &[f32], sample_rate: f64) -> Vec<u8> {
        let sample_rate = sample_rate.max(1.0).round() as u32;
        let channels = 1u16;
        let bits = 16u16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
        let block_align = channels * bits / 8;
        let data_bytes = (samples.len() * 2) as u32;

        let mut wav = Vec::with_capacity(44 + data_bytes as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            wav.extend_from_slice(&value.to_le_bytes());
        }
        wav
    }
}
