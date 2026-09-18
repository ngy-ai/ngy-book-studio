//! Tests for the pane layout, dragging and the build gate.

use super::*;

#[test]
pub(crate) fn reading_window_switch_maps_to_the_three_display_modes() {
    assert_eq!(TRANSLATION_DISPLAY_MODES.len(), 3);
    assert_eq!(
        translation_display_mode(0),
        TranslationDisplayMode::Bilingual
    );
    assert_eq!(
        translation_display_mode(1),
        TranslationDisplayMode::OriginalOnly
    );
    assert_eq!(
        translation_display_mode(2),
        TranslationDisplayMode::TranslationOnly
    );
    // An unknown index keeps the default instead of panicking.
    assert_eq!(
        translation_display_mode(99),
        TranslationDisplayMode::TranslationOnly
    );
    assert_eq!(
        [
            translation_display_mode_name(TranslationDisplayMode::Bilingual),
            translation_display_mode_name(TranslationDisplayMode::OriginalOnly),
            translation_display_mode_name(TranslationDisplayMode::TranslationOnly),
        ],
        ["双语", "原文", "译文"]
    );
}

#[test]
pub(crate) fn reader_pane_drag_preserves_width_at_the_handle_center() {
    let mut layout = ReaderPaneLayout::new(px(360.));
    assert!(layout.begin_resize(ReaderResizablePane::Navigation));
    assert!(!layout.resize_from_pointer(
        ReaderResizablePane::Navigation,
        px(READER_NAVIGATION_DEFAULT_WIDTH + READER_PANE_RESIZE_HANDLE_WIDTH / 2.),
        px(1180.),
        false,
    ));
    assert_eq!(layout.navigation_width(), px(286.));
    assert!(layout.finish_resize());

    assert!(layout.begin_resize(ReaderResizablePane::Ai));
    assert!(!layout.resize_from_pointer(
        ReaderResizablePane::Ai,
        px(1180. - 360. - READER_PANE_RESIZE_HANDLE_WIDTH / 2.),
        px(1180.),
        false,
    ));
    assert_eq!(layout.ai_width(), px(360.));
}

#[test]
pub(crate) fn reader_pane_drag_clamps_each_sidebar_and_keeps_the_center_readable() {
    let mut layout = ReaderPaneLayout::new(px(360.));
    assert!(layout.begin_resize(ReaderResizablePane::Navigation));
    assert!(layout.resize_from_pointer(ReaderResizablePane::Navigation, px(0.), px(1180.), false,));
    assert_eq!(layout.navigation_width(), px(READER_NAVIGATION_MIN_WIDTH));
    assert!(layout.resize_from_pointer(
        ReaderResizablePane::Navigation,
        px(1180.),
        px(1180.),
        false,
    ));
    assert_eq!(layout.navigation_width(), px(READER_NAVIGATION_MAX_WIDTH));
    assert!(layout.finish_resize());

    assert!(layout.begin_resize(ReaderResizablePane::Ai));
    assert!(layout.resize_from_pointer(ReaderResizablePane::Ai, px(1180.), px(1180.), false,));
    assert_eq!(layout.ai_width(), px(AI_SIDEBAR_MIN_WIDTH));
    assert!(layout.resize_from_pointer(ReaderResizablePane::Ai, px(0.), px(1180.), false,));
    assert_eq!(layout.ai_width(), px(368.));
    assert_eq!(
        f32::from(layout.navigation_width())
            + f32::from(layout.ai_width())
            + READER_CONTENT_MIN_WIDTH
            + READER_PANE_RESIZE_HANDLE_WIDTH * 2.,
        1180.
    );
}

#[test]
pub(crate) fn collapsing_navigation_narrows_the_pane_and_frees_room_for_content() {
    let mut layout = ReaderPaneLayout::new(px(360.));
    assert!(layout.constrain(px(900.), false));
    assert_eq!(layout.navigation_width(), px(286.));
    assert_eq!(layout.ai_width(), px(282.));

    assert!(layout.toggle_navigation_collapsed());
    assert!(layout.is_navigation_collapsed());
    assert_eq!(
        layout.navigation_width(),
        px(READER_NAVIGATION_COLLAPSED_WIDTH)
    );
    // The freed width goes first to the reading area, and the AI sidebar may
    // now grow past the limit the expanded navigation used to impose.
    assert!(layout.begin_resize(ReaderResizablePane::Ai));
    assert!(layout.resize_from_pointer(ReaderResizablePane::Ai, px(0.), px(900.), false,));
    assert_eq!(layout.ai_width(), px(524.));
    assert!(layout.finish_resize());

    assert!(layout.toggle_navigation_collapsed());
    assert!(!layout.is_navigation_collapsed());
    assert_eq!(layout.navigation_width(), px(286.));
    assert!(layout.constrain(px(900.), false));
    assert_eq!(layout.ai_width(), px(282.));
}

