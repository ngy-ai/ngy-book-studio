//! 阅读窗口的朗读：页面脚本通道与窗口适配。
//!
//! 页面负责「读什么」：它把当前显示模式下的段落列表（id + 显示文字）随「开始朗读」
//! 一起发过来，宿主只负责按顺序合成、播放，并把「正在读哪一段」推回页面做高亮。
//! 这么做有两个好处：朗读文字天然跟随译文/原文显示方式，而且宿主不需要重新解析
//! 章节 DOM。
//!
//! 会话状态机、进程级单会话约束与输入校验在 `super::read_aloud` 的共享层里：三个
//! 阅读窗口（EPUB、PDF、页面图片）用同一份实现，这里只接页面脚本这一条呈现通道。

use super::*;

impl ReadAloudWindow for ReaderApp {
    fn read_aloud(&mut self) -> &mut ReadAloudHost {
        &mut self.read_aloud
    }

    fn read_aloud_services(&self) -> &Arc<AppServices> {
        &self.services
    }

    /// 让页面高亮（或清除）当前朗读段。高亮只走页面覆盖层，不动正文节点，
    /// 因此不会进入笔记锚点、选区文本或 AI 引用。
    fn apply_read_aloud_highlight(&mut self, block_id: Option<&str>, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let script = read_aloud_highlight_script(block_id);
        self.run_reader_script(
            script,
            // 高亮只是呈现：丢一次更新不该升级成阅读器错误。
            |_, error, _| tracing::warn!(%error, "cannot apply the read-aloud highlight"),
            cx,
        );
    }

    fn report_read_aloud_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.set_error(message, cx);
    }
}

impl ReaderApp {
    /// 从页面发来的段落列表开始朗读（或从 `start_block` 指向的那一段开始）。
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

    /// 工具栏的「朗读」：请页面把它**当前视口的第一段**作为起点发起朗读。
    ///
    /// 视口只有页面知道，所以这里只发一条脚本；页面收集段落与文字后走
    /// `read_aloud_start` 消息回来，宿主再启动会话。
    pub(crate) fn start_read_aloud_from_viewport(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.run_reader_script(
            "window.ngyReadAloud && window.ngyReadAloud.startFromViewport();".to_string(),
            |_, error, _| tracing::warn!(%error, "cannot start read-aloud from the viewport"),
            cx,
        );
    }

    /// 改朗读语速：工具栏的下拉与「系统配置」的档位是同一份应用级偏好
    /// （`speech.preferences.v1`），所以这里保存的是全局值，对之后的朗读生效。
    ///
    /// 正在播放的这一段保持原来的速度：会话在开始时固定了音色与语速，中途换速度
    /// 要么重读要么沉默，都会打断用户在听的内容。
    pub(crate) fn set_speech_rate(&mut self, rate: f32, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let outcome = services.set_speech_rate(rate).await;
            let _ = view.update(cx, |this, cx| {
                match outcome {
                    // 提交成功后刷新所有已打开阅读窗口的档位显示。广播不能在这个
                    // 更新里直接做：当前窗口正被可变借用，再对它自己 `update` 会
                    // panic；`App::defer` 把它排到这次借用释放之后。
                    Ok(()) => cx.defer(apply_speech_rate),
                    Err(error) => this.set_error(format!("无法保存朗读语速：{error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// 页面高亮脚本。页面侧对象可能还不存在（旧页面或脚本未注入完），调用要判空。
pub(crate) fn read_aloud_highlight_script(block_id: Option<&str>) -> String {
    match block_id {
        Some(id) => format!(
            "window.ngyReadAloud && window.ngyReadAloud.highlight({});",
            serde_json::to_string(id).unwrap_or_else(|_| "\"\"".to_string())
        ),
        None => "window.ngyReadAloud && window.ngyReadAloud.clear();".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlight_script_never_injects_raw_page_input() {
        // 引号被 JSON 转义后，整串 id 仍然落在字符串字面量内部，不会提前闭合。
        let script = read_aloud_highlight_script(Some("block\");alert(1);//"));
        assert!(
            script.contains(r#"highlight("block\");alert(1);//");"#),
            "{script}"
        );
        assert!(script.starts_with("window.ngyReadAloud && "));
        assert_eq!(
            read_aloud_highlight_script(None),
            "window.ngyReadAloud && window.ngyReadAloud.clear();"
        );
    }
}
