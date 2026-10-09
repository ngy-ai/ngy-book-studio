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
    pub const RATE_MIN: f32 = 0.5;
    pub const RATE_MAX: f32 = 2.0;

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

    use anyhow::Context as _;
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::HSTRING;

    /// WinRT 的 `SpeakingRate` 以 0.0 为正常速度，负值变慢、正值变快；这里按
    /// 倍率差映射（1.0 倍 → 0.0），并钳到 WinRT 接受的范围，避免非法值让整次
    /// 合成失败。
    const WINRT_RATE_MIN: f64 = -1.0;
    const WINRT_RATE_MAX: f64 = 2.0;

    #[derive(Debug, Default)]
    pub(super) struct WinRtSpeechEngine;

    impl SpeechEngine for WinRtSpeechEngine {
        fn voices(&self) -> Result<Vec<SpeechVoice>> {
            ensure_apartment()?;
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

        fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>> {
            ensure_apartment()?;
            let synthesizer = SpeechSynthesizer::new().context("无法创建系统语音合成器")?;
            if !request.voice_id.is_empty() {
                let voice = find_voice(&request.voice_id)?;
                synthesizer
                    .SetVoice(&voice)
                    .with_context(|| format!("无法选用系统语音 {}", request.voice_id))?;
            }
            let options = synthesizer.Options().context("无法读取语音合成选项")?;
            let rate = f64::from(request.rate) - 1.0;
            options
                .SetSpeakingRate(rate.clamp(WINRT_RATE_MIN, WINRT_RATE_MAX))
                .context("无法设置朗读语速")?;
            let stream = synthesizer
                .SynthesizeTextToStreamAsync(&HSTRING::from(request.text.as_str()))
                .context("无法开始语音合成")?
                .get()
                .context("语音合成失败")?;
            read_stream(&stream)
        }
    }

    fn find_voice(voice_id: &str) -> Result<windows::Media::SpeechSynthesis::VoiceInformation> {
        let all = SpeechSynthesizer::AllVoices().context("无法枚举系统语音")?;
        let count = all.Size().context("无法读取系统语音数量")?;
        for index in 0..count {
            let voice = all.GetAt(index).context("无法读取系统语音")?;
            if let Ok(id) = voice.Id()
                && id.to_string() == voice_id
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

    /// WinRT 调用要求当前线程已进入 COM 单元。朗读线程是我们自己起的阻塞线程，
    /// 这里按 MTA 初始化；失败（例如线程已在别的单元）不在这里拦，交给真正的
    /// WinRT 调用去报错，避免把「已经初始化」误判成错误。
    fn ensure_apartment() -> Result<()> {
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        Ok(())
    }
}
