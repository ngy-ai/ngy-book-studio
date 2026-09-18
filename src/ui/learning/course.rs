//! The teaching document attached to a course.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CourseDocument {
    #[default]
    Overview,
    Answers,
    Rubric,
    References,
    Specification,
    Pilot,
    Tutorial,
    Tools,
    Retrieval,
    Memory,
    Planning,
    Evaluation,
    Approval,
    MultiAgent,
    Protocol,
    Capstone,
}

impl CourseDocument {
    #[cfg(test)]
    pub(super) const ALL: [Self; 16] = [
        Self::Overview,
        Self::Answers,
        Self::Rubric,
        Self::References,
        Self::Specification,
        Self::Pilot,
        Self::Tutorial,
        Self::Tools,
        Self::Retrieval,
        Self::Memory,
        Self::Planning,
        Self::Evaluation,
        Self::Approval,
        Self::MultiAgent,
        Self::Protocol,
        Self::Capstone,
    ];

    pub(super) const CHAPTERS: [Self; 10] = [
        Self::Overview,
        Self::Tools,
        Self::Retrieval,
        Self::Memory,
        Self::Planning,
        Self::Evaluation,
        Self::Approval,
        Self::MultiAgent,
        Self::Protocol,
        Self::Capstone,
    ];

    pub(super) const MATERIALS: [Self; 5] = [
        Self::Answers,
        Self::Rubric,
        Self::References,
        Self::Specification,
        Self::Pilot,
    ];

    pub(super) fn chapter_index(self) -> Option<usize> {
        Self::CHAPTERS.iter().position(|chapter| *chapter == self)
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Overview => "第1章 · 工具调用循环",
            Self::Answers => "参考解答",
            Self::Rubric => "评分量规",
            Self::References => "参考资料",
            Self::Specification => "课程规格",
            Self::Pilot => "试学记录表",
            Self::Tutorial => "AI Agent 教程总览",
            Self::Tools => "第2章 · 工具与契约",
            Self::Retrieval => "第3章 · 检索与证据",
            Self::Memory => "第4章 · 记忆与上下文",
            Self::Planning => "第5章 · 规划与工作流",
            Self::Evaluation => "第6章 · 评测与排错",
            Self::Approval => "第7章 · 审批与恢复",
            Self::MultiAgent => "第8章 · 多 Agent 协作",
            Self::Protocol => "第9章 · MCP 与集成",
            Self::Capstone => "第10章 · 综合项目与研究",
        }
    }

    pub(super) fn path(self) -> &'static str {
        match self {
            Self::Overview => "README.md",
            Self::Answers => "worksheets/reference-answers.md",
            Self::Rubric => "worksheets/rubric.md",
            Self::References => "references.md",
            Self::Specification => "spec.md",
            Self::Pilot => "worksheets/pilot-session.md",
            Self::Tutorial => "../ai-agent-tutorial/README.md",
            Self::Tools => "../ai-agent-tutorial/chapters/02-tools-and-contracts.md",
            Self::Retrieval => "../ai-agent-tutorial/chapters/03-retrieval-and-evidence.md",
            Self::Memory => "../ai-agent-tutorial/chapters/04-memory-and-context.md",
            Self::Planning => "../ai-agent-tutorial/chapters/05-planning-and-workflows.md",
            Self::Evaluation => "../ai-agent-tutorial/chapters/06-evaluation-and-debugging.md",
            Self::Approval => "../ai-agent-tutorial/chapters/07-human-approval-and-recovery.md",
            Self::MultiAgent => "../ai-agent-tutorial/chapters/08-multi-agent-systems.md",
            Self::Protocol => "../ai-agent-tutorial/chapters/09-mcp-and-integration.md",
            Self::Capstone => "../ai-agent-tutorial/chapters/10-capstone-and-research.md",
        }
    }

    pub(super) fn markdown(self) -> &'static str {
        match self {
            Self::Overview => include_str!("../../../courses/agent-foundations/README.md"),
            Self::Answers => {
                include_str!("../../../courses/agent-foundations/worksheets/reference-answers.md")
            }
            Self::Rubric => include_str!("../../../courses/agent-foundations/worksheets/rubric.md"),
            Self::References => include_str!("../../../courses/agent-foundations/references.md"),
            Self::Specification => include_str!("../../../courses/agent-foundations/spec.md"),
            Self::Pilot => {
                include_str!("../../../courses/agent-foundations/worksheets/pilot-session.md")
            }
            Self::Tutorial => include_str!("../../../courses/ai-agent-tutorial/README.md"),
            Self::Tools => {
                include_str!(
                    "../../../courses/ai-agent-tutorial/chapters/02-tools-and-contracts.md"
                )
            }
            Self::Retrieval => include_str!(
                "../../../courses/ai-agent-tutorial/chapters/03-retrieval-and-evidence.md"
            ),
            Self::Memory => {
                include_str!("../../../courses/ai-agent-tutorial/chapters/04-memory-and-context.md")
            }
            Self::Planning => include_str!(
                "../../../courses/ai-agent-tutorial/chapters/05-planning-and-workflows.md"
            ),
            Self::Evaluation => include_str!(
                "../../../courses/ai-agent-tutorial/chapters/06-evaluation-and-debugging.md"
            ),
            Self::Approval => include_str!(
                "../../../courses/ai-agent-tutorial/chapters/07-human-approval-and-recovery.md"
            ),
            Self::MultiAgent => {
                include_str!(
                    "../../../courses/ai-agent-tutorial/chapters/08-multi-agent-systems.md"
                )
            }
            Self::Protocol => {
                include_str!(
                    "../../../courses/ai-agent-tutorial/chapters/09-mcp-and-integration.md"
                )
            }
            Self::Capstone => {
                include_str!(
                    "../../../courses/ai-agent-tutorial/chapters/10-capstone-and-research.md"
                )
            }
        }
    }
}

// TextView opens links through the OS and has no internal-link callback. Keep
// course-relative references as text; the native course navigation opens them.
// Parse links so examples inside code blocks are never rewritten.
pub(crate) fn course_display_markdown(markdown: &str) -> String {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut replacements = Vec::new();
    let mut local_link = None;
    for (event, range) in Parser::new(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::Link { dest_url, .. })
                if !dest_url.starts_with("https://") && !dest_url.starts_with("http://") =>
            {
                local_link = Some((range, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, label)) = &mut local_link {
                    label.push_str(&text);
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(replacement) = local_link.take() {
                    replacements.push(replacement);
                }
            }
            _ => {}
        }
    }
    let mut display = markdown.to_string();
    for (range, label) in replacements.into_iter().rev() {
        display.replace_range(range, &label);
    }
    display
}
