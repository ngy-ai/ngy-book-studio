//! 章内阅读位置：把滚动位置随进度保存，并在重新打开时恢复。
//!
//! 位置与笔记共用同一坐标系——章节 DOM 文本去掉 ECMAScript 空白后的 UTF-16
//! 偏移加一段短引文。宿主只校验范围与引文自洽；引文与当前章节文本的比对发生在
//! 章节页里，和引用定位一样：找不到对应文字就保持章节开头，绝不猜测位置。

use super::*;

/// 让章节页把保存的位置滚回视口。锚点在页面里按当前正文复验。
pub(crate) fn position_restore_script(anchor: &TextAnchor) -> String {
    let payload = serde_json::to_string(anchor).unwrap_or_else(|_| "null".to_string());
    format!("window.ngyPosition && window.ngyPosition.restore({payload});")
}

impl ReaderApp {
    /// 章节页报告滚动停止后的章内位置。只接受当前章节页的上报；它与章节切换
    /// 共用同一个有序进度写入器，窗口关闭时的最终位置取自最后一条上报。
    pub(crate) fn sync_reading_position(
        &mut self,
        url: &str,
        anchor: TextAnchor,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.book.spine_index_for_url(url) != Some(self.current_spine) {
            return;
        }
        if self.current_anchor.as_ref() == Some(&anchor) {
            return;
        }
        let Some(progress) = reading_progress_for_locator(
            self.current_spine,
            &self.progress_locators,
            Some(anchor.clone()),
        ) else {
            return;
        };
        self.current_anchor = Some(anchor);
        self.queue_progress(progress, cx);
    }

    /// 打开书后的首个章节加载完成时恢复保存的章内位置。只消费一次；同章的引用
    /// 导航优先；正文里找不到锚点文字时静默保持章节开头。
    pub(crate) fn run_pending_position_restore(&mut self, url: &str, cx: &mut Context<Self>) {
        let (spine_index, anchor) = match self.pending_position_restore.as_ref() {
            Some((spine_index, anchor)) => (*spine_index, anchor.clone()),
            None => return,
        };
        if self.book.spine_index_for_url(url) != Some(spine_index) {
            return;
        }
        self.pending_position_restore = None;
        if self.pending_citation_navigation.is_some() {
            return;
        }
        let Some(webview) = self.webview.as_ref() else {
            return;
        };
        let script = position_restore_script(&anchor);
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            tracing::warn!(%error, "cannot restore chapter reading position");
        }
    }
}
