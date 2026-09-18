//! The build and protocol gates that keep a closing window alive until its
//! pending work settles.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReaderWebViewBuildGate {
    pub(crate) building: bool,
    pub(crate) close_requested: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ReaderProtocolGate {
    pub(crate) open: Arc<Mutex<bool>>,
    pub(crate) annotation_navigation_blocked: Arc<AtomicBool>,
}

impl ReaderProtocolGate {
    pub(crate) fn new() -> Self {
        Self {
            open: Arc::new(Mutex::new(true)),
            annotation_navigation_blocked: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.lock().is_ok_and(|open| *open)
    }

    pub(crate) fn close(&self) {
        if let Ok(mut open) = self.open.lock() {
            *open = false;
        }
    }
}

impl ReaderWebViewBuildGate {
    pub(crate) fn new(building: bool) -> Self {
        Self {
            building,
            close_requested: false,
        }
    }

    /// Keep the native parent HWND alive until Wry's asynchronous child build
    /// no longer holds the borrowed raw handle.
    pub(crate) fn request_close(&mut self) -> bool {
        if self.building {
            self.close_requested = true;
            true
        } else {
            false
        }
    }

    /// Marks the build settled and consumes a close request that was vetoed.
    pub(crate) fn finish(&mut self) -> bool {
        self.building = false;
        std::mem::take(&mut self.close_requested)
    }
}
