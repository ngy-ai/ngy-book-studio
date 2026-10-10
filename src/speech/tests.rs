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

/// 倍率语义的回归测试：0.5x–2.0x（含界面全部预设档位）的实际朗读时长应与
/// 1.0x 的时长成反比。2026-10-10 在 Huihui 与 David 上实测该关系由引擎保证、
/// 与具体语音无关，各档误差 <1%；这里留 15% 容差，吸收机器间语音差异。
#[cfg(target_os = "windows")]
#[test]
fn speech_rate_presets_scale_duration_inversely() {
    let engine = platform_engine();
    let text = "这段文字用来测量语速，长度足够读出稳定的时长。";
    let measure = |rate: f32| {
        let wav = engine
            .synthesize(&SpeechRequest::new(text, "", rate))
            .expect("合成");
        let byte_rate = u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]]);
        assert!(byte_rate > 0, "WAV 头里必须带字节率");
        (wav.len() as f64 - 44.0) / f64::from(byte_rate)
    };
    let base = measure(1.0);
    println!("rate=1.0 seconds≈{base:.3}");
    for rate in [0.5f32, 0.75, 1.25, 1.5, 2.0] {
        let seconds = measure(rate);
        let expected = base / f64::from(rate);
        println!("rate={rate} seconds≈{seconds:.3} expected≈{expected:.3}");
        assert!(
            (seconds - expected).abs() <= expected * 0.15,
            "rate={rate} 的时长 {seconds:.3}s 偏离名义倍率（期望≈{expected:.3}s）"
        );
    }
}

/// 多个线程同时首次使用引擎不能崩：`windows` crate 的工厂激活缓存有并发竞态，
/// 实测能在 `SpeechSynthesizer::AllVoices()` 上打出访问违例（0xC0000005，并行跑
/// 整套测试时随机复现）；引擎用一把进程级锁把所有 WinRT 调用串起来。这个用例是
/// 那条约束的护栏——去掉锁再跑，它（以及并行测试）会随机崩。
#[cfg(target_os = "windows")]
#[test]
fn concurrent_voice_enumeration_never_crashes_the_process() {
    let engine = platform_engine();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let voices = engine.voices().expect("枚举系统语音");
                assert!(!voices.is_empty());
            });
        }
    });
}
