//! Bundled chapter assets and stable identities for the desktop course runner.

use anyhow::{Result, ensure};

pub fn validate_chapter(chapter: u8) -> Result<()> {
    ensure!((1..=10).contains(&chapter), "课程章节无效");
    Ok(())
}

pub fn chapter_course_id(chapter: u8) -> &'static str {
    match chapter {
        1 => "agent-foundations.chapter-01",
        2 => "agent-foundations.chapter-02",
        3 => "agent-foundations.chapter-03",
        4 => "agent-foundations.chapter-04",
        5 => "agent-foundations.chapter-05",
        6 => "agent-foundations.chapter-06",
        7 => "agent-foundations.chapter-07",
        8 => "agent-foundations.chapter-08",
        9 => "agent-foundations.chapter-09",
        10 => "agent-foundations.chapter-10",
        _ => "",
    }
}

pub fn chapter_title(chapter: u8) -> &'static str {
    match chapter {
        1 => "第1章 · 工具调用循环",
        2 => "第2章 · 工具与契约",
        3 => "第3章 · 检索与证据",
        4 => "第4章 · 记忆与上下文",
        5 => "第5章 · 规划与工作流",
        6 => "第6章 · 评测与排错",
        7 => "第7章 · 审批与恢复",
        8 => "第8章 · 多 Agent 协作",
        9 => "第9章 · MCP 与集成",
        10 => "第10章 · 综合项目与研究",
        _ => "未知章节",
    }
}

pub fn chapter_scenarios(chapter: u8) -> &'static [(&'static str, &'static str)] {
    match chapter {
        1 => &[
            ("normal", "正常资料研究"),
            ("invalid_call", "非法工具名称"),
            ("invalid_arguments", "非法工具参数"),
            ("transient", "暂时失败与重试"),
            ("missing", "正文永久缺失"),
            ("transfer", "公开迁移练习"),
            ("model_budget", "模型决策预算"),
            ("tool_budget", "实际工具预算"),
        ],
        2..=10 => &[
            ("normal", "正常任务"),
            ("fault", "故障练习"),
            ("transfer", "迁移挑战"),
        ],
        _ => &[],
    }
}

pub fn chapter_markdown(chapter: u8) -> &'static str {
    match chapter {
        1 => include_str!("../courses/agent-foundations/README.md"),
        2 => include_str!("../courses/ai-agent-tutorial/exercises/02-tools-and-contracts.md"),
        3 => include_str!("../courses/ai-agent-tutorial/exercises/03-retrieval-and-evidence.md"),
        4 => include_str!("../courses/ai-agent-tutorial/exercises/04-memory-and-context.md"),
        5 => include_str!("../courses/ai-agent-tutorial/exercises/05-planning-and-workflows.md"),
        6 => include_str!("../courses/ai-agent-tutorial/exercises/06-evaluation-and-debugging.md"),
        7 => {
            include_str!("../courses/ai-agent-tutorial/exercises/07-human-approval-and-recovery.md")
        }
        8 => include_str!("../courses/ai-agent-tutorial/exercises/08-multi-agent-systems.md"),
        9 => include_str!("../courses/ai-agent-tutorial/exercises/09-mcp-and-integration.md"),
        10 => include_str!("../courses/ai-agent-tutorial/exercises/10-capstone-and-research.md"),
        _ => "",
    }
}

pub fn starter_code_for(chapter: u8, implementation: &str) -> &'static str {
    match (chapter, implementation == "langgraph") {
        (1, false) => include_str!("../courses/agent-foundations/starters/manual.py"),
        (1, true) => include_str!("../courses/agent-foundations/starters/langgraph_agent.py"),
        (2, false) => include_str!("../courses/agent-foundations/starters/ch02_manual.py"),
        (2, true) => include_str!("../courses/agent-foundations/starters/ch02_langgraph.py"),
        (3, false) => include_str!("../courses/agent-foundations/starters/ch03_manual.py"),
        (3, true) => include_str!("../courses/agent-foundations/starters/ch03_langgraph.py"),
        (4, false) => include_str!("../courses/agent-foundations/starters/ch04_manual.py"),
        (4, true) => include_str!("../courses/agent-foundations/starters/ch04_langgraph.py"),
        (5, false) => include_str!("../courses/agent-foundations/starters/ch05_manual.py"),
        (5, true) => include_str!("../courses/agent-foundations/starters/ch05_langgraph.py"),
        (6, false) => include_str!("../courses/agent-foundations/starters/ch06_manual.py"),
        (6, true) => include_str!("../courses/agent-foundations/starters/ch06_langgraph.py"),
        (7, false) => include_str!("../courses/agent-foundations/starters/ch07_manual.py"),
        (7, true) => include_str!("../courses/agent-foundations/starters/ch07_langgraph.py"),
        (8, false) => include_str!("../courses/agent-foundations/starters/ch08_manual.py"),
        (8, true) => include_str!("../courses/agent-foundations/starters/ch08_langgraph.py"),
        (9, false) => include_str!("../courses/agent-foundations/starters/ch09_manual.py"),
        (9, true) => include_str!("../courses/agent-foundations/starters/ch09_langgraph.py"),
        (10, false) => include_str!("../courses/agent-foundations/starters/ch10_manual.py"),
        (10, true) => include_str!("../courses/agent-foundations/starters/ch10_langgraph.py"),
        _ => "",
    }
}

pub fn reference_code_for(chapter: u8, implementation: &str) -> &'static str {
    match (chapter, implementation == "langgraph") {
        (1, false) => {
            include_str!("../courses/agent-foundations/ngy_lab/implementations/manual.py")
        }
        (1, true) => {
            include_str!("../courses/agent-foundations/ngy_lab/implementations/langgraph_agent.py")
        }
        (2, false) => include_str!("../courses/agent-foundations/references/ch02_manual.py"),
        (2, true) => include_str!("../courses/agent-foundations/references/ch02_langgraph.py"),
        (3, false) => include_str!("../courses/agent-foundations/references/ch03_manual.py"),
        (3, true) => include_str!("../courses/agent-foundations/references/ch03_langgraph.py"),
        (4, false) => include_str!("../courses/agent-foundations/references/ch04_manual.py"),
        (4, true) => include_str!("../courses/agent-foundations/references/ch04_langgraph.py"),
        (5, false) => include_str!("../courses/agent-foundations/references/ch05_manual.py"),
        (5, true) => include_str!("../courses/agent-foundations/references/ch05_langgraph.py"),
        (6, false) => include_str!("../courses/agent-foundations/references/ch06_manual.py"),
        (6, true) => include_str!("../courses/agent-foundations/references/ch06_langgraph.py"),
        (7, false) => include_str!("../courses/agent-foundations/references/ch07_manual.py"),
        (7, true) => include_str!("../courses/agent-foundations/references/ch07_langgraph.py"),
        (8, false) => include_str!("../courses/agent-foundations/references/ch08_manual.py"),
        (8, true) => include_str!("../courses/agent-foundations/references/ch08_langgraph.py"),
        (9, false) => include_str!("../courses/agent-foundations/references/ch09_manual.py"),
        (9, true) => include_str!("../courses/agent-foundations/references/ch09_langgraph.py"),
        (10, false) => include_str!("../courses/agent-foundations/references/ch10_manual.py"),
        (10, true) => include_str!("../courses/agent-foundations/references/ch10_langgraph.py"),
        _ => "",
    }
}
