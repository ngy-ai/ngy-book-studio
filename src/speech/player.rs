//! 播放后端：把一段 WAV 字节播出来，支持暂停、继续与停止。
//!
//! Windows 上用 WinRT `MediaPlayer`（系统媒体播放器，零新依赖）：把内存里的
//! WAV 字节包成内存流再交给它；macOS 上用 `AVAudioPlayer`（AVFAudio 绑定），
//! 直接吃内存数据。播放状态按轮询读取——朗读会话在自己的线程上 20ms 轮询一次，
//! 比事件回调少一层桥接细节，也让「播完换下一段」的状态机保持简单。
//! 其它平台没有实现，创建播放器返回明确错误。

#[cfg(target_os = "windows")]
pub(crate) use windows_impl::AudioPlayer;

#[cfg(target_os = "macos")]
pub(crate) use avf_audio::AudioPlayer;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) use unsupported::AudioPlayer;

#[cfg(target_os = "windows")]
mod windows_impl {
    use anyhow::{Context as _, Result, ensure};
    use windows::Foundation::TimeSpan;
    use windows::Media::Core::MediaSource;
    use windows::Media::Playback::{MediaPlaybackState, MediaPlayer};
    use windows::Storage::Streams::{DataWriter, IRandomAccessStream, InMemoryRandomAccessStream};
    use windows::core::{HSTRING, Interface as _};

    /// 合成音频的内容类型：系统语音产出的就是 WAV。
    const AUDIO_CONTENT_TYPE: &str = "audio/wav";

    /// 一段音频的播放句柄。整个生命周期只在一根线程上使用（朗读会话的工作线程）。
    pub(crate) struct AudioPlayer {
        player: MediaPlayer,
        /// 已经观察到进入过播放状态：刚 `SetSource` 时状态也是 `None`，不能用它
        /// 判断「播完了」。
        started: bool,
    }

    impl AudioPlayer {
        pub(crate) fn new() -> Result<Self> {
            let player = MediaPlayer::new().context("无法创建系统音频播放器")?;
            Ok(Self {
                player,
                started: false,
            })
        }

        /// 播放一份 WAV 字节，替换掉当前音频（上一段的播放随即结束）。
        pub(crate) fn play_wav(&mut self, bytes: &[u8]) -> Result<()> {
            ensure!(!bytes.is_empty(), "没有可播放的音频");
            let stream = InMemoryRandomAccessStream::new().context("无法创建音频内存流")?;
            let writer = DataWriter::CreateDataWriter(&stream).context("无法写入音频内存流")?;
            writer.WriteBytes(bytes).context("无法写入音频数据")?;
            let stored = writer
                .StoreAsync()
                .context("无法提交音频数据")?
                .get()
                .context("提交音频数据失败")?;
            ensure!(stored as usize == bytes.len(), "音频内存流写入不完整");
            writer
                .FlushAsync()
                .context("无法刷新音频数据")?
                .get()
                .context("刷新音频数据失败")?;
            writer.DetachStream().context("无法分离音频内存流")?;
            writer.Close().context("无法关闭音频写入器")?;
            // DataWriter 写完后流的游标在末尾，必须回到起点，否则播出来是空的。
            stream.Seek(0).context("无法定位音频内存流")?;
            let source = MediaSource::CreateFromStream(
                &stream
                    .cast::<IRandomAccessStream>()
                    .context("无法转换音频内存流")?,
                &HSTRING::from(AUDIO_CONTENT_TYPE),
            )
            .context("无法创建音频源")?;
            self.player.SetSource(&source).context("无法装载朗读音频")?;
            self.started = false;
            self.player.Play().context("无法开始播放朗读音频")?;
            Ok(())
        }

        pub(crate) fn pause(&mut self) -> Result<()> {
            self.player.Pause().context("无法暂停朗读")
        }

        pub(crate) fn resume(&mut self) -> Result<()> {
            self.player.Play().context("无法继续朗读")
        }

        /// 当前音频是否已经播完（或已被替换/停止）。
        pub(crate) fn finished(&mut self) -> Result<bool> {
            let session = self.player.PlaybackSession().context("无法读取播放状态")?;
            let state = session.PlaybackState().context("无法读取播放状态")?;
            if matches!(state, MediaPlaybackState::Playing) {
                self.started = true;
                return Ok(false);
            }
            if !self.started {
                // 还没进入播放状态：可能仍在缓冲，也可能一开始就失败。
                return Ok(false);
            }
            if matches!(state, MediaPlaybackState::None) {
                return Ok(true);
            }
            // 有些设备播完后停在 Paused 而不是 None：用播放位置兜底。
            let position: TimeSpan = session.Position().context("无法读取播放位置")?;
            let duration: TimeSpan = session.NaturalDuration().context("无法读取音频时长")?;
            Ok(duration.Duration > 0 && position.Duration >= duration.Duration)
        }

