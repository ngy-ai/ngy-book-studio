//! 页面图片窗口的朗读：本页文字层段落与面板高亮。
//!
//! 与阅读窗口不同，这里的段落由宿主切：文字来自 DjVu 内嵌的隐藏文字层，
//! 窗口本来就把整页文字渲染在右侧「本页文字」面板里，宿主手上就有全文。
//! 段落索引就是朗读块 id，高亮按同一个索引在面板里画一个 `<mark>`。
//!
//! 只对 DjVu 开放：Office 逐页预览是图片页，没有文字层；Office 图书的
//! 结构化阅读在阅读窗口里，那里已经支持朗读。

use super::*;

impl ReadAloudWindow for PageImageApp {
    fn read_aloud(&mut self) -> &mut ReadAloudHost {
        &mut self.read_aloud
    }

    fn read_aloud_services(&self) -> &Arc<AppServices> {
        &self.services
    }

    /// 高亮当前朗读段：「本页文字」面板的 HTML 按这个索引重画。
    fn apply_read_aloud_highlight(&mut self, block_id: Option<&str>, cx: &mut Context<Self>) {
        let paragraph = block_id.and_then(|id| id.parse::<usize>().ok());
        if self.read_aloud_paragraph != paragraph {
            self.read_aloud_paragraph = paragraph;
            cx.notify();
        }
    }

    fn report_read_aloud_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: message,
            error: true,
        });
        cx.notify();
    }
}

impl PageImageApp {
    /// 当前页可供朗读的段落，与「本页文字」面板显示的是同一份切分。
    pub(crate) fn read_aloud_paragraphs(&self) -> Vec<String> {
        let text = self
            .page_texts
            .get(self.current)
            .and_then(|text| text.as_deref())
            .unwrap_or_default();
        page_text_paragraphs(&page_text_preview(text))
    }

    /// 工具栏的「朗读」：从本页文字的第一段开始读到底。翻页与关闭窗口都会停止。
    pub(crate) fn start_read_aloud(&mut self, cx: &mut Context<Self>) {
        if self.closing || self.read_aloud.is_active() {
            return;
        }
        let paragraphs = self.read_aloud_paragraphs();
        if paragraphs.is_empty() {
            self.notice = Some(Notice {
                text: "这一页没有文字层，无法朗读".to_string(),
                error: true,
            });
            cx.notify();
            return;
        }
        // 高亮画在「本页文字」面板上：开始朗读时把它打开。
        self.text_panel_open = true;
        let blocks = paragraphs
            .into_iter()
            .enumerate()
            .map(|(index, text)| ReadAloudPageBlock {
                id: index.to_string(),
                text,
            })
            .collect::<Vec<_>>();
        self.start_read_aloud_blocks(blocks, "0", cx);
    }
}
