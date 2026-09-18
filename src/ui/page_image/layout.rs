//! Zoom bounds and the fixed widths of the page chrome.

/// Zoom steps are thousandths; `ZOOM_FIT` means "fit the window".
pub(crate) const ZOOM_FIT: u32 = 0;
pub(crate) const ZOOM_MIN_MILLI: u32 = 250;
pub(crate) const ZOOM_MAX_MILLI: u32 = 4_000;
pub(crate) const ZOOM_STEP_MILLI: u32 = 250;
/// Ctrl + wheel notches arrive in bursts; the page size is written after the
/// reader stops changing it instead of once per notch.
pub(crate) const ZOOM_SAVE_DEBOUNCE_MS: u64 = 400;
pub(crate) const PAGE_TEXT_PANEL_WIDTH: f32 = 330.;
/// One page of hidden text is shown at a time; the cap only guards the panel
/// against a pathological page, the full text stays in the content unit.
pub(crate) const MAX_PAGE_TEXT_CHARS: usize = 20_000;
