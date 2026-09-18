//! Tests for the AI settings window.

use super::*;

#[test]
pub(crate) fn singleton_window_state_distinguishes_live_stale_and_reentrant_requests() {
    assert_eq!(
        existing_window_disposition(false, false, None::<u8>, false, |_| false),
        ExistingWindowDisposition::OpenNew
    );
    assert_eq!(
        existing_window_disposition(true, false, None::<u8>, false, |_| false),
        ExistingWindowDisposition::SuppressReentrantOpen
    );
    assert_eq!(
        existing_window_disposition(false, false, Some(7_u8), true, |handle| handle == 7),
        ExistingWindowDisposition::Activate(7)
    );
    assert_eq!(
        existing_window_disposition(false, false, Some(7_u8), false, |_| true),
        ExistingWindowDisposition::ClearStale(7)
    );
    assert_eq!(
        existing_window_disposition(false, false, Some(7_u8), true, |_| false),
        ExistingWindowDisposition::ClearStale(7)
    );
    assert_eq!(
        existing_window_disposition(false, true, Some(7_u8), true, |_| true),
        ExistingWindowDisposition::SuppressReentrantOpen
    );
}

#[test]
pub(crate) fn api_key_input_has_explicit_keep_set_and_delete_semantics() {
    assert_eq!(api_key_update_for_input("", false), ApiKeyUpdate::Keep);
    assert_eq!(api_key_update_for_input("   ", false), ApiKeyUpdate::Keep);
    assert_eq!(
        api_key_update_for_input("new-secret", false),
        ApiKeyUpdate::Set("new-secret".to_string())
    );
    assert_eq!(
        api_key_update_for_input("new-secret", true),
        ApiKeyUpdate::Delete
    );
}

#[test]
pub(crate) fn every_section_has_a_distinct_singleton_selector_and_app_id() {
    let mut slugs = std::collections::BTreeSet::new();
    for section in AiSettingsSection::ALL {
        assert!(slugs.insert(section.slug()));
        assert_eq!(
            section.selector("save-settings"),
            format!("ai-{}-save-settings", section.slug())
        );
        assert!(section.app_id().ends_with(section.slug()));
        assert!(!section.label().is_empty());
        assert!(section.window_min_size().width <= section.window_size().width);
        assert!(section.window_min_size().height <= section.window_size().height);
    }
}
