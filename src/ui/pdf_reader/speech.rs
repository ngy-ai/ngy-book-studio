//! PDF 阅读窗口的朗读：PDF.js 文字层的段落与页面高亮。
//!
//! 与 EPUB 阅读窗口同一模式：viewer 页面按当前已绘制的文字层收集段落并发起
//! 朗读（`read_aloud_start`），宿主合成播放，并把「正在读哪一段」推回页面。
//! 会话状态机、进程级单会话约束与输入校验在共享层 `super::read_aloud` 里。

use super::*;

impl ReadAloudWindow for PdfReaderApp {
    fn read_aloud(&mut self) -> &mut ReadAloudHost {
        &mut self.read_aloud
    }

    fn read_aloud_services(&self) -> &Arc<AppServices> {
        &self.services
    }

    /// 让页面高亮（或清除）当前朗读段。高亮只改文字层 span 上的一个可逆类，
    /// 不动文字、选区与笔记锚点。
    fn apply_read_aloud_highlight(&mut self, block_id: Option<&str>, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        let script = read_aloud_highlight_script(block_id);
        if let Err(error) = webview.read(cx).raw().evaluate_script(&script) {
            // 高亮只是呈现：丢一次更新不该升级成阅读器错误。
            tracing::warn!(%error, "cannot apply the PDF read-aloud highlight");
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

impl PdfReaderApp {
    /// 从 viewer 发来的段落列表开始朗读（或从 `start_block` 指向的那一段开始）。
    pub(crate) fn start_read_aloud(
        &mut self,
        blocks: Vec<ReadAloudPageBlock>,
        start_block: &str,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        self.start_read_aloud_blocks(blocks, start_block, cx);
    }

    /// 工具栏的「朗读」：请 viewer 从视口里第一段可见的文字开始。
    ///
    /// 视口只有页面知道，所以这里只发一条脚本；页面收集文字层段落与文字后走
    /// `read_aloud_start` 消息回来，宿主再启动会话。
    pub(crate) fn start_read_aloud_from_viewport(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(webview) = self.webview.clone() else {
            return;
        };
        if let Err(error) = webview.read(cx).raw().evaluate_script(
            "window.ngyPdfReadAloud && window.ngyPdfReadAloud.startFromViewport();",
        ) {
            tracing::warn!(%error, "cannot start the PDF read-aloud from the viewport");
        }
    }
}

/// 页面高亮脚本。页面侧对象可能还不存在（旧页面或脚本未注入完），调用要判空。
pub(crate) fn read_aloud_highlight_script(block_id: Option<&str>) -> String {
    match block_id {
        Some(id) => format!(
            "window.ngyPdfReadAloud && window.ngyPdfReadAloud.highlight({});",
            serde_json::to_string(id).unwrap_or_else(|_| "\"\"".to_string())
        ),
        None => "window.ngyPdfReadAloud && window.ngyPdfReadAloud.clear();".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pdf_highlight_script_never_injects_raw_page_input() {
        // 引号被 JSON 转义后，整串 id 仍然落在字符串字面量内部，不会提前闭合。
        let script = read_aloud_highlight_script(Some("block\");alert(1);//"));
        assert!(
            script.contains(r#"highlight("block\");alert(1);//");"#),
            "{script}"
        );
        assert!(script.starts_with("window.ngyPdfReadAloud && "));
        assert_eq!(
            read_aloud_highlight_script(None),
            "window.ngyPdfReadAloud && window.ngyPdfReadAloud.clear();"
        );
    }
}
