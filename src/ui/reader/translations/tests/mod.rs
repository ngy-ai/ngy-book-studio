//! Tests for translation progress and manual edits.

use super::super::*;

use super::*;
use ngy_book_studio::reader::TocItem;

use super::labels::{toc_label, toc_label_requests};

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

/// 目录条目跟着显示方式走：译文/双语显示译文，原文模式显示书里自己的写法；还没有
/// 译文的条目（任务没跑到那一章）在任何模式下都保持原样，绝不显示空白或猜测。
#[test]
fn the_table_of_contents_follows_the_display_mode() {
    let original = "Linear Algebra Intuition";
    let translated = "线性代数的直观理解";
    for mode in [
        TranslationDisplayMode::Bilingual,
        TranslationDisplayMode::TranslationOnly,
    ] {
        assert_eq!(toc_label(original, Some(translated), mode), translated);
        assert_eq!(toc_label(original, None, mode), original);
    }
    assert_eq!(
        toc_label(
            original,
            Some(translated),
            TranslationDisplayMode::OriginalOnly
        ),
        original,
        "原文模式必须给出书里自己的标签，哪怕译文已经存在"
    );
}

/// 每个目录条目要问的是「它指的那一章」的译文：章节标签靠这个单元发出，服务层才能
/// 在全书范围内优先选这一章自己的那一份。指不到章节的条目（纯分组或陈旧索引）按
/// 没有单元处理，不能把别的章节的译文顶到它身上。
#[test]
fn every_toc_entry_asks_for_its_own_chapter() {
    let toc = vec![
        TocItem {
            label: "Part I — Setup & Tooling".to_string(),
            href: Some("EPUB/text/part1.xhtml".to_string()),
            depth: 0,
            spine_index: Some(1),
        },
        TocItem {
            label: "About This Volume".to_string(),
            href: Some("EPUB/text/ch001.xhtml".to_string()),
            depth: 1,
            spine_index: Some(0),
        },
        TocItem {
            label: "Cover".to_string(),
            href: Some("EPUB/text/cover.xhtml".to_string()),
            depth: 0,
            spine_index: Some(9),
        },
        TocItem {
            label: "未命名分组".to_string(),
            href: None,
            depth: 0,
            spine_index: None,
        },
    ];
    let locators = vec![
        DocumentLocator::unit("book", "unit-1"),
        DocumentLocator::unit("book", "unit-2"),
    ];
    let requests = toc_label_requests(&toc, &locators);
    assert_eq!(requests.len(), toc.len(), "请求与目录条目一一对应");
    assert_eq!(requests[0].text, "Part I — Setup & Tooling");
    assert_eq!(requests[0].content_unit_id.as_deref(), Some("unit-2"));
    assert_eq!(requests[1].content_unit_id.as_deref(), Some("unit-1"));
    assert_eq!(
        requests[2].content_unit_id, None,
        "定位表里没有的章号只说明这一条问不到单元，不是错误"
    );
    assert_eq!(requests[3].content_unit_id, None);
    assert_eq!(requests[3].text, "未命名分组");
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
fn retranslate_ipc_accepts_only_a_bounded_block_reference() {
    let own_uri = "epubreader://book/Text/one.xhtml".parse().unwrap();
    let accepted = reader_ipc_event(
        &own_uri,
        r#"{"type":"retranslate_block","revision":3,"request_id":9,"key":"block-1"}"#,
    );
    let Some(ReaderWebEvent::RetranslateBlock { url, request }) = accepted else {
        panic!("一个合法的重新翻译请求必须被接受");
    };
    assert_eq!(url, "epubreader://book/Text/one.xhtml");
    assert_eq!(request.revision, 3);
    assert_eq!(request.request_id, 9);
    assert_eq!(request.key, "block-1");

    // 多出来的字段一概不进入宿主：请求里根本没有「第几块」这个概念，序号由宿主在
    // 任务自己的文本块列表里查出来，页面伪造一个也不会被采用。
    let with_extra = reader_ipc_event(
        &own_uri,
        r#"{"type":"retranslate_block","revision":3,"request_id":7,"key":"block-1","ordinal":2}"#,
    );
    assert!(matches!(
        with_extra,
        Some(ReaderWebEvent::RetranslateBlock { request, .. })
            if request.key == "block-1" && request.request_id == 7
    ));

    for forged in [
        // 没有块标识：宿主无从知道要重跑哪一块
        r#"{"type":"retranslate_block","revision":3,"request_id":1,"key":""}"#,
        // 请求序号 0 永远不是一个真实的请求
        r#"{"type":"retranslate_block","revision":3,"request_id":0,"key":"block-1"}"#,
    ] {
        assert!(reader_ipc_event(&own_uri, forged).is_none(), "{forged}");
    }
    // 超长标识在同一条流水线上被拒绝，而不是传进服务层。
    let oversized_key = format!(
        r#"{{"type":"retranslate_block","revision":1,"request_id":1,"key":"{}"}}"#,
        "k".repeat(MAX_MANUAL_TRANSLATION_KEY_BYTES + 1)
    );
    assert!(reader_ipc_event(&own_uri, &oversized_key).is_none());
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
