//! Tests for translation progress and manual edits.

use super::super::*;

use super::*;

#[test]
fn refresh_follows_task_progress_and_settles_when_idle() {
    let mut observed = None;
    assert!(!translation_refresh_due(&mut observed, None));
    assert_eq!(observed, None);

    assert!(translation_refresh_due(&mut observed, Some(0)));
    assert_eq!(observed, Some(0));
    assert!(!translation_refresh_due(&mut observed, Some(0)));

    assert!(translation_refresh_due(&mut observed, Some(7)));
    assert_eq!(observed, Some(7));

    // The task finished: reload once more for the blocks committed between
    // the last two polls, then stay quiet.
    assert!(translation_refresh_due(&mut observed, None));
    assert_eq!(observed, None);
    assert!(!translation_refresh_due(&mut observed, None));
}

#[test]
fn a_manual_edit_must_come_from_the_chapter_the_host_pushed() {
    // 正常路径：窗口推的是第 3 章、版本 7，页面就带着这两个值回来。
    assert!(manual_edit_belongs_to_current_chapter(
        Some(3),
        3,
        Some(7),
        7
    ));
    // 另一章的 URL：即使版本号凑巧相同也不能写进当前章。
    assert!(!manual_edit_belongs_to_current_chapter(
        Some(2),
        3,
        Some(7),
        7
    ));
    assert!(!manual_edit_belongs_to_current_chapter(
        Some(4),
        3,
        Some(7),
        7
    ));
    // 不属于本书的 URL 解析不出章号。
    assert!(!manual_edit_belongs_to_current_chapter(None, 3, Some(7), 7));
    // 上一版载荷的陈旧请求：页面还没换成当前的译文。
    assert!(!manual_edit_belongs_to_current_chapter(
        Some(3),
        3,
        Some(7),
        6
    ));
    // 没有版本记录时宿主推送 0，页面回传 0 必须被接受，
    // 否则该章的手工译文永远写不进去。
    assert!(manual_edit_belongs_to_current_chapter(Some(3), 3, None, 0));
    assert!(!manual_edit_belongs_to_current_chapter(Some(3), 3, None, 1));
    // 有版本记录时伪造成 0 同样被拒。
    assert!(!manual_edit_belongs_to_current_chapter(
        Some(3),
        3,
        Some(7),
        0
    ));
}

#[test]
fn every_display_mode_reaches_the_page_with_its_own_label() {
    assert_eq!(
        translation_display_mode_label(TranslationDisplayMode::Bilingual),
        "bilingual"
    );
    assert_eq!(
        translation_display_mode_label(TranslationDisplayMode::OriginalOnly),
        "original-only"
    );
    assert_eq!(
        translation_display_mode_label(TranslationDisplayMode::TranslationOnly),
        "translation-only"
    );
    // Switching mode must rewrite the page even when the blocks are equal,
    // so the label is part of the fingerprint.
    let blocks = serde_json::json!([{ "key": "b1", "source": "one", "segments": [] }]);
    assert_ne!(
        translation_fingerprint("unit-1", "translation-only", std::slice::from_ref(&blocks)),
        translation_fingerprint("unit-1", "original-only", std::slice::from_ref(&blocks))
    );
}

#[test]
fn fingerprint_only_moves_with_the_pushed_payload() {
    let blocks = serde_json::json!([{ "key": "b1", "source": "one", "segments": [] }]);
    let translated = serde_json::json!([{
        "key": "b1",
        "source": "one",
        "segments": [{ "source": "one", "translated": "一" }],
    }]);
    let base = translation_fingerprint("unit-1", "bilingual", std::slice::from_ref(&blocks));
    assert_eq!(
        base,
        translation_fingerprint("unit-1", "bilingual", std::slice::from_ref(&blocks))
    );
    assert_ne!(
        base,
        translation_fingerprint("unit-2", "bilingual", std::slice::from_ref(&blocks))
    );
    assert_ne!(
        base,
        translation_fingerprint("unit-1", "translation-only", std::slice::from_ref(&blocks))
    );
    assert_ne!(
        base,
        translation_fingerprint("unit-1", "bilingual", std::slice::from_ref(&translated))
    );
}

