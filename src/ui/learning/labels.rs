//! Turning stored identifiers into the labels the window shows.

use super::*;

pub(crate) fn scenario_label(chapter: u8, scenario: &str) -> &str {
    chapter_scenarios(chapter)
        .iter()
        .find(|(id, _)| *id == scenario)
        .map_or(scenario, |(_, label)| label)
}

pub(crate) fn implementation_label(chapter: u8, implementation: &str) -> &'static str {
    if implementation == "langgraph" {
        if chapter == 1 {
            "LangGraph"
        } else {
            "库实现"
        }
    } else {
        "Python 手搓"
    }
}

pub(crate) fn run_label(status: &str, passed: bool, imported: bool) -> &'static str {
    if imported {
        "导入记录 · 待核验"
    } else if passed {
        "本次运行检查通过"
    } else {
        match status {
            "cancelled" => "本次运行已取消",
            "incomplete" => "练习代码尚未完成",
            "budget_exhausted" => "运行已到达预算",
            "error" => "本次运行出现错误",
            _ => "本次运行检查未通过",
        }
    }
}

pub(crate) fn stop_label(chapter: u8, reason: &str) -> &str {
    match reason {
        "final_answer" if chapter > 1 => "已提交候选答案",
        "final_answer" => "模型返回最终答案",
        "model_budget" => "模型决策预算耗尽",
        "tool_budget" => "工具执行预算耗尽",
        "model_decisions" => "模型接口调用预算耗尽",
        "tool_executions" => "工具执行预算耗尽",
        "deadline" => "运行期限已到",
        "cancelled" => "已取消",
        "starter_todo" => "练习骨架待补全",
        "exercise_complete" => "本章练习已完成",
        other => other,
    }
}

/// Render arbitrary output as a code block without letting its Markdown form
/// links/images or close the fence. Course Markdown itself is bundled material.
pub(crate) fn fenced_text(text: &str, language: &str) -> String {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{language}\n{text}\n{fence}")
}

pub(crate) fn short_text(text: &str, max_characters: usize) -> String {
    let mut characters = text.chars();
    let prefix: String = characters.by_ref().take(max_characters).collect();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

pub(crate) fn record_time(timestamp: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    relative_record_time(timestamp, now)
}

pub(crate) fn relative_record_time(timestamp: &str, now: u64) -> String {
    let Some(elapsed) = timestamp
        .parse::<u64>()
        .ok()
        .and_then(|time| now.checked_sub(time))
    else {
        return "记录时间待核验".to_string();
    };
    match elapsed {
        0..60 => "刚刚".to_string(),
        60..3600 => format!("{} 分钟前", elapsed / 60),
        3600..86400 => format!("{} 小时前", elapsed / 3600),
        _ => format!("{} 天前", elapsed / 86400),
    }
}
