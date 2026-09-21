//! Tests for the scope, endpoint status and sidebar widths.

use super::*;

#[test]
fn endpoint_status_distinguishes_loopback_and_remote_hosts() {
    for local in [
        "http://127.0.0.1:11434/v1",
        "http://localhost:11434/v1",
        "http://[::1]:11434/v1",
    ] {
        let status = AiEndpointStatus::from_base_url(local);
        assert_eq!(status, AiEndpointStatus::Local);
        assert_eq!(status.label(), "本地");
        assert!(!status.is_warning());
    }

    let remote = AiEndpointStatus::from_base_url("https://models.example.test/v1");
    assert_eq!(
        remote,
        AiEndpointStatus::Remote {
            hostname: "models.example.test".to_string(),
        }
    );
    assert_eq!(remote.label(), "远程 · models.example.test");
    assert!(remote.is_warning());

    let private_network = AiEndpointStatus::from_base_url("http://192.168.1.10:11434/v1");
    assert_eq!(
        private_network,
        AiEndpointStatus::Remote {
            hostname: "192.168.1.10".to_string(),
        }
    );
    assert!(private_network.is_warning());
}

#[test]
fn endpoint_status_reports_remote_embedding_with_local_chat() {
    let mut settings = ProviderSettings::default();
    let mut endpoint = settings.endpoint_for(ModelRole::Chat).unwrap();
    endpoint.id = "remote-embedding".into();
    endpoint.name = "远程向量".into();
    endpoint.base_url = "https://vectors.example.test/v1".into();
    settings.endpoint_routing.embedding_endpoint_id = endpoint.id.clone();
    settings
        .endpoint_routing
        .additional_endpoints
        .push(endpoint);
    let status = AiEndpointStatus::from_settings(&settings);
    assert!(status.is_warning());
    assert!(status.label().contains("Embedding：vectors.example.test"));
    assert!(!status.label().contains("问答："));
    settings.endpoint_routing.vision_endpoint_id = "missing".into();
    assert_eq!(
        AiEndpointStatus::from_settings(&settings),
        AiEndpointStatus::Unavailable
    );
}

#[test]
fn invalid_endpoint_status_is_explicitly_unavailable() {
    let status = AiEndpointStatus::from_base_url("not a URL");
    assert_eq!(status, AiEndpointStatus::Unavailable);
    assert_eq!(status.label(), "端点不可用");
    assert!(status.is_warning());
}

#[test]
fn library_scope_deduplicates_books_and_preserves_host_order() {
    let scope = AiSidebarScope::library(
        "Group",
        vec![
            book("a"),
            book("b"),
            book("a"),
            AiBookOption::new("", "bad"),
        ],
    );
    assert_eq!(scope.book_ids(), vec!["a", "b"]);
    assert_eq!(scope.label(), "Group");
}

#[test]
fn book_scope_keeps_current_book_and_only_adds_host_options() {
    let mut scope = AiSidebarScope::book(book("current"), vec![book("other")]);
    assert_eq!(scope.book_ids(), vec!["current"]);
    assert!(!scope.remove_book("current"));
    assert!(!scope.add_book("not-offered"));
    assert!(scope.add_book("other"));
    assert!(!scope.add_book("other"));
    assert_eq!(scope.book_ids(), vec!["current", "other"]);
    assert!(scope.remove_book("other"));
    assert_eq!(scope.book_ids(), vec!["current"]);
}

#[test]
fn narrow_windows_start_collapsed_until_user_overrides() {
    let narrow = ResponsiveSidebar::new(px(900.));
    assert!(narrow.collapsed);

    let mut wide = ResponsiveSidebar::new(px(1400.));
    assert!(!wide.collapsed);
    assert!(wide.resize(px(900.)));
    assert!(wide.collapsed);
    wide.toggle();
    assert!(!wide.collapsed);
    assert!(!wide.resize(px(1400.)));
    assert!(!wide.resize(px(900.)));
    assert!(!wide.collapsed);
}

#[test]
fn expanded_sidebar_width_is_clamped_to_usable_bounds() {
    assert_eq!(
        clamp_ai_sidebar_width(px(AI_SIDEBAR_MIN_WIDTH - 1.)),
        px(AI_SIDEBAR_MIN_WIDTH)
    );
    assert_eq!(clamp_ai_sidebar_width(px(420.)), px(420.));
    assert_eq!(
        clamp_ai_sidebar_width(px(AI_SIDEBAR_MAX_WIDTH + 1.)),
        px(AI_SIDEBAR_MAX_WIDTH)
    );
}

#[test]
fn reference_list_shows_at_most_three_rows_without_dropping_candidates() {
    assert_eq!(reference_list_max_height(1), px(28.));
    assert_eq!(reference_list_max_height(2), px(56.));
    assert_eq!(reference_list_max_height(3), px(84.));
    assert_eq!(reference_list_max_height(6), px(84.));
}
