//! Shared entry point for GPUI tests.
//!
//! The product runs its SQLite, HTTP and translation work on an independent
//! `IoRuntime` (tokio) and wakes the GPUI foreground tasks when those jobs
//! finish. GPUI's newer test scheduler treats that cross-thread wake-up as
//! non-deterministic activity and fails the test unless parking is allowed.
//! `allow_parking` is the framework's own switch for exactly this mixture
//! (real I/O inside an otherwise deterministic test): it keeps the simulated
//! clock but lets the test block on the real worker instead of panicking.
//!
//! Every test that initializes the component library should do it through
//! this helper so the two stay together.

use gpui_kit::TestAppContext;

/// Initializes GPUI Kit's component layer and lets the test scheduler wait for
/// the application's real asynchronous workers.
pub(crate) fn init_ui_test(cx: &mut TestAppContext) {
    cx.update(|app| {
        app.background_executor().allow_parking();
        gpui_kit::init(app);
    });
}
