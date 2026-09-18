//! The navigation/wide panes and the drag that resizes them.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReaderPaneLayout {
    pub(crate) navigation_width: f32,
    pub(crate) navigation_collapsed: bool,
    pub(crate) ai_width: f32,
    pub(crate) resizing: Option<ReaderResizablePane>,
}

impl ReaderPaneLayout {
    pub(crate) fn new(ai_width: Pixels) -> Self {
        Self {
            navigation_width: READER_NAVIGATION_DEFAULT_WIDTH,
            navigation_collapsed: false,
            ai_width: f32::from(ai_width).clamp(AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_MAX_WIDTH),
            resizing: None,
        }
    }

    pub(crate) fn navigation_width(&self) -> Pixels {
        px(self.effective_navigation_width())
    }

    pub(crate) fn is_navigation_collapsed(&self) -> bool {
        self.navigation_collapsed
    }

    pub(crate) fn set_navigation_collapsed(&mut self, collapsed: bool) -> bool {
        if self.navigation_collapsed == collapsed {
            return false;
        }
        self.navigation_collapsed = collapsed;
        if collapsed {
            self.resizing = self
                .resizing
                .filter(|pane| *pane != ReaderResizablePane::Navigation);
        }
        true
    }

    pub(crate) fn toggle_navigation_collapsed(&mut self) -> bool {
        self.set_navigation_collapsed(!self.navigation_collapsed)
    }

    /// The width the navigation pane actually occupies. Collapsing keeps the
    /// resized width so expanding again restores what the reader had chosen.
    pub(crate) fn effective_navigation_width(&self) -> f32 {
        if self.navigation_collapsed {
            READER_NAVIGATION_COLLAPSED_WIDTH
        } else {
            self.navigation_width
        }
    }

    pub(crate) fn ai_width(&self) -> Pixels {
        px(self.ai_width)
    }

    pub(crate) fn begin_resize(&mut self, pane: ReaderResizablePane) -> bool {
        if self.resizing == Some(pane) {
            return false;
        }
        if pane == ReaderResizablePane::Navigation && self.navigation_collapsed {
            return false;
        }
        self.resizing = Some(pane);
        true
    }

    pub(crate) fn finish_resize(&mut self) -> bool {
        self.resizing.take().is_some()
    }

    pub(crate) fn is_resizing(&self, pane: ReaderResizablePane) -> bool {
        self.resizing == Some(pane)
    }

    pub(crate) fn constrain(&mut self, viewport_width: Pixels, ai_collapsed: bool) -> bool {
        let viewport_width = f32::from(viewport_width);
        if !viewport_width.is_finite() {
            return false;
        }
        let old = (self.navigation_width, self.ai_width);
        self.navigation_width = self
            .navigation_width
            .clamp(READER_NAVIGATION_MIN_WIDTH, READER_NAVIGATION_MAX_WIDTH);
        self.ai_width = self
            .ai_width
            .clamp(AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_MAX_WIDTH);
        if !ai_collapsed {
            self.ai_width = self.ai_width.min(self.maximum_ai_width(viewport_width));
        }
        if !self.navigation_collapsed {
            self.navigation_width = self
                .navigation_width
                .min(self.maximum_navigation_width(viewport_width, ai_collapsed));
        }
        old != (self.navigation_width, self.ai_width)
    }

    pub(crate) fn resize_from_pointer(
        &mut self,
        pane: ReaderResizablePane,
        pointer_x: Pixels,
        viewport_width: Pixels,
        ai_collapsed: bool,
    ) -> bool {
        if self.resizing != Some(pane)
            || (pane == ReaderResizablePane::Ai && ai_collapsed)
            || (pane == ReaderResizablePane::Navigation && self.navigation_collapsed)
        {
            return false;
        }
        let pointer_x = f32::from(pointer_x);
        let viewport_width = f32::from(viewport_width);
        if !pointer_x.is_finite() || !viewport_width.is_finite() {
            return false;
        }

        let constrained = self.constrain(px(viewport_width), ai_collapsed);
        let old = (self.navigation_width, self.ai_width);
        let half_handle = READER_PANE_RESIZE_HANDLE_WIDTH / 2.;
        match pane {
            ReaderResizablePane::Navigation => {
                self.navigation_width = (pointer_x - half_handle).clamp(
                    READER_NAVIGATION_MIN_WIDTH,
                    self.maximum_navigation_width(viewport_width, ai_collapsed),
                );
            }
            ReaderResizablePane::Ai => {
                self.ai_width = (viewport_width - pointer_x - half_handle)
                    .clamp(AI_SIDEBAR_MIN_WIDTH, self.maximum_ai_width(viewport_width));
            }
        }
        constrained || old != (self.navigation_width, self.ai_width)
    }

    pub(crate) fn maximum_navigation_width(&self, viewport_width: f32, ai_collapsed: bool) -> f32 {
        let ai_width = if ai_collapsed {
            AI_SIDEBAR_COLLAPSED_WIDTH
        } else {
            self.ai_width
        };
        let handle_count = if ai_collapsed { 1. } else { 2. };
        (viewport_width
            - ai_width
            - READER_CONTENT_MIN_WIDTH
            - READER_PANE_RESIZE_HANDLE_WIDTH * handle_count)
            .clamp(READER_NAVIGATION_MIN_WIDTH, READER_NAVIGATION_MAX_WIDTH)
    }

    pub(crate) fn maximum_ai_width(&self, viewport_width: f32) -> f32 {
        (viewport_width
            - self.effective_navigation_width()
            - READER_CONTENT_MIN_WIDTH
            - READER_PANE_RESIZE_HANDLE_WIDTH * 2.)
            .clamp(AI_SIDEBAR_MIN_WIDTH, AI_SIDEBAR_MAX_WIDTH)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReaderPaneResizeDrag(pub(crate) ReaderResizablePane);
