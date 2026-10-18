//! Shared fixtures for the Endpoint settings window tests.

mod endpoint_test;

use super::*;

use super::super::tests_support;

pub(super) use super::super::endpoint_draft::{
    apply_detected_models, canonical_endpoint, detection_matches,
};

pub(super) use tests_support::*;

use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

pub(super) fn open_window(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    Entity<EndpointSettingsWindow>,
    &mut VisualTestContext,
) {
    open_settings_view::<EndpointSettingsWindow>(cx, EndpointSettingsWindow::new)
}
