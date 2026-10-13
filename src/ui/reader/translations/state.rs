//! Per-window translation state.

use super::*;

pub(crate) struct ReaderTranslations {
    pub(crate) session: String,
    pub(crate) generation: u64,
    /// Highest block count reported by the book's unfinished translation task
    /// at the last poll; `None` while nothing is queued, running, or paused.
    pub(crate) observed_progress: Option<usize>,
    /// Chapter and payload fingerprint the page currently shows, so the poll
    /// never rewrites the DOM with translations that are already displayed.
    pub(crate) pushed: Option<(String, u64)>,
    /// Set while a poll is in flight so a slow query cannot stack ticks.
    pub(crate) polling: bool,
    /// Choice made in this reading window for this book. `None` means the book
    /// still follows the global "system configuration" preference; a stored
    /// choice always wins over it.
    pub(crate) override_mode: Option<TranslationDisplayMode>,
    /// Mode currently applied to the page: the book's choice, or the global
    /// preference while the book has none.
    pub(crate) effective: TranslationDisplayMode,
    /// 目录条目的译文，与 `book.toc` 一一对应；`None` 表示这一条没有译文，
    /// 侧栏照旧显示书里的原文。
    pub(crate) navigation_labels: Vec<Option<String>>,
    /// 目录译文请求的代次：迟到的回答只能被丢弃，不能覆盖更新的那一份。
    pub(crate) navigation_generation: u64,
    /// Set while「立即翻译本章」is in flight (它可能要先把正在跑的整本翻译停下来),
    /// 按钮据此显示「正在排队…」并禁止重复点击。
    pub(crate) chapter_pending: bool,
    /// Keeps the refresh task alive for the window's lifetime.
    pub(crate) _refresh_task: Option<Task<()>>,
}

impl ReaderTranslations {
    pub(crate) fn new(global: TranslationDisplayMode) -> Self {
        Self {
            session: String::new(),
            generation: 0,
            observed_progress: None,
            pushed: None,
            polling: false,
            override_mode: None,
            effective: global,
            navigation_labels: Vec::new(),
            navigation_generation: 0,
            chapter_pending: false,
            _refresh_task: None,
        }
    }

    /// The mode the reading window switch shows and the page renders with.
    pub(crate) fn effective(&self) -> TranslationDisplayMode {
        self.effective
    }

    /// Whether the book carries its own choice instead of following the global
    /// preference. The reading window only offers "跟随全局" while it does.
    pub(crate) fn overrides_global(&self) -> bool {
        self.override_mode.is_some()
    }
}