        /// 停止播放并丢弃当前音频。
        pub(crate) fn stop(&mut self) {
            let _ = self.player.Pause();
            let _ = self.player.SetSource(None);
            self.started = false;
        }
    }
}

#[cfg(target_os = "macos")]
mod avf_audio {
    use anyhow::{Result, anyhow, ensure};
    use objc2::AnyThread as _;
    use objc2::rc::Retained;
    use objc2_avf_audio::AVAudioPlayer;
    use objc2_foundation::NSData;

    /// 一段音频的播放句柄。整个生命周期只在一根线程上使用（朗读会话的工作线程）。
    ///
    /// `AVAudioPlayer` 必须带着音频数据创建，所以句柄在第一次 `play_wav` 之前是
    /// 空的；播放中 `isPlaying` 直接就是「播完没有」的判据（本地内存数据，没有
    /// 网络/缓冲造成的假暂停）。
    pub(crate) struct AudioPlayer {
        player: Option<Retained<AVAudioPlayer>>,
        /// 播放期间保活音频数据（`AVAudioPlayer` 对 `NSData` 的持有语义没有明文保证）。
        data: Option<Retained<NSData>>,
        started: bool,
    }

    impl AudioPlayer {
        pub(crate) fn new() -> Result<Self> {
            Ok(Self {
                player: None,
                data: None,
                started: false,
            })
        }

        /// 播放一份 WAV 字节，替换掉当前音频（上一段的播放随即结束）。
        pub(crate) fn play_wav(&mut self, bytes: &[u8]) -> Result<()> {
            ensure!(!bytes.is_empty(), "没有可播放的音频");
            let data = NSData::with_bytes(bytes);
            // SAFETY: data 是有效的 NSData；失败时返回 NSError。
            let player =
                unsafe { AVAudioPlayer::initWithData_error(AVAudioPlayer::alloc(), &data) }
                    .map_err(|error| {
                        anyhow!("无法装载朗读音频：{}", error.localizedDescription())
                    })?;
            // SAFETY: 播放器刚创建，尚未播放。
            let started = unsafe { player.play() };
            ensure!(started, "无法开始播放朗读音频");
            self.player = Some(player);
            self.data = Some(data);
            self.started = true;
            Ok(())
        }

        pub(crate) fn pause(&mut self) -> Result<()> {
            if let Some(player) = self.player.as_ref() {
                // SAFETY: 播放器在句柄存活期间一直有效。
                unsafe { player.pause() };
            }
            Ok(())
        }

        pub(crate) fn resume(&mut self) -> Result<()> {
            if let Some(player) = self.player.as_ref() {
                // SAFETY: 同上。
                unsafe { player.play() };
            }
            Ok(())
        }

        /// 当前音频是否已经播完（或已被替换/停止）。
        pub(crate) fn finished(&mut self) -> Result<bool> {
            let Some(player) = self.player.as_ref() else {
                return Ok(false);
            };
            if !self.started {
                return Ok(false);
            }
            // SAFETY: 同上。
            Ok(!unsafe { player.isPlaying() })
        }

        /// 停止播放并丢弃当前音频。
        pub(crate) fn stop(&mut self) {
            if let Some(player) = self.player.as_ref() {
                // SAFETY: 同上。
                unsafe { player.stop() };
            }
            self.player = None;
            self.data = None;
            self.started = false;
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod unsupported {
    use anyhow::Result;

    #[derive(Debug, Default)]
    pub(crate) struct AudioPlayer;

    impl AudioPlayer {
        pub(crate) fn new() -> Result<Self> {
            anyhow::bail!("朗读目前只支持 Windows 与 macOS 的系统语音")
        }

        pub(crate) fn play_wav(&mut self, _bytes: &[u8]) -> Result<()> {
            anyhow::bail!("朗读目前只支持 Windows 与 macOS 的系统语音")
        }

        pub(crate) fn pause(&mut self) -> Result<()> {
            Ok(())
        }

        pub(crate) fn resume(&mut self) -> Result<()> {
            Ok(())
        }

        pub(crate) fn finished(&mut self) -> Result<bool> {
            Ok(true)
        }

        pub(crate) fn stop(&mut self) {}
    }
}
