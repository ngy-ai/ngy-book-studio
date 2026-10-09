//! The block detail panel.

use super::*;

/// The debug panel of one block: what it shows, that it never grows past the
/// window, and that a click on the overlay closes it without also activating
/// the block row underneath.
#[gpui_kit::test]
pub(crate) fn block_detail_panel_shows_every_payload_and_stays_inside_the_window(
    cx: &mut TestAppContext,
) {
    let (_directory, view, visual) = open_fixture(cx);
    visual.simulate_resize(size(px(900.), px(640.)));
    redraw(visual);
    visual.update(|window, cx| {
        view.update(cx, |jobs, cx| {
            jobs.tab = DetailTab::Blocks;
            jobs.blocks = Some(block_list(78));
            jobs.blocks_job_id = Some(task_id(0));
            assert!(jobs.blocks_error.is_none());
            jobs.open_block_detail(0, false, window, cx);
        });
    });
    redraw(visual);
    // Loading is driven by the real service, which has no such task here, so the
    // panel must report the failure instead of showing a stale or empty body.
    visual.update(|_, cx| {
        let state = view.read(cx).block_detail.as_ref().unwrap();
        assert_eq!(state.ordinal, 0);
        assert_eq!(state.tab, BlockDetailTab::Overview);
        assert!(state.detail.is_none(), "the synthetic task has no blocks");
    });

    // Feed a real payload so every tab renders its own text.
    let detail = TranslationBlockDetail {
        ordinal: 0,
        unit_id: "unit-0000".into(),
        unit_ordinal: 0,
        unit_title: "第一章".into(),
        unit_revision: 1,
        block_id: "unit-0000::h0".into(),
        source_text: "原文".repeat(200),
        segments: vec!["原文".repeat(100), "第二段".to_string()],
        request: TranslationBlockRequest {
            model: "chat-test".into(),
            target_language: "zh-Hans".into(),
            source_language: Some("en".into()),
            max_tokens: 4_096,
            temperature: Some(0.0),
            body: serde_json::json!({
                "model": "chat-test",
                "messages": [
                    { "role": "system", "content": "系统提示词" },
                    { "role": "user", "content": "用户内容" }
                ]
            })
            .to_string(),
            system_prompt: "系统提示词".into(),
            user_content: "用户内容".into(),
        },
    };
    visual.update(|_, cx| {
        view.update(cx, |jobs, cx| {
            let state = jobs.block_detail.as_mut().unwrap();
            state.loading = false;
            state.error = None;
            state.detail = Some(detail.clone());
            // A failed block, with a replay already answered: this is the one tab
            // whose text does not come from the pinned payload, and it still has
            // to keep the panel inside the window.
            state.failed = true;
            state.probe = BlockProbeState {
                loading: false,
                result: Some(TranslationBlockProbe {
                    ordinal: 0,
                    model: "chat-test".into(),
                    response_text: "{\"translations\":[{\"id\":0,\"text\":\"甲\"}]}".into(),
                    response_bytes: 39,
                    end: "done",
                    finish_reason: Some("stop"),
                    decoded_segments: Some(1),
                    decode_error_kind: None,
                    decode_error_line: None,
                    decode_error_column: None,
                    elapsed_ms: 1200,
                }),
                error: None,
            };
            cx.notify();
        });
    });
    redraw(visual);
    assert!(bounds(visual, "jobs-block-detail-panel").size.height > px(0.));
    let panel = bounds(visual, "jobs-block-detail-panel");
    let root = bounds(visual, "jobs-layout-root");
    assert!(
        panel.top() >= root.top() && panel.bottom() <= root.bottom(),
        "the panel must stay inside the window: panel={panel:?}, root={root:?}"
    );

    for tab in block_detail_tabs(true) {
        visual.update(|_, cx| {
            view.update(cx, |jobs, cx| {
                jobs.block_detail.as_mut().unwrap().tab = tab;
                cx.notify();
            });
        });
        redraw(visual);
        let text = visual.update(|_, cx| {
            let jobs = view.read(cx);
            block_detail_text(jobs.block_detail.as_ref().unwrap(), Some(&detail))
        });
        let text = text.expect("a loaded block always has text");
        assert!(!text.is_empty(), "{tab:?} rendered nothing");
        match tab {
            BlockDetailTab::Body => {
                // The body tab is the raw wire payload, so it has to carry both
                // messages and stay parseable.
                assert!(text.contains("系统提示词") && text.contains("用户内容"));
                assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
            }
            BlockDetailTab::Prompt => assert_eq!(text, detail.request.system_prompt),
            BlockDetailTab::Message => assert_eq!(text, detail.request.user_content),
            BlockDetailTab::Source => assert_eq!(text, detail.source_text),
            BlockDetailTab::Response => {
                assert!(text.contains("甲"), "the replayed answer is shown: {text}");
                assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
                // The replay panel is the body here, not the plain text block.
                assert!(bounds(visual, "jobs-block-probe-text").size.height > px(0.));
            }
            BlockDetailTab::Overview => {
                assert!(text.contains("chat-test"));
                assert!(text.contains("#0"));
                assert!(text.contains("中文（简体）"));
            }
        }
        // Every tab keeps the panel and its scroll body laid out.
        assert!(bounds(visual, "jobs-block-detail-body").size.height > px(0.));
        let panel = bounds(visual, "jobs-block-detail-panel");
        assert!(
            panel.top() >= root.top() && panel.bottom() <= root.bottom(),
            "{tab:?} pushed the panel out of the window: panel={panel:?}, root={root:?}"
        );
    }
    // The underlying list is still rendered, so the panel really is an overlay.
    assert!(bounds(visual, "jobs-layout-detail").size.height > px(0.));

    // Clicking the overlay outside the panel is a dismissal, not a block row
    // activation: the hitbox barrier must swallow the press.
    let overlay_point = point(root.left() + px(4.), root.center().y);
    visual.simulate_click(overlay_point, Modifiers::none());
    redraw(visual);
    visual.update(|_, cx| {
        assert!(
            view.read(cx).block_detail.is_none(),
            "a click outside the panel closes it"
        );
    });
    // Closing must restore the normal layout. `debug_bounds` reports the last
    // rendered frame, so the window is asserted through its regions instead of
    // the (still cached) overlay selector.
    assert_regions_fit(visual, 900., 640.);
}
