//! 合成引擎的冒烟测试：真的调用系统语音，验证产出是可播放的 WAV。
//!
//! 合成不需要音频设备，所以在没有声卡的机器上也能跑；播放（`AudioPlayer`）
//! 才需要设备，这里的测试不触发它。

use super::*;

#[cfg(target_os = "windows")]
use super::player::AudioPlayer;

/// 播放链路的冒烟测试：真的把一段合成音频放到结束。会出声一次（很短），
/// 但这是唯一能验证「点了有声音」的自动化方式。
#[cfg(target_os = "windows")]
#[test]
fn the_player_plays_a_synthesized_clip_to_the_end() {
    let engine = platform_engine();
    let voices = engine.voices().expect("枚举系统语音");
    let voice = voices
        .iter()
        .find(|voice| voice.language.to_ascii_lowercase().starts_with("zh"))
        .unwrap_or(&voices[0]);
    let wav = engine
        .synthesize(&SpeechRequest::new("测试", voice.id.clone(), 2.0))
        .expect("合成一段短音频");
    let mut player = AudioPlayer::new().expect("创建系统音频播放器");
    player.play_wav(&wav).expect("开始播放");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if player.finished().expect("读取播放状态") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "短音频没有在 10 秒内播放结束"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    player.stop();
}

#[cfg(target_os = "windows")]
#[test]
fn platform_engine_lists_voices_and_synthesizes_a_wav() {
    let engine = platform_engine();
    let voices = engine.voices().expect("枚举系统语音");
    assert!(!voices.is_empty(), "Windows 至少应有一个可用语音");
    for voice in &voices {
        assert!(!voice.id.trim().is_empty(), "语音必须有稳定的 ID");
        assert!(!voice.label.trim().is_empty(), "语音必须有名称");
    }

    // 优先中文语音（本机已装 Huihui / Kangkang / Yaoyao），没有就用第一个。
    let voice = voices
        .iter()
        .find(|voice| voice.language.to_ascii_lowercase().starts_with("zh"))
        .unwrap_or(&voices[0]);
    let wav = engine
        .synthesize(&SpeechRequest::new(
            "朗读功能的合成测试。",
            voice.id.clone(),
            1.0,
        ))
        .expect("合成一段中文");
    assert!(wav.starts_with(b"RIFF"), "合成结果必须是 WAV 容器");
    assert_eq!(&wav[8..12], b"WAVE", "WAV 头部必须声明 WAVE 格式");
    assert!(wav.len() > 44, "WAV 必须包含 44 字节头以外的采样数据");
}

#[cfg(target_os = "windows")]
#[test]
fn an_unknown_voice_id_is_reported_instead_of_silently_falling_back() {
    let engine = platform_engine();
    let error = engine
        .synthesize(&SpeechRequest::new(
            "测试。",
            "voice-that-does-not-exist",
            1.0,
        ))
        .expect_err("不存在的语音必须报错");
    assert!(
        format!("{error:#}").contains("voice-that-does-not-exist"),
        "错误信息要点出是哪个语音，实际：{error:#}"
    );
}

#[test]
fn speech_rate_is_clamped_to_the_supported_range() {
    let request = SpeechRequest::new("文本", "", 99.0);
    assert_eq!(request.rate, SpeechRequest::RATE_MAX);
    let request = SpeechRequest::new("文本", "", 0.0);
    assert_eq!(request.rate, SpeechRequest::RATE_MIN);
    let request = SpeechRequest::new("文本", "", 1.25);
    assert_eq!(request.rate, 1.25);
}