#[test]
fn manual_translation_ipc_bounds_the_edit_before_the_host_reads_the_database() {
    let own_uri = "epubreader://book/Text/one.xhtml".parse().unwrap();
    let accepted = reader_ipc_event(
        &own_uri,
        r#"{"type":"manual_translation","action":"update","revision":3,"request_id":9,"key":"block-1","segments":[{"source":"One","translated":"一个"}]}"#,
    );
    let Some(ReaderWebEvent::ManualTranslation { url, request }) = accepted else {
        panic!("a bounded manual translation edit must be accepted");
    };
    assert_eq!(url, "epubreader://book/Text/one.xhtml");
    assert_eq!(request.action, ManualTranslationAction::Update);
    assert_eq!(request.revision, 3);
    assert_eq!(request.request_id, 9);
    assert_eq!(request.key, "block-1");
    assert_eq!(request.segments.len(), 1);
    assert_eq!(request.segments[0].source, "One");
    assert_eq!(request.segments[0].translated, "一个");

    // 恢复只表达“删掉手工译文”，所以它不携带任何文本。
    assert!(matches!(
        reader_ipc_event(
            &own_uri,
            r#"{"type":"manual_translation","action":"restore","revision":3,"request_id":10,"key":"block-1","segments":[]}"#,
        ),
        Some(ReaderWebEvent::ManualTranslation { .. })
    ));

    for forged in [
        // 没有文本块标识，宿主无法定位已存的机器译文行
        r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"","segments":[{"source":"One","translated":"一个"}]}"#,
        // 改译文必须带文本，恢复必须不带
        r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"block-1","segments":[]}"#,
        r#"{"type":"manual_translation","action":"restore","revision":3,"request_id":1,"key":"block-1","segments":[{"source":"One","translated":"一个"}]}"#,
        // 空译文与空原文都不是阅读窗口能产生的编辑
        r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"block-1","segments":[{"source":"One","translated":"   "}]}"#,
        r#"{"type":"manual_translation","action":"update","revision":3,"request_id":1,"key":"block-1","segments":[{"source":"","translated":"一个"}]}"#,
        // 请求序号 0 永远不是一个真实的请求
        r#"{"type":"manual_translation","action":"restore","revision":3,"request_id":0,"key":"block-1","segments":[]}"#,
        // 未定义的动作
        r#"{"type":"manual_translation","action":"delete","revision":3,"request_id":1,"key":"block-1","segments":[]}"#,
    ] {
        assert!(reader_ipc_event(&own_uri, forged).is_none(), "{forged}");
    }

    // 超长译文与超长标识在同一条流水线上被拒绝，而不是写进数据库。
    let oversized_segment = format!(
        r#"{{"type":"manual_translation","action":"update","revision":1,"request_id":1,"key":"block-1","segments":[{{"source":"One","translated":"{}"}}]}}"#,
        "字".repeat(MAX_MANUAL_TRANSLATION_SEGMENT_BYTES)
    );
    assert!(reader_ipc_event(&own_uri, &oversized_segment).is_none());
    let oversized_key = format!(
        r#"{{"type":"manual_translation","action":"restore","revision":1,"request_id":1,"key":"{}","segments":[]}}"#,
        "k".repeat(MAX_MANUAL_TRANSLATION_KEY_BYTES + 1)
    );
    assert!(reader_ipc_event(&own_uri, &oversized_key).is_none());
    let too_many_segments = format!(
        r#"{{"type":"manual_translation","action":"update","revision":1,"request_id":1,"key":"block-1","segments":[{}]}}"#,
        vec![r#"{"source":"One","translated":"一"}"#; MAX_MANUAL_TRANSLATION_SEGMENTS + 1]
            .join(",")
    );
    assert!(reader_ipc_event(&own_uri, &too_many_segments).is_none());
}
