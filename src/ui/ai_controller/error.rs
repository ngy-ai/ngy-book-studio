//! Rewording an agent failure for the sidebar.

pub(crate) fn friendly_agent_error(error: &str) -> String {
    if error.contains("connection refused") || error.contains("failed to connect") {
        "无法连接本地 Ollama。请确认 Ollama 已启动，并在 AI 设置中检查端点和模型；本应用不会代为启动服务。".to_string()
    } else if error.contains("cancelled") {
        "本次回答已取消。".to_string()
    } else if error.contains("unknown or unserved citation") {
        "模型引用了一个本次检索并未提供的来源，因此该回答未被采用。请重新提问；小模型更容易出现这种情况，可在 AI 设置中换用更强的对话模型。".to_string()
    } else {
        format!("AI 回答失败：{error}")
    }
}
