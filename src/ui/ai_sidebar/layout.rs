//! Widths and heights the sidebar lays itself out with.

use super::*;

pub(crate) const AI_SIDEBAR_WIDTH: f32 = 360.;
pub(crate) const AI_SIDEBAR_MIN_WIDTH: f32 = 280.;
pub(crate) const AI_SIDEBAR_MAX_WIDTH: f32 = 640.;
pub(crate) const AI_SIDEBAR_COLLAPSED_WIDTH: f32 = 44.;
pub(crate) const AI_SIDEBAR_NARROW_THRESHOLD: f32 = 1080.;
pub(crate) const AI_REFERENCE_VISIBLE_ROWS: usize = 3;
pub(crate) const AI_REFERENCE_ROW_HEIGHT: f32 = 28.;
pub(crate) const MAX_QUESTION_BYTES: usize = 32 * 1024;
pub(crate) const SELECTION_EXPLANATION_QUESTION: &str = "用汉语详细解释一下";
/// Used when the reader selected text of a reading-time translation: the model
/// must explain what the reader actually read, while the frozen quote it also
/// receives stays the immutable original behind every citation and note.
pub(crate) const SELECTION_TRANSLATION_QUESTION: &str =
    "用汉语详细解释一下下面这段译文（原文见所选引用，请按译文解释）：\n\n";
pub(crate) const SELECTION_TRANSLATION_QUESTION_TRUNCATED: &str = "…";
pub(crate) const NO_KNOWLEDGE_BASE_SOURCE_WARNING_TITLE: &str = "没有可验证的知识库来源";
pub(crate) const NO_KNOWLEDGE_BASE_SOURCE_WARNING_BODY: &str =
    "以下回答由大模型根据自身能力生成，不能视为基于所选图书的回答，请自行核实。";
pub(crate) const AI_ENDPOINT_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const MAX_SELECTED_REFERENCES: usize = 16;
// Used by the response-side adapter API, which is deliberately not wired to a
// concrete backend from this UI module.
#[allow(dead_code)]
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub(crate) fn clamp_ai_sidebar_width(width: Pixels) -> Pixels {
    width.clamp(px(AI_SIDEBAR_MIN_WIDTH), px(AI_SIDEBAR_MAX_WIDTH))
}

pub(crate) fn reference_list_max_height(reference_count: usize) -> Pixels {
    px(AI_REFERENCE_ROW_HEIGHT * reference_count.min(AI_REFERENCE_VISIBLE_ROWS) as f32)
}

pub(crate) fn message_text_width(sidebar_width: Pixels, rem_size: Pixels) -> Pixels {
    // Two horizontal padding pairs (3 rem), the copy-button gap (0.25 rem),
    // the 24 px copy column and 4 px for borders/rounding. TextView 0.5.1's
    // clipped list rows lose short text on Windows when width is only a percent
    // inside a flex item. Resolve it before laying out the Markdown tree.
    (sidebar_width - rems(3.25).to_pixels(rem_size) - px(28.)).max(px(0.))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResponsiveSidebar {
    pub(crate) collapsed: bool,
    pub(crate) narrow: bool,
    pub(crate) manual_override: bool,
}

impl ResponsiveSidebar {
    pub(super) fn new(width: Pixels) -> Self {
        let narrow = width < px(AI_SIDEBAR_NARROW_THRESHOLD);
        Self {
            collapsed: narrow,
            narrow,
            manual_override: false,
        }
    }

    pub(super) fn resize(&mut self, width: Pixels) -> bool {
        let narrow = width < px(AI_SIDEBAR_NARROW_THRESHOLD);
        if narrow == self.narrow {
            return false;
        }
        self.narrow = narrow;
        if self.manual_override || self.collapsed == narrow {
            return false;
        }
        self.collapsed = narrow;
        true
    }

    pub(super) fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
        self.manual_override = true;
    }
}