#[test]
pub(crate) fn collapsed_navigation_rail_is_not_resizable_and_cancels_its_drag() {
    let mut layout = ReaderPaneLayout::new(px(360.));
    assert!(layout.begin_resize(ReaderResizablePane::Navigation));
    assert!(layout.is_resizing(ReaderResizablePane::Navigation));

    assert!(layout.toggle_navigation_collapsed());
    assert!(!layout.is_resizing(ReaderResizablePane::Navigation));
    assert!(!layout.finish_resize());
    assert!(!layout.begin_resize(ReaderResizablePane::Navigation));
    assert!(!layout.resize_from_pointer(
        ReaderResizablePane::Navigation,
        px(600.),
        px(900.),
        false,
    ));

    assert!(layout.set_navigation_collapsed(false));
    assert!(!layout.set_navigation_collapsed(false));
    assert_eq!(
        layout.navigation_width(),
        px(READER_NAVIGATION_DEFAULT_WIDTH)
    );
}

#[test]
pub(crate) fn reader_pane_layout_reconstrains_expanded_widths_after_window_shrinks() {
    let mut layout = ReaderPaneLayout::new(px(AI_SIDEBAR_MAX_WIDTH));
    assert!(layout.constrain(px(900.), false));
    assert_eq!(layout.navigation_width(), px(286.));
    assert_eq!(layout.ai_width(), px(282.));
    assert!(!layout.constrain(px(900.), false));
}

#[test]
pub(crate) fn collapsed_ai_sidebar_leaves_room_to_expand_navigation_until_reopened() {
    let mut layout = ReaderPaneLayout::new(px(360.));
    assert!(layout.begin_resize(ReaderResizablePane::Navigation));
    assert!(layout.resize_from_pointer(ReaderResizablePane::Navigation, px(900.), px(900.), true,));
    assert_eq!(layout.navigation_width(), px(READER_NAVIGATION_MAX_WIDTH));
    assert_eq!(layout.ai_width(), px(360.));
    assert!(layout.finish_resize());
    assert!(!layout.constrain(px(900.), true));
    assert!(layout.begin_resize(ReaderResizablePane::Ai));
    assert!(!layout.resize_from_pointer(ReaderResizablePane::Ai, px(400.), px(900.), true,));
    assert_eq!(layout.ai_width(), px(360.));
    assert!(layout.finish_resize());

    assert!(layout.constrain(px(900.), false));
    assert_eq!(layout.navigation_width(), px(288.));
    assert_eq!(layout.ai_width(), px(AI_SIDEBAR_MIN_WIDTH));
    assert_eq!(
        f32::from(layout.navigation_width())
            + f32::from(layout.ai_width())
            + READER_CONTENT_MIN_WIDTH
            + READER_PANE_RESIZE_HANDLE_WIDTH * 2.,
        900.
    );
}

#[test]
pub(crate) fn reader_pane_layout_ignores_moves_without_matching_drag_state() {
    let mut layout = ReaderPaneLayout::new(px(360.));
    assert!(!layout.resize_from_pointer(
        ReaderResizablePane::Navigation,
        px(400.),
        px(1180.),
        false,
    ));
    assert!(layout.begin_resize(ReaderResizablePane::Ai));
    assert!(!layout.resize_from_pointer(
        ReaderResizablePane::Navigation,
        px(400.),
        px(1180.),
        false,
    ));
    assert_eq!(layout.navigation_width(), px(286.));
    assert!(layout.finish_resize());
    assert!(!layout.finish_resize());
}

#[test]
pub(crate) fn webview_build_gate_keeps_parent_alive_until_build_settles() {
    let mut gate = ReaderWebViewBuildGate::new(true);
    assert!(gate.request_close());
    assert!(gate.request_close());
    assert!(gate.finish());
    assert!(!gate.finish());
    assert!(!gate.request_close());

    let mut idle_gate = ReaderWebViewBuildGate::new(false);
    assert!(!idle_gate.request_close());
    assert!(!idle_gate.finish());
}
