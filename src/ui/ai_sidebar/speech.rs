//! 侧栏的朗读接入：一条消息就是一个朗读会话，跑在共享宿主上（`ui::read_aloud`）。
//!
//! 侧栏是宿主窗口里的一个 view，不是独立窗口；`ReadAloudWindow` 全部基于
//! `Context<Self>` / `cx.entity()`，view 同样能参与进程级的单会话约束——从侧栏
//! 朗读会停掉同一窗口阅读区的朗读，反之亦然（音频输出只有一份）。

use super::markdown::{assistant_read_aloud_paragraphs, user_read_aloud_paragraphs};
use super::*;

impl ReadAloudWindow for AiSidebar {
    fn read_aloud(&mut self) -> &mut ReadAloudHost {
        &mut self.read_aloud
    }

    fn read_aloud_services(&self) -> &Arc<AppServices> {
        &self.services
    }

    /// 侧栏不做逐段高亮：消息粒度的按钮自己会换成停止图标说明正在读哪条。
    fn apply_read_aloud_highlight(&mut self, _block_id: Option<&str>, _cx: &mut Context<Self>) {}

    fn report_read_aloud_error(&mut self, message: String, cx: &mut Context<Self>) {
        // 复用会话面板的错误行：错误存在时面板会渲染，不需要新的提示位。
        self.sessions.error = Some(message);
        cx.notify();
    }
}

impl AiSidebar {
    /// 这条消息是否正在被朗读：当前段 id 以消息 id 为前缀。
    pub(super) fn is_reading_message(&self, message_id: u64) -> bool {
        let prefix = format!("{message_id}-");
        self.read_aloud
            .current_block()
            .is_some_and(|block| block.starts_with(&prefix))
    }

    /// 消息操作条上的朗读按钮：正在读这条就停，否则从这条开始读。
    pub(super) fn toggle_message_read_aloud(
        &mut self,
        message_id: u64,
        role: AiMessageRole,
        content: &str,
        cx: &mut Context<Self>,
    ) {
        if self.is_reading_message(message_id) {
            self.stop_read_aloud(cx);
            return;
        }
        let blocks = message_read_aloud_blocks(message_id, role, content);
        let Some(first) = blocks.first().map(|block| block.id.clone()) else {
            self.sessions.error = Some("这条消息没有可朗读的文字。".to_string());
            cx.notify();
            return;
        };
        self.start_read_aloud_blocks(blocks, &first, cx);
    }
}

/// 一条消息的朗读块：每个段落一块，超长段落按字符边界切开，整条再按与阅读
/// 窗口相同的上限截断（`ReadAloudPageBlock` 的校验会拒掉越界的请求）。
///
/// 用户提问按纯文本读（它在气泡里也是纯文本），AI 回答先去掉 Markdown 记号
/// （见 `markdown::assistant_read_aloud_paragraphs`）。
fn message_read_aloud_blocks(
    message_id: u64,
    role: AiMessageRole,
    content: &str,
) -> Vec<ReadAloudPageBlock> {
    let paragraphs = match role {
        AiMessageRole::Assistant => assistant_read_aloud_paragraphs(content),
        _ => user_read_aloud_paragraphs(content),
    };
    let mut blocks = Vec::new();
    let mut total = 0usize;
    for paragraph in paragraphs {
        for piece in split_oversized_paragraph(&paragraph) {
            if blocks.len() >= MAX_READ_ALOUD_BLOCKS
                || total + piece.len() > MAX_READ_ALOUD_TOTAL_BYTES
            {
                return blocks;
            }
            total += piece.len();
            blocks.push(ReadAloudPageBlock {
                id: format!("{message_id}-{}", blocks.len()),
                text: piece,
            });
        }
    }
    blocks
}

/// 一段文字超过单块上限时按字符边界切开：长回答里没有空行的整段文字可能超过
/// 8 KiB，直接提交会让整条请求被校验拒掉、点了朗读没有反应。
fn split_oversized_paragraph(text: &str) -> Vec<String> {
    if text.len() <= MAX_READ_ALOUD_BLOCK_BYTES {
        return vec![text.to_string()];
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + MAX_READ_ALOUD_BLOCK_BYTES).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        pieces.push(text[start..end].to_string());
        start = end;
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_numbered_and_prefixed_by_message_id() {
        let blocks = message_read_aloud_blocks(7, AiMessageRole::User, "第一段\n\n第二段");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].id, "7-0");
        assert_eq!(blocks[0].text, "第一段");
        assert_eq!(blocks[1].id, "7-1");
        assert_eq!(blocks[1].text, "第二段");
    }

    #[test]
    fn code_only_answers_produce_no_blocks() {
        let blocks =
            message_read_aloud_blocks(3, AiMessageRole::Assistant, "```rust\nfn main() {}\n```\n");
        assert!(blocks.is_empty());
    }

    #[test]
    fn oversized_paragraphs_split_at_character_boundaries() {
        let text = "中文段落".repeat(MAX_READ_ALOUD_BLOCK_BYTES);
        let pieces = split_oversized_paragraph(&text);
        assert!(pieces.len() > 1);
        assert!(
            pieces
                .iter()
                .all(|piece| piece.len() <= MAX_READ_ALOUD_BLOCK_BYTES)
        );
        assert_eq!(
            pieces.concat(),
            text,
            "splitting must not drop or alter text"
        );
    }
}
