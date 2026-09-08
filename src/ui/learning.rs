use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

use gpui_component::text::TextView;
use moye_epub_editor::learning::{
    LearningEnvironment, LearningLesson, LearningRunReport, LearningRunSummary, LearningService,
    LearningSnapshot, LearningTrace, LearningWorkspace,
};
use moye_epub_editor::learning_records::{chapter_scenarios, chapter_title, reference_code_for};

const MAX_VISIBLE_TRACES: usize = 1_000;
const CODE_LANGUAGE: &str = "python";

fn register_python_highlighting() {
    use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        // The component's default language bundle only contains JSON. Register
        // Python explicitly before editors or Markdown code blocks create a parser.
        LanguageRegistry::singleton().register(
            CODE_LANGUAGE,
            &LanguageConfig::new(
                CODE_LANGUAGE,
                tree_sitter_python::LANGUAGE.into(),
                vec![],
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
    });
}

// TextView 0.5.1 stores selection endpoints relative to its own bounds. Its
// virtual list scrolls text inside fixed bounds, so repainting selects different
// characters after a wheel event. Scroll the whole, naturally sized TextView
// instead: the text and selection coordinates then move together.
#[inline(never)]
fn scrollable_learning_text(
    id: impl Into<SharedString>,
    markdown: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let id = id.into();
    let scroll_handle = window
        .use_keyed_state(SharedString::from(format!("{id}/scroll")), cx, |_, _| {
            gpui::ScrollHandle::default()
        })
        .read(cx)
        .clone();
    div()
        .id(SharedString::from(format!("{id}/viewport")))
        .size_full()
        .relative()
        .child(
            div()
                .id("text-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .child(
                    TextView::markdown(id, markdown, window, cx)
                        .selectable(true)
                        .scrollable(false)
                        .w_full()
                        .h_auto()
                        .flex_shrink_0(),
                ),
        )
        .vertical_scrollbar(&scroll_handle)
        .into_any_element()
}

const HELP_LEVELS: [(&str, &str); 5] = [
    ("H0", "H0 · 独立尝试"),
    ("H1", "H1 · 定位提示"),
    ("H2", "H2 · 结构提示"),
    ("H3", "H3 · 局部示范"),
    ("S", "S · 完整示范 / 代写"),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CourseDocument {
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
    const ALL: [Self; 16] = [
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

    const CHAPTERS: [Self; 10] = [
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

    const MATERIALS: [Self; 5] = [
        Self::Answers,
        Self::Rubric,
        Self::References,
        Self::Specification,
        Self::Pilot,
    ];

    fn chapter_index(self) -> Option<usize> {
        Self::CHAPTERS.iter().position(|chapter| *chapter == self)
    }

    fn title(self) -> &'static str {
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

    fn path(self) -> &'static str {
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

    fn markdown(self) -> &'static str {
        match self {
            Self::Overview => include_str!("../../courses/agent-foundations/README.md"),
            Self::Answers => {
                include_str!("../../courses/agent-foundations/worksheets/reference-answers.md")
            }
            Self::Rubric => include_str!("../../courses/agent-foundations/worksheets/rubric.md"),
            Self::References => include_str!("../../courses/agent-foundations/references.md"),
            Self::Specification => include_str!("../../courses/agent-foundations/spec.md"),
            Self::Pilot => {
                include_str!("../../courses/agent-foundations/worksheets/pilot-session.md")
            }
            Self::Tutorial => include_str!("../../courses/ai-agent-tutorial/README.md"),
            Self::Tools => {
                include_str!("../../courses/ai-agent-tutorial/chapters/02-tools-and-contracts.md")
            }
            Self::Retrieval => include_str!(
                "../../courses/ai-agent-tutorial/chapters/03-retrieval-and-evidence.md"
            ),
            Self::Memory => {
                include_str!("../../courses/ai-agent-tutorial/chapters/04-memory-and-context.md")
            }
            Self::Planning => include_str!(
                "../../courses/ai-agent-tutorial/chapters/05-planning-and-workflows.md"
            ),
            Self::Evaluation => include_str!(
                "../../courses/ai-agent-tutorial/chapters/06-evaluation-and-debugging.md"
            ),
            Self::Approval => include_str!(
                "../../courses/ai-agent-tutorial/chapters/07-human-approval-and-recovery.md"
            ),
            Self::MultiAgent => {
                include_str!("../../courses/ai-agent-tutorial/chapters/08-multi-agent-systems.md")
            }
            Self::Protocol => {
                include_str!("../../courses/ai-agent-tutorial/chapters/09-mcp-and-integration.md")
            }
            Self::Capstone => {
                include_str!("../../courses/ai-agent-tutorial/chapters/10-capstone-and-research.md")
            }
        }
    }
}

// TextView opens links through the OS and has no internal-link callback. Keep
// course-relative references as text; the native course navigation opens them.
// Parse links so examples inside code blocks are never rewritten.
fn course_display_markdown(markdown: &str) -> String {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StudyPane {
    Document(CourseDocument),
    Lesson,
    Code,
    Notes,
}

impl Default for StudyPane {
    fn default() -> Self {
        Self::Document(CourseDocument::Tutorial)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EvidencePane {
    Result,
    Trace,
    Attempt,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Loading,
    Saving,
    Exporting,
    Restoring,
    NewRound,
    LoadingReport,
    SwitchingChapter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAction {
    Wait,
    Save,
    Remove,
}

fn close_action(running: bool, pending: bool, dirty: bool) -> CloseAction {
    if running || pending {
        CloseAction::Wait
    } else if dirty {
        CloseAction::Save
    } else {
        CloseAction::Remove
    }
}

#[derive(Default)]
struct LearningWindowTracker {
    opening: bool,
    closing: bool,
    handle: Option<gpui::AnyWindowHandle>,
}

impl gpui::Global for LearningWindowTracker {}

struct LearningNotice {
    text: String,
    error: bool,
}

/// Native controls only: no child HWND/WebView is introduced by this window.
/// All persistence and execution cross LearningService's asynchronous boundary.
pub(super) struct LearningWindow {
    service: Arc<LearningService>,
    lessons: Vec<LearningLesson>,
    saved_workspace: Option<LearningWorkspace>,
    reload_required: bool,
    environment: Option<LearningEnvironment>,
    history: Vec<LearningRunSummary>,
    manual_input: Entity<InputState>,
    langgraph_input: Entity<InputState>,
    notes_input: Entity<InputState>,
    prediction_input: Entity<InputState>,
    selected_lesson: usize,
    implementation: String,
    scenario: String,
    help_level: String,
    study_pane: StudyPane,
    evidence_pane: EvidencePane,
    showing_reference: bool,
    operation: Option<Operation>,
    running: bool,
    run_generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    report: Option<LearningRunReport>,
    trace: Vec<LearningTrace>,
    trace_selected: Option<usize>,
    omitted_traces: usize,
    closing: bool,
    removing: bool,
    notice: Option<LearningNotice>,
    _subscriptions: Vec<Subscription>,
}

impl LearningWindow {
    fn create_inputs(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ([Entity<InputState>; 4], Vec<Subscription>) {
        let manual_input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(CODE_LANGUAGE)
                .line_number(true)
                .placeholder("正在读取你的手搓实现…")
        });
        let langgraph_input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(CODE_LANGUAGE)
                .line_number(true)
                .placeholder("正在读取你的 LangGraph 实现…")
        });
        let notes_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(18)
                .placeholder("记录首次作答、卡点、修改理由和复测结果；保留原判断。")
        });
        let prediction_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(4)
                .placeholder("运行前：你预计会调用什么、在哪里失败、怎样停止？")
        });
        let subscriptions = [
            &manual_input,
            &langgraph_input,
            &notes_input,
            &prediction_input,
        ]
        .into_iter()
        .map(|input| cx.subscribe_in(input, window, Self::on_input_event))
        .collect();
        (
            [manual_input, langgraph_input, notes_input, prediction_input],
            subscriptions,
        )
    }

    fn replace_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // set_value deliberately keeps InputState's undo history. A different
        // chapter, restored backup or new round must not inherit edits whose
        // ranges and old text belonged to the previous workspace.
        let ([manual, langgraph, notes, prediction], subscriptions) =
            Self::create_inputs(window, cx);
        self.manual_input = manual;
        self.langgraph_input = langgraph;
        self.notes_input = notes;
        self.prediction_input = prediction;
        self._subscriptions = subscriptions;
    }

    fn new(service: Arc<LearningService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        register_python_highlighting();
        let ([manual_input, langgraph_input, notes_input, prediction_input], subscriptions) =
            Self::create_inputs(window, cx);
        Self {
            service,
            lessons: Vec::new(),
            saved_workspace: None,
            reload_required: false,
            environment: None,
            history: Vec::new(),
            manual_input,
            langgraph_input,
            notes_input,
            prediction_input,
            selected_lesson: 0,
            implementation: "manual".to_string(),
            scenario: "normal".to_string(),
            help_level: "H0".to_string(),
            study_pane: StudyPane::default(),
            evidence_pane: EvidencePane::Result,
            showing_reference: false,
            operation: None,
            running: false,
            run_generation: 0,
            cancellation: None,
            report: None,
            trace: Vec::new(),
            trace_selected: None,
            omitted_traces: 0,
            closing: false,
            removing: false,
            notice: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    }

    fn busy(&self) -> bool {
        self.operation.is_some() || self.running || self.closing
    }

    fn inputs_disabled(&self) -> bool {
        self.busy() || self.reload_required || self.saved_workspace.is_none()
    }

    fn chapter_entry_disabled(&self) -> bool {
        // A chapter that never loaded has no editable work to lose. Permit
        // leaving it or opening its restore controls, while a loaded workspace
        // with an uncertain disk revision must be reconciled first.
        self.busy() || (self.reload_required && self.saved_workspace.is_some())
    }

    fn entered_workspace(&self, cx: &App) -> Option<LearningWorkspace> {
        let mut workspace = self.saved_workspace.clone()?;
        workspace.manual_code = self.manual_input.read(cx).value().to_string();
        workspace.langgraph_code = self.langgraph_input.read(cx).value().to_string();
        workspace.notes = self.notes_input.read(cx).value().to_string();
        workspace.prediction = self.prediction_input.read(cx).value().to_string();
        workspace.help_level = self.help_level.clone();
        workspace.lesson_index = self.selected_lesson;
        workspace.implementation = self.implementation.clone();
        workspace.scenario = self.scenario.clone();
        Some(workspace)
    }

    fn dirty(&self, cx: &App) -> bool {
        let Some(saved) = &self.saved_workspace else {
            return false;
        };
        self.manual_input.read(cx).value().as_ref() != saved.manual_code
            || self.langgraph_input.read(cx).value().as_ref() != saved.langgraph_code
            || self.notes_input.read(cx).value().as_ref() != saved.notes
            || self.prediction_input.read(cx).value().as_ref() != saved.prediction
            || self.help_level != saved.help_level
            || self.selected_lesson != saved.lesson_index
            || self.implementation != saved.implementation
            || self.scenario != saved.scenario
    }

    fn apply_snapshot(
        &mut self,
        snapshot: LearningSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = snapshot.workspace;
        self.selected_lesson = workspace
            .lesson_index
            .min(snapshot.lessons.len().saturating_sub(1));
        self.implementation = workspace.implementation.clone();
        self.scenario = workspace.scenario.clone();
        self.help_level = workspace.help_level.clone();
        self.manual_input.update(cx, |input, cx| {
            input.set_value(workspace.manual_code.clone(), window, cx);
        });
        self.langgraph_input.update(cx, |input, cx| {
            input.set_value(workspace.langgraph_code.clone(), window, cx);
        });
        self.notes_input.update(cx, |input, cx| {
            input.set_value(workspace.notes.clone(), window, cx);
        });
        self.prediction_input.update(cx, |input, cx| {
            input.set_value(workspace.prediction.clone(), window, cx);
        });
        self.saved_workspace = Some(workspace);
        self.reload_required = false;
        self.lessons = snapshot.lessons;
        self.environment = Some(snapshot.environment);
        self.history = snapshot.history;
    }

    fn clear_unloaded_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_inputs(window, cx);
        self.saved_workspace = None;
        self.reload_required = false;
        self.lessons.clear();
        self.environment = None;
        self.history.clear();
        self.selected_lesson = 0;
        self.implementation = "manual".into();
        self.scenario = "normal".into();
        self.help_level = "H0".into();
        self.report = None;
        self.trace.clear();
        self.trace_selected = None;
        self.omitted_traces = 0;
        self.showing_reference = false;
        self.evidence_pane = EvidencePane::Result;
        self.study_pane = StudyPane::Lesson;
    }

    fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(LearningNotice { text, error: true });
        if self.closing {
            self.closing = false;
            cx.global_mut::<LearningWindowTracker>().closing = false;
        }
        cx.notify();
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.operation.is_some() || (self.closing && !self.reload_required) {
            return;
        }
        let pending_save = (!self.reload_required && self.dirty(cx))
            .then(|| self.entered_workspace(cx))
            .flatten();
        let preserved_revision = self
            .reload_required
            .then(|| {
                self.saved_workspace
                    .as_ref()
                    .map(|workspace| workspace.revision)
            })
            .flatten();
        self.operation = Some(Operation::Loading);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let (saved, result) = match pending_save {
                Some(workspace) => match service.save(workspace).await {
                    Ok(saved) => (Some(saved), service.snapshot().await),
                    Err(error) => (None, Err(error)),
                },
                None => (None, service.snapshot().await),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                if let Some(saved) = saved {
                    this.saved_workspace = Some(saved);
                }
                match result {
                    Ok(snapshot) => {
                        if preserved_revision == Some(snapshot.workspace.revision) {
                            // A failed operation may not have committed. Keep the
                            // unsaved editor contents when the disk did not advance.
                            this.saved_workspace = Some(snapshot.workspace);
                            this.lessons = snapshot.lessons;
                            this.history = snapshot.history;
                            this.environment = Some(snapshot.environment);
                            this.reload_required = false;
                        } else {
                            if this.saved_workspace.is_some()
                                && this.entered_workspace(cx).as_ref() != Some(&snapshot.workspace)
                            {
                                this.replace_inputs(window, cx);
                            }
                            this.apply_snapshot(snapshot, window, cx);
                        }
                        this.notice = None;
                        this.advance_close(window, cx);
                    }
                    Err(error) => this.set_error(format!("无法载入学习工作区：{error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn enter_chapter(&mut self, chapter: u8, window: &mut Window, cx: &mut Context<Self>) {
        if self.chapter_entry_disabled() {
            return;
        }
        if chapter == self.service.chapter() {
            self.study_pane = StudyPane::Lesson;
            self.showing_reference = false;
            if self.saved_workspace.is_none() {
                self.load(window, cx);
            }
            cx.notify();
            return;
        }
        let next = match self.service.for_chapter(chapter) {
            Ok(service) => Arc::new(service),
            Err(error) => {
                self.set_error(format!("无法打开本章实验：{error:#}"), cx);
                return;
            }
        };
        let current = Arc::clone(&self.service);
        let pending = self.dirty(cx).then(|| self.entered_workspace(cx)).flatten();
        self.operation = Some(Operation::SwitchingChapter);
        cx.spawn_in(window, async move |view, cx| {
            // Save against the old immutable service before leaving its inputs.
            // A target load failure still opens that chapter's recovery state,
            // so its own backup can be restored from the visible controls.
            let result = match pending {
                Some(workspace) => match current.save(workspace).await {
                    Ok(_) => Ok(next.snapshot().await),
                    Err(error) => Err(error),
                },
                None => Ok(next.snapshot().await),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match result {
                    Ok(Ok(snapshot)) => {
                        this.service = next;
                        this.replace_inputs(window, cx);
                        this.apply_snapshot(snapshot, window, cx);
                        this.report = None;
                        this.trace.clear();
                        this.trace_selected = None;
                        this.omitted_traces = 0;
                        this.showing_reference = false;
                        this.evidence_pane = EvidencePane::Result;
                        this.study_pane = StudyPane::Lesson;
                        this.notice = None;
                        this.advance_close(window, cx);
                    }
                    Ok(Err(error)) => {
                        this.service = next;
                        this.clear_unloaded_chapter(window, cx);
                        this.set_error(
                            format!(
                                "第{chapter}章记录未能载入。此前章节已保存；可恢复本章备份、重新检测，或从左侧进入其它章节：{error:#}"
                            ),
                            cx,
                        );
                    }
                    Err(error) => {
                        this.set_error(
                            format!("当前章节保存失败，切换未完成，输入已保留：{error:#}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation.is_some() || self.running || self.reload_required {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        self.operation = Some(Operation::Saving);
        self.notice = Some(LearningNotice {
            text: "正在保存代码、预测和学习记录…".to_string(),
            error: false,
        });
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = service.save(workspace).await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match outcome {
                    Ok(saved) => {
                        this.saved_workspace = Some(saved);
                        this.notice = Some(LearningNotice {
                            text: "代码、预测和学习记录已保存。".to_string(),
                            error: false,
                        });
                        this.advance_close(window, cx);
                    }
                    Err(error) => {
                        this.set_error(format!("保存失败，窗口保持打开，请重试：{error:#}"), cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn start_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs_disabled() || !self.environment.as_ref().is_some_and(|env| env.ready) {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        let submitted_revision = workspace.revision;
        self.running = true;
        self.run_generation = self.run_generation.wrapping_add(1);
        let generation = self.run_generation;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellation = Some(Arc::clone(&cancellation));
        self.report = None;
        self.trace.clear();
        self.trace_selected = None;
        self.omitted_traces = 0;
        self.evidence_pane = EvidencePane::Trace;
        self.notice = Some(LearningNotice {
            text: "正在保存本次代码并运行实验；结果和轨迹来自实际执行。".to_string(),
            error: false,
        });
        let (sender, receiver) = async_channel::bounded::<LearningTrace>(128);
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = receiver.recv().await {
                let accepted = view.update(cx, |this, cx| {
                    if this.run_generation != generation || !this.running {
                        return false;
                    }
                    this.push_trace(event);
                    cx.notify();
                    true
                });
                if !matches!(accepted, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let result = service.run(workspace, cancellation, sender).await;
            // run persists the exact code revision before execution. Refresh even
            // after cancellation/error so the next save uses that new revision.
            let snapshot = service.snapshot().await;
            let _ = view.update_in(cx, |this, window, cx| {
                if this.run_generation != generation {
                    return;
                }
                this.running = false;
                this.cancellation = None;
                this.run_generation = this.run_generation.wrapping_add(1);
                match result {
                    Ok(report) => {
                        this.install_report(report);
                        this.evidence_pane = EvidencePane::Result;
                        this.notice = Some(LearningNotice {
                            text: "本次实验已结束。运行判定与学习证据请分别查看。".to_string(),
                            error: false,
                        });
                    }
                    Err(error) => {
                        // Persistence failures must remain visible even when the
                        // user requested closing during the run.
                        this.set_error(format!("实验未完成：{error:#}"), cx);
                    }
                }
                match snapshot {
                    Ok(snapshot) => {
                        if snapshot.workspace.revision == submitted_revision && this.dirty(cx) {
                            // Validation may fail before run saved anything. Keep
                            // unsaved input instead of restoring the older code.
                            this.lessons = snapshot.lessons;
                            this.history = snapshot.history;
                            this.environment = Some(snapshot.environment);
                        } else {
                            this.apply_snapshot(snapshot, window, cx);
                        }
                        this.advance_close(window, cx);
                    }
                    Err(error) => {
                        this.reload_required = true;
                        this.set_error(
                            format!("实验已结束，工作区版本尚未确认；请点击重新检测载入已保存记录：{error:#}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn push_trace(&mut self, trace: LearningTrace) {
        if self.trace.len() == MAX_VISIBLE_TRACES {
            self.trace.remove(0);
            self.omitted_traces += 1;
            self.trace_selected = self.trace_selected.and_then(|index| index.checked_sub(1));
        }
        self.trace.push(trace);
    }

    fn install_report(&mut self, report: LearningRunReport) {
        self.omitted_traces = report.trace.len().saturating_sub(MAX_VISIBLE_TRACES);
        self.trace = report
            .trace
            .iter()
            .skip(self.omitted_traces)
            .cloned()
            .collect();
        self.trace_selected = None;
        self.report = Some(report);
    }

    fn cancel_run(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.store(true, Ordering::Release);
            self.notice = Some(LearningNotice {
                text: "已请求取消，正在等待执行器停止并保存真实状态…".to_string(),
                error: false,
            });
            cx.notify();
        }
    }

    fn load_history(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.operation = Some(Operation::LoadingReport);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = service.load_report(id).await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match outcome {
                    Ok(report) => {
                        this.install_report(report);
                        this.evidence_pane = EvidencePane::Result;
                        this.advance_close(window, cx);
                    }
                    Err(error) => this.set_error(format!("无法读取历史记录：{error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs_disabled() {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("导出第{}章学习备份", self.service.chapter()))
            .set_filename(format!(
                "墨页-Agent第{}章学习备份.json",
                self.service.chapter()
            ))
            .add_filter("墨页学习备份", ["json"])
            .save_single_file();
        self.operation = Some(Operation::Exporting);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            // Native dialogs pump messages: show only outside the entity borrow.
            let selected = dialog.show();
            let outcome = match selected {
                Ok(Some(path)) => match service.save(workspace).await {
                    Ok(saved) => {
                        let exported = service.export(path.clone()).await;
                        (Some(saved), exported.map(|()| Some(path)))
                    }
                    Err(error) => (None, Err(error)),
                },
                Ok(None) => (None, Ok(None)),
                Err(error) => (None, Err(anyhow::anyhow!("无法打开导出对话框：{error}"))),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                if let Some(saved) = outcome.0 {
                    this.saved_workspace = Some(saved);
                }
                match outcome.1 {
                    Ok(path) => {
                        if let Some(path) = path {
                            this.notice = Some(LearningNotice {
                                text: format!("学习备份已导出到 {}", path.display()),
                                error: false,
                            });
                        }
                        this.advance_close(window, cx);
                    }
                    Err(error) => this.set_error(format!("导出学习备份失败：{error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || (self.reload_required && self.saved_workspace.is_some()) {
            return;
        }
        let workspace = self.entered_workspace(cx);
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title(format!("恢复第{}章学习备份", self.service.chapter()))
            .add_filter("墨页学习备份", ["json"]);
        self.operation = Some(Operation::Restoring);
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let dialog = match service.restore_dialog_directory().await {
                Ok(directory) => dialog.set_location(&directory),
                Err(error) => {
                    // A default folder is only a convenience. A missing or
                    // inaccessible archive must not prevent choosing a backup.
                    tracing::warn!(%error, "无法准备学习备份默认目录，使用系统文件选择位置");
                    dialog
                }
            };
            // Native dialogs pump messages: show outside the entity borrow.
            let selected = dialog.open_single_file().show();
            let (saved, outcome, records_may_have_changed) = match selected {
                Ok(Some(path)) => match workspace {
                    Some(workspace) => match service.save(workspace).await {
                        Ok(saved) => (Some(saved), service.restore(path).await.map(Some), true),
                        Err(error) => (None, Err(error), true),
                    },
                    None => (None, service.restore(path).await.map(Some), true),
                },
                Ok(None) => (None, Ok(None), false),
                Err(error) => (
                    None,
                    Err(anyhow::anyhow!("无法打开恢复对话框：{error}")),
                    false,
                ),
            };
            let _ = view.update_in(cx, |this, window, cx| {
                this.finish_restore(saved, outcome, records_may_have_changed, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_restore(
        &mut self,
        saved: Option<LearningWorkspace>,
        outcome: Result<Option<LearningSnapshot>>,
        records_may_have_changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation = None;
        if let Some(saved) = saved {
            self.saved_workspace = Some(saved);
        }
        match outcome {
            Ok(snapshot) => {
                if let Some(snapshot) = snapshot {
                    self.replace_inputs(window, cx);
                    self.apply_snapshot(snapshot, window, cx);
                    self.report = None;
                    self.trace.clear();
                    self.trace_selected = None;
                    self.omitted_traces = 0;
                    self.showing_reference = false;
                    self.evidence_pane = EvidencePane::History;
                    self.notice = Some(LearningNotice {
                        text: "学习备份已恢复。导入的历史结果均标记为待核验。".to_string(),
                        error: false,
                    });
                }
                self.advance_close(window, cx);
            }
            Err(error) => {
                // Opening/cancelling the picker never touches the record file.
                // Keep existing uncertainty, but do not invent a revision conflict.
                if records_may_have_changed {
                    self.reload_required = true;
                    self.set_error(format!("恢复未完成，请重新检测当前记录：{error:#}"), cx);
                } else {
                    self.set_error(format!("{error:#}；当前输入已保留，可重试恢复。"), cx);
                }
            }
        }
        cx.notify();
    }

    fn new_round(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs_disabled() {
            return;
        }
        let Some(workspace) = self.entered_workspace(cx) else {
            return;
        };
        self.operation = Some(Operation::NewRound);
        self.notice = Some(LearningNotice {
            text: "正在保存当前编辑并归档本轮历史…".to_string(),
            error: false,
        });
        let service = Arc::clone(&self.service);
        cx.spawn_in(window, async move |view, cx| {
            let outcome = service.new_round(workspace).await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.operation = None;
                match outcome {
                    Ok(snapshot) => {
                        this.replace_inputs(window, cx);
                        this.apply_snapshot(snapshot, window, cx);
                        this.report = None;
                        this.trace.clear();
                        this.trace_selected = None;
                        this.omitted_traces = 0;
                        this.evidence_pane = EvidencePane::History;
                        this.notice = Some(LearningNotice {
                            text: "已开始新一轮，当前代码与笔记保留。此前历史已归档，可通过恢复备份重新载入。".to_string(),
                            error: false,
                        });
                        this.advance_close(window, cx);
                    }
                    Err(error) => {
                        this.reload_required = true;
                        this.set_error(format!("新一轮未完成，请重新检测当前记录：{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.removing {
            return;
        }
        self.closing = true;
        cx.global_mut::<LearningWindowTracker>().closing = true;
        self.cancel_run(cx);
        self.advance_close(window, cx);
        cx.notify();
    }

    fn advance_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing || self.removing {
            return;
        }
        if self.reload_required && !self.running && self.operation.is_none() {
            self.load(window, cx);
            return;
        }
        match close_action(self.running, self.operation.is_some(), self.dirty(cx)) {
            CloseAction::Wait => {
                self.notice = Some(LearningNotice {
                    text: "关闭前正在等待实验停止和已接受的保存完成…".to_string(),
                    error: false,
                });
            }
            CloseAction::Save => self.save(window, cx),
            CloseAction::Remove => {
                self.removing = true;
                remove_window_after_current_frame(window, cx, None);
            }
        }
    }
}

fn scenario_label(chapter: u8, scenario: &str) -> &str {
    chapter_scenarios(chapter)
        .iter()
        .find(|(id, _)| *id == scenario)
        .map_or(scenario, |(_, label)| label)
}

fn implementation_label(chapter: u8, implementation: &str) -> &'static str {
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

fn run_label(status: &str, passed: bool, imported: bool) -> &'static str {
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

fn stop_label(chapter: u8, reason: &str) -> &str {
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
fn fenced_text(text: &str, language: &str) -> String {
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

fn short_text(text: &str, max_characters: usize) -> String {
    let mut characters = text.chars();
    let prefix: String = characters.by_ref().take(max_characters).collect();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn record_time(timestamp: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    relative_record_time(timestamp, now)
}

fn relative_record_time(timestamp: &str, now: u64) -> String {
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

impl Drop for LearningWindow {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.store(true, Ordering::Release);
        }
    }
}

impl LearningWindow {
    fn open_course_document(&mut self, document: CourseDocument, cx: &mut Context<Self>) {
        // Keep execution controls, especially cancellation, reachable while a
        // run is active. Document pages intentionally hide those controls.
        if self.busy() {
            return;
        }
        if document == CourseDocument::Answers {
            if self.inputs_disabled() {
                return;
            }
            self.help_level = "S".to_string();
            self.notice = Some(LearningNotice {
                text: "已查看参考解答，本次帮助标为完整示范。请保留查看前的预测与作答。".into(),
                error: false,
            });
        }
        self.study_pane = StudyPane::Document(document);
        cx.notify();
    }

    #[inline(never)]
    fn render_document_button(
        &self,
        document: CourseDocument,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let view = cx.entity();
        Button::new(SharedString::from(format!(
            "learning-document-{}",
            document.path()
        )))
        .debug_selector(move || format!("learning-document-{}", document.path()))
        .ghost()
        .w_full()
        .justify_start()
        .label(document.title())
        .when(self.study_pane == StudyPane::Document(document), |button| {
            button.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT_DARK))
        })
        .disabled(self.busy() || (document == CourseDocument::Answers && self.inputs_disabled()))
        .on_click(move |_, _, cx| {
            view.update(cx, |this, cx| this.open_course_document(document, cx));
        })
        .into_any_element()
    }

    #[inline(never)]
    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let save_view = cx.entity();
        let export_view = cx.entity();
        let restore_view = cx.entity();
        let refresh_view = cx.entity();
        let round_view = cx.entity();
        let disabled = self.inputs_disabled();
        let actions = div()
            .id("learning-header-actions")
            .h_flex()
            .min_w(px(0.))
            .gap_2()
            .overflow_x_scroll()
            .child(
                Button::new("learning-save")
                    .outline()
                    .label(format!("保存第{}章记录", self.service.chapter()))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        save_view.update(cx, |this, cx| this.save(window, cx));
                    }),
            )
            .child(
                Button::new("learning-export")
                    .ghost()
                    .icon(IconName::ArrowUp)
                    .label(format!("导出第{}章", self.service.chapter()))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        export_view.update(cx, |this, cx| this.export(window, cx));
                    }),
            )
            .child(
                Button::new("learning-restore")
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .label(format!("恢复第{}章", self.service.chapter()))
                    .tooltip("先保存当前章节，再恢复该章备份；其它章节不受影响")
                    .disabled(
                        self.busy() || (self.reload_required && self.saved_workspace.is_some()),
                    )
                    .on_click(move |_, window, cx| {
                        restore_view.update(cx, |this, cx| this.restore(window, cx));
                    }),
            )
            .child(
                Button::new("learning-new-round")
                    .ghost()
                    .label("新一轮")
                    .tooltip("保存当前代码与笔记，将本轮历史存入档案后开始新一轮")
                    .disabled(disabled || self.history.is_empty())
                    .on_click(move |_, window, cx| {
                        round_view.update(cx, |this, cx| this.new_round(window, cx));
                    }),
            )
            .child(
                Button::new("learning-refresh")
                    .ghost()
                    .icon(IconName::Redo2)
                    .label("重新检测")
                    .tooltip("保存当前编辑并重新检查运行环境")
                    .disabled(self.busy())
                    .on_click(move |_, window, cx| {
                        refresh_view.update(cx, |this, cx| this.load(window, cx));
                    }),
            )
            .into_any_element();
        div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .py_3()
            .bg(rgb(SURFACE))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .min_w(px(0.))
                    .child(Icon::new(IconName::BookOpen).text_color(rgb(ACCENT)))
                    .child(
                        div()
                            .v_flex()
                            .min_w(px(0.))
                            .child(div().text_lg().font_semibold().child("学习 · AI Agent"))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(
                                match self.study_pane {
                                    StudyPane::Document(document) => document.title(),
                                    _ => chapter_title(self.service.chapter()),
                                },
                            )),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    #[inline(never)]
    fn render_notice(&self) -> Option<gpui::AnyElement> {
        let notice = self.notice.as_ref()?;
        Some(
            div()
                .h_flex()
                .flex_none()
                .items_start()
                .gap_2()
                .px_5()
                .py_2()
                .bg(rgb(if notice.error { 0xf8e2de } else { ACCENT_SOFT }))
                .text_color(rgb(if notice.error { DANGER } else { ACCENT_DARK }))
                .text_sm()
                .child(
                    Icon::new(if notice.error {
                        IconName::TriangleAlert
                    } else {
                        IconName::Info
                    })
                    .small(),
                )
                .child(notice.text.clone())
                .into_any_element(),
        )
    }

    #[inline(never)]
    fn render_navigation(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut steps = div().v_flex().gap_2();
        for (index, lesson) in self.lessons.iter().enumerate() {
            let view = cx.entity();
            let selected =
                self.selected_lesson == index && !matches!(self.study_pane, StudyPane::Document(_));
            steps = steps.child(
                Button::new(SharedString::from(format!("learning-step-{}", lesson.id)))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .label(lesson.title.clone())
                    .when(selected, |button| {
                        button.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT_DARK))
                    })
                    .disabled(self.inputs_disabled())
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.inputs_disabled() {
                                return;
                            }
                            this.selected_lesson = index;
                            this.study_pane = StudyPane::Lesson;
                            this.showing_reference = false;
                            cx.notify();
                        });
                    }),
            );
        }
        div()
            .v_flex()
            .w(px(244.))
            .flex_none()
            .h_full()
            .min_h(px(0.))
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .px_4()
                    .py_4()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(MUTED))
                    .child("课程导航"),
            )
            .child(
                div()
                    .id("learning-steps-scroll")
                    .v_flex()
                    .px_2()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(self.render_document_button(CourseDocument::Tutorial, cx))
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("完整教程 · 十章"),
                    )
                    .children(
                        CourseDocument::CHAPTERS
                            .into_iter()
                            .map(|chapter| self.render_document_button(chapter, cx)),
                    )
                    .child(div().px_2().py_3().text_xs().text_color(rgb(MUTED)).child(
                        if self.service.chapter() == 1 {
                            "第1章 · 六步练习".to_string()
                        } else {
                            format!("第{}章 · 练习任务", self.service.chapter())
                        },
                    ))
                    .child(steps)
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("第一章 · 实验资料"),
                    )
                    .children(
                        CourseDocument::MATERIALS
                            .into_iter()
                            .map(|document| self.render_document_button(document, cx)),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap_2()
                    .p_4()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("手搓 → 框架重建 → 同题比较")
                    .child("先写预测，再看运行。保留第一次判断和使用过的提示。")
                    .child("十章均可进入代码实验；切换实验章节时自动保存当前编辑。各章记录独立。"),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_study_tabs(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut tabs = div().h_flex().gap_1().px_3().py_2();
        for (id, label, pane) in [
            ("learning-tab-lesson", "讲义", StudyPane::Lesson),
            ("learning-tab-code", "代码实验", StudyPane::Code),
            ("learning-tab-notes", "预测与作答", StudyPane::Notes),
        ] {
            let view = cx.entity();
            let selected = self.study_pane == pane;
            tabs = tabs.child(
                Button::new(id)
                    .ghost()
                    .label(label)
                    .when(selected, |button| {
                        button.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT_DARK))
                    })
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.study_pane = pane;
                            cx.notify();
                        });
                    }),
            );
        }
        let view = cx.entity();
        let chapter = self.service.chapter();
        tabs.child(
            Button::new("learning-current-chapter-text")
                .debug_selector(|| "learning-current-chapter-text".into())
                .ghost()
                .label("本章正文")
                .disabled(self.busy())
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.open_course_document(
                            CourseDocument::CHAPTERS[usize::from(chapter - 1)],
                            cx,
                        );
                    });
                }),
        )
        .border_b_1()
        .border_color(rgb(BORDER))
        .into_any_element()
    }

    #[inline(never)]
    fn render_chapter_navigation(
        &self,
        document: CourseDocument,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut navigation = div().h_flex().gap_2();
        if let Some(index) = document.chapter_index() {
            for (id, label, destination) in [
                (
                    "learning-previous-chapter",
                    "上一章",
                    index
                        .checked_sub(1)
                        .map(|previous| CourseDocument::CHAPTERS[previous]),
                ),
                (
                    "learning-next-chapter",
                    "下一章",
                    CourseDocument::CHAPTERS.get(index + 1).copied(),
                ),
            ] {
                if let Some(destination) = destination {
                    let view = cx.entity();
                    navigation = navigation.child(
                        Button::new(id)
                            .debug_selector(move || id.into())
                            .ghost()
                            .label(label)
                            .tooltip(destination.title())
                            .disabled(self.busy())
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.open_course_document(destination, cx)
                                });
                            }),
                    );
                }
            }
        }
        navigation.into_any_element()
    }

    #[inline(never)]
    fn render_course_document(
        &self,
        document: CourseDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let guide_view = cx.entity();
        let continue_view = cx.entity();
        let chapter = document
            .chapter_index()
            .map_or(self.service.chapter(), |index| index as u8 + 1);
        let resume = chapter == self.service.chapter()
            && (self.selected_lesson > 0
                || self.dirty(cx)
                || self
                    .saved_workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.revision > 0 || workspace.lesson_index > 0));
        let actions = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .when(document != CourseDocument::Tutorial, |row| {
                row.child(
                    Button::new("learning-back-to-guide")
                        .debug_selector(|| "learning-back-to-guide".into())
                        .ghost()
                        .label("教程总览")
                        .disabled(self.busy())
                        .on_click(move |_, _, cx| {
                            guide_view.update(cx, |this, cx| {
                                this.open_course_document(CourseDocument::Tutorial, cx)
                            });
                        }),
                )
            })
            .when(
                document == CourseDocument::Tutorial || document.chapter_index().is_some(),
                |row| {
                    row.child(
                        Button::new("learning-start-or-continue")
                            .debug_selector(move || {
                                if chapter > 1 {
                                    format!("learning-enter-chapter-{chapter}")
                                } else if resume {
                                    "learning-continue-first-chapter".into()
                                } else {
                                    "learning-start-first-chapter".into()
                                }
                            })
                            .primary()
                            .label(if document == CourseDocument::Tutorial && chapter > 1 {
                                format!("继续第{chapter}章实验")
                            } else if chapter > 1 {
                                "本章代码实验".to_string()
                            } else if resume {
                                "继续第一章".to_string()
                            } else {
                                "开始第一章".to_string()
                            })
                            .disabled(self.chapter_entry_disabled())
                            .on_click(move |_, window, cx| {
                                continue_view.update(cx, |this, cx| {
                                    this.enter_chapter(chapter, window, cx);
                                });
                            }),
                    )
                },
            )
            .child(self.render_chapter_navigation(document, cx))
            .into_any_element();
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .child(actions)
            .child(div().px_4().pt_2().text_xs().text_color(rgb(MUTED)).child(
                if document.chapter_index().is_some_and(|index| index > 0) {
                    "读完本章后进入“本章代码实验”：先预测，再编写手搓与库版本，查看宿主检查和运行轨迹。"
                } else {
                    "各章与第一章六步讲义可从左侧打开；外部链接均为可选延伸阅读。"
                },
            ))
            .child(div().flex_1().min_h(px(0.)).min_w(px(0.)).p_4().child(
                scrollable_learning_text(
                    SharedString::from(format!("learning-document-text-{}", document.path())),
                    course_display_markdown(document.markdown()),
                    window,
                    cx,
                ),
            ))
            .into_any_element()
    }

    #[inline(never)]
    fn render_lesson_navigation(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let previous_view = cx.entity();
        let next_view = cx.entity();
        div()
            .h_flex()
            .flex_none()
            .justify_between()
            .items_center()
            .border_t_1()
            .border_color(rgb(BORDER))
            .pt_2()
            .child(
                Button::new("learning-previous-lesson")
                    .debug_selector(|| "learning-previous-lesson".into())
                    .ghost()
                    .label("上一节")
                    .disabled(self.inputs_disabled() || self.selected_lesson == 0)
                    .on_click(move |_, _, cx| {
                        previous_view.update(cx, |this, cx| {
                            if this.inputs_disabled() || this.selected_lesson == 0 {
                                return;
                            }
                            this.selected_lesson -= 1;
                            cx.notify();
                        });
                    }),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "第{}章 · 第 {} / {} 节",
                self.service.chapter(),
                self.selected_lesson + 1,
                self.lessons.len(),
            )))
            .child(
                Button::new("learning-next-lesson")
                    .debug_selector(|| "learning-next-lesson".into())
                    .ghost()
                    .label(if self.selected_lesson + 1 < self.lessons.len() {
                        "下一节".to_string()
                    } else if self.service.chapter() == 10 {
                        "教程总览".to_string()
                    } else {
                        format!("进入第{}章", self.service.chapter() + 1)
                    })
                    .disabled(self.inputs_disabled())
                    .on_click(move |_, _, cx| {
                        next_view.update(cx, |this, cx| {
                            if this.inputs_disabled() {
                                return;
                            }
                            if this.selected_lesson + 1 < this.lessons.len() {
                                this.selected_lesson += 1;
                            } else {
                                let destination = CourseDocument::CHAPTERS
                                    .get(usize::from(this.service.chapter()))
                                    .copied()
                                    .unwrap_or(CourseDocument::Tutorial);
                                this.open_course_document(destination, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_lesson(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let content = match self.lessons.get(self.selected_lesson) {
            Some(lesson) => scrollable_learning_text(
                SharedString::from(format!(
                    "learning-lesson-{}-{}",
                    self.service.chapter(),
                    lesson.id
                )),
                course_display_markdown(&lesson.markdown),
                window,
                cx,
            ),
            None => div()
                .p_5()
                .text_color(rgb(MUTED))
                .child(if self.operation == Some(Operation::Loading) {
                    "正在读取课程与学习记录…"
                } else {
                    "课程尚未载入，请点击重新检测。"
                })
                .into_any_element(),
        };
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .p_4()
            .child(div().flex_1().min_h(px(0.)).child(content))
            .when(!self.lessons.is_empty(), |body| {
                body.child(self.render_lesson_navigation(cx))
            })
            .into_any_element()
    }

    #[inline(never)]
    fn render_code(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let reference_view = cx.entity();
        let copy_view = cx.entity();
        let header = div()
            .h_flex()
            .justify_between()
            .gap_2()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(rgb(MUTED))
            .child(if self.showing_reference {
                "参考实现 · 只读 · 运行仍使用你的代码"
            } else if self.implementation == "langgraph" {
                "我的实现 · 库版本 Python"
            } else {
                "我的实现 · manual.py"
            })
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .child(
                        Button::new("learning-reference-toggle")
                            .ghost()
                            .small()
                            .icon(IconName::Eye)
                            .label(if self.showing_reference { "返回我的代码" } else { "查看参考" })
                            .disabled(self.inputs_disabled())
                            .on_click(move |_, _, cx| {
                                reference_view.update(cx, |this, cx| {
                                    if this.inputs_disabled() {
                                        return;
                                    }
                                    this.showing_reference = !this.showing_reference;
                                    if this.showing_reference {
                                        this.help_level = "S".to_string();
                                        this.notice = Some(LearningNotice {
                                            text: "已将本次帮助标为完整示范。参考代码只读，你的实现保持在编辑区。".to_string(),
                                            error: false,
                                        });
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .when(self.showing_reference, |row| {
                        row.child(
                            Button::new("learning-reference-copy")
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .label("复制")
                                .disabled(self.inputs_disabled())
                                .on_click(move |_, _, cx| {
                                    copy_view.update(cx, |this, cx| {
                                        if this.inputs_disabled() {
                                            return;
                                        }
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            reference_code_for(this.service.chapter(), &this.implementation).to_string(),
                                        ));
                                        this.help_level = "S".to_string();
                                        cx.notify();
                                    });
                                }),
                        )
                    }),
            )
            .into_any_element();
        let body = if self.showing_reference {
            scrollable_learning_text(
                SharedString::from(format!(
                    "learning-reference-text-{}-{}",
                    self.service.chapter(),
                    self.implementation
                )),
                fenced_text(
                    reference_code_for(self.service.chapter(), &self.implementation),
                    CODE_LANGUAGE,
                ),
                window,
                cx,
            )
        } else {
            let input = if self.implementation == "langgraph" {
                &self.langgraph_input
            } else {
                &self.manual_input
            };
            Input::new(input)
                .h_full()
                .disabled(self.inputs_disabled())
                .into_any_element()
        };
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .child(header)
            .child(div().flex_1().min_h(px(0.)).p_2().child(body))
            .into_any_element()
    }

    #[inline(never)]
    fn render_notes(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let menu_view = cx.entity();
        let selected_help = self.help_level.clone();
        let label = HELP_LEVELS
            .iter()
            .find(|(id, _)| *id == selected_help)
            .map_or("选择帮助级别", |(_, label)| *label);
        let help = Button::new("learning-help-level")
            .outline()
            .small()
            .label(label)
            .icon(IconName::ChevronDown)
            .disabled(self.inputs_disabled())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (id, label) in HELP_LEVELS {
                    let view = menu_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(selected_help == id)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if !this.inputs_disabled() {
                                        this.help_level = id.to_string();
                                        cx.notify();
                                    }
                                });
                            }),
                    );
                }
                menu
            });
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_4()
            .child(div().text_sm().font_semibold().child("运行前预测"))
            .child(
                div()
                    .h(px(116.))
                    .flex_none()
                    .child(Input::new(&self.prediction_input).h_full().disabled(self.inputs_disabled())),
            )
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("本章作答与复盘"))
                    .child(help),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(Input::new(&self.notes_input).h_full().disabled(self.inputs_disabled())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("记录首次判断、提示和修改理由。每次运行保存当时的代码、预测及帮助级别；评分仍待评阅。"),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_run_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let scenario_view = cx.entity();
        let selected_scenario = self.scenario.clone();
        let chapter = self.service.chapter();
        let run_view = cx.entity();
        let cancel_view = cx.entity();
        let disabled = self.inputs_disabled();
        let can_run = !disabled && self.environment.as_ref().is_some_and(|env| env.ready);
        let scenario_button = Button::new("learning-scenario")
            .outline()
            .small()
            .label(scenario_label(chapter, &self.scenario).to_string())
            .icon(IconName::ChevronDown)
            .disabled(disabled)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for &(id, label) in chapter_scenarios(chapter) {
                    let view = scenario_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(selected_scenario == id)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if !this.inputs_disabled() {
                                        this.scenario = id.to_string();
                                        cx.notify();
                                    }
                                });
                            }),
                    );
                }
                menu
            });
        let mut implementations = div().h_flex().gap_1();
        for id in ["manual", "langgraph"] {
            let label = implementation_label(chapter, id);
            let view = cx.entity();
            implementations = implementations.child(
                Button::new(SharedString::from(format!("learning-implementation-{id}")))
                    .ghost()
                    .small()
                    .label(label)
                    .when(self.implementation == id, |button| {
                        button.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT_DARK))
                    })
                    .disabled(disabled)
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if !this.inputs_disabled() {
                                this.implementation = id.to_string();
                                cx.notify();
                            }
                        });
                    }),
            );
        }
        let mut row = div()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(implementations)
            .child(scenario_button)
            .child(
                Button::new("learning-run")
                    .primary()
                    .small()
                    .icon(IconName::SquareTerminal)
                    .label(if self.running {
                        "运行中…"
                    } else {
                        "运行我的代码"
                    })
                    .disabled(!can_run)
                    .on_click(move |_, window, cx| {
                        run_view.update(cx, |this, cx| this.start_run(window, cx));
                    }),
            );
        if self.running {
            row = row.child(
                Button::new("learning-cancel")
                    .debug_selector(|| "learning-cancel".into())
                    .outline()
                    .small()
                    .label("取消")
                    .disabled(
                        self.cancellation
                            .as_ref()
                            .is_some_and(|token| token.load(Ordering::Acquire)),
                    )
                    .on_click(move |_, _, cx| {
                        cancel_view.update(cx, |this, cx| this.cancel_run(cx));
                    }),
            );
        }
        div()
            .v_flex()
            .flex_none()
            .gap_1p5()
            .px_3()
            .py_3()
            .border_t_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(row)
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if chapter == 1 {
                        "本章使用固定模型响应与模拟只读工具，让两种实现面对相同任务。"
                    } else {
                        "使用运行时生成的虚构练习数据，未调用真实大模型。读取和模拟操作由宿主记录并检查。"
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_evidence_tabs(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut tabs = div().h_flex().gap_1().px_2().py_2();
        for (id, label, pane) in [
            ("learning-result-tab", "结果", EvidencePane::Result),
            ("learning-trace-tab", "逐步轨迹", EvidencePane::Trace),
            ("learning-attempt-tab", "本次作答", EvidencePane::Attempt),
            ("learning-history-tab", "历史", EvidencePane::History),
        ] {
            let view = cx.entity();
            tabs = tabs.child(
                Button::new(id)
                    .ghost()
                    .label(label)
                    .when(self.evidence_pane == pane, |button| {
                        button.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT_DARK))
                    })
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.evidence_pane = pane;
                            cx.notify();
                        });
                    }),
            );
        }
        tabs.border_b_1()
            .border_color(rgb(BORDER))
            .into_any_element()
    }

    #[inline(never)]
    fn render_result(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(report) = &self.report else {
            return div()
                .v_flex()
                .gap_3()
                .p_4()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(if self.running {
                    "正在执行你的代码…"
                } else {
                    "这里还没有运行结果。"
                })
                .child("填写预测并运行代码后，这里会展示实际结果、调用计数和停止原因。")
                .child("代码通过检查与学会了什么，将分别记录。")
                .into_any_element();
        };
        let color = if report.imported {
            MUTED
        } else if report.passed {
            0x376441
        } else {
            DANGER
        };
        let mut checks = div().v_flex().gap_1p5();
        if let Some(items) = report
            .raw
            .get("checks")
            .and_then(serde_json::Value::as_array)
        {
            for item in items.iter().take(24) {
                let passed = item.get("passed").and_then(serde_json::Value::as_bool) == Some(true);
                let detail = item
                    .get("detail")
                    .or_else(|| item.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("运行检查");
                let prefix = if report.imported {
                    "原记录"
                } else if passed {
                    "通过"
                } else {
                    "未通过"
                };
                checks = checks.child(
                    div()
                        .text_xs()
                        .text_color(rgb(if report.imported || passed {
                            MUTED
                        } else {
                            DANGER
                        }))
                        .child(format!("{prefix} · {detail}")),
                );
            }
        }
        let answer = if report.answer.trim().is_empty() {
            "本次没有可提交的最终答案。"
        } else {
            &report.answer
        };
        div()
            .id("learning-result-scroll")
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_3()
            .p_4()
            .overflow_y_scroll()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .text_color(rgb(color))
                    .child(div().font_semibold().child(run_label(&report.status, report.passed, report.imported)))
                    .child(div().text_xs().child(format!("停止原因：{}", stop_label(self.service.chapter(), &report.stop_reason)))),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("记录 {}", short_text(&report.id, 14)))
                    .child(format!("记录时间：{}", record_time(&report.created_at))),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1p5()
                    .p_3()
                    .rounded(px(8.))
                    .bg(rgb(PAPER))
                    .text_sm()
                    .child(format!("{}：{}", if self.service.chapter() == 1 { "模型决策" } else { "练习输入获取" }, report.metrics.model_decisions))
                    .child(format!("实际工具执行：{}", report.metrics.actual_tool_executions))
                    .child(format!("框架节点标注（学员）：{}", report.metrics.framework_steps))
                    .child(format!("耗时：{:.2} 秒", report.metrics.elapsed_seconds)),
            )
            .child(div().text_sm().font_semibold().child("最终答案"))
            .child(TextView::markdown(
                "learning-answer-text",
                fenced_text(answer, "json"),
                window,
                cx,
            ).selectable(true))
            .when_some(report.raw.pointer("/error/message").and_then(serde_json::Value::as_str), |panel, error| {
                panel.child(div().text_sm().font_semibold().child("运行诊断"))
                    .child(TextView::markdown("learning-runtime-error", fenced_text(error, "text"), window, cx).selectable(true))
            })
            .child(checks)
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .p_3()
                    .rounded(px(8.))
                    .bg(rgb(ACCENT_SOFT))
                    .text_xs()
                    .text_color(rgb(ACCENT_DARK))
                    .child("学习证据：待评阅")
                    .child(if report.imported {
                        "这份报告来自导入备份，其历史结果和来源尚未重新核验。"
                    } else {
                        "运行结果只说明本次代码表现。理解、独立完成和迁移需要结合首次作答与复测判断。"
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_attempt(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(report) = &self.report else {
            return div()
                .p_4()
                .text_sm()
                .text_color(rgb(MUTED))
                .child("选择一条历史记录，或运行一次代码后，查看当时保存的代码、预测和学习笔记。")
                .into_any_element();
        };
        let workspace = &report.workspace;
        let help = HELP_LEVELS
            .iter()
            .find(|(id, _)| *id == workspace.help_level)
            .map_or("帮助级别待核验", |(_, label)| *label);
        let prediction = if workspace.prediction.trim().is_empty() {
            "本次未填写运行前预测。"
        } else {
            &workspace.prediction
        };
        let notes = if workspace.notes.trim().is_empty() {
            "本次未填写学习笔记。"
        } else {
            &workspace.notes
        };
        let code = if workspace.implementation == "langgraph" {
            &workspace.langgraph_code
        } else {
            &workspace.manual_code
        };
        let markdown = format!(
            "### 运行前预测\n\n{}\n\n### 当时的学习笔记\n\n{}\n\n### 本次运行代码\n\n{}",
            fenced_text(prediction, "text"),
            fenced_text(notes, "text"),
            fenced_text(code, CODE_LANGUAGE),
        );
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_3()
            .child(div().text_sm().font_semibold().child("本次作答快照 · 只读"))
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "{} · {} · {}",
                implementation_label(self.service.chapter(), &workspace.implementation),
                scenario_label(self.service.chapter(), &workspace.scenario),
                help,
            )))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if report.imported {
                        "导入记录 · 待核验。这里展示备份保存的作答，不能证明当时独立完成。"
                    } else {
                        "这里保留提交运行时的作答；当前编辑区的后续修改不会改写这份快照。"
                    }),
            )
            .child(div().flex_1().min_h(px(0.)).child(scrollable_learning_text(
                "learning-attempt-text",
                markdown,
                window,
                cx,
            )))
            .into_any_element()
    }

    #[inline(never)]
    fn render_trace(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.trace.is_empty() {
            return div()
                .p_4()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(if self.running {
                    "正在等待执行器的实际事件…"
                } else {
                    "当前没有可展示的执行轨迹。"
                })
                .into_any_element();
        }
        let selected = self
            .trace_selected
            .unwrap_or(self.trace.len() - 1)
            .min(self.trace.len() - 1);
        let mut events = div()
            .id("learning-trace-event-list")
            .v_flex()
            .h(px(170.))
            .min_h(px(0.))
            .gap_1()
            .overflow_y_scroll();
        for (index, event) in self.trace.iter().enumerate() {
            let view = cx.entity();
            events = events.child(
                Button::new(SharedString::from(format!("learning-trace-event-{index}")))
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .label(format!(
                        "{} · {}",
                        self.omitted_traces + index + 1,
                        short_text(&event.label, 36)
                    ))
                    .when(index == selected, |button| button.bg(rgb(ACCENT_SOFT)))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.trace_selected = Some(index);
                            cx.notify();
                        });
                    }),
            );
        }
        let previous_view = cx.entity();
        let next_view = cx.entity();
        let event = &self.trace[selected];
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_3()
            .when(self.omitted_traces > 0, |panel| {
                panel.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                    "当前显示最近 {MAX_VISIBLE_TRACES} 条；完整报告保留更早事件。"
                )))
            })
            .child(events)
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("learning-trace-previous")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label("上一步")
                            .disabled(selected == 0)
                            .on_click(move |_, _, cx| {
                                previous_view.update(cx, |this, cx| {
                                    this.trace_selected = Some(selected.saturating_sub(1));
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("learning-trace-next")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronRight)
                            .label("下一步")
                            .disabled(selected + 1 >= self.trace.len())
                            .on_click(move |_, _, cx| {
                                next_view.update(cx, |this, cx| {
                                    this.trace_selected = Some(
                                        (selected + 1).min(this.trace.len().saturating_sub(1)),
                                    );
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(div().text_sm().font_semibold().child(event.label.clone()))
            .child(div().flex_1().min_h(px(0.)).child(scrollable_learning_text(
                "learning-trace-detail",
                fenced_text(&event.detail, "text"),
                window,
                cx,
            )))
            .into_any_element()
    }

    #[inline(never)]
    fn render_history(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("learning-history-scroll")
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .p_3()
            .overflow_y_scroll();
        if self.history.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("尚无运行历史。第一次运行后会保留当时的代码、预测与结果。"),
            );
        }
        for (index, entry) in self.history.iter().enumerate() {
            let view = cx.entity();
            let id = entry.id.clone();
            let selected = self.report.as_ref().is_some_and(|report| report.id == id);
            list = list.child(
                div()
                    .id(SharedString::from(format!("learning-history-{}", entry.id)))
                    .v_flex()
                    .gap_1p5()
                    .p_3()
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { BORDER }))
                    .rounded(px(8.))
                    .bg(rgb(SURFACE))
                    .child(div().text_sm().font_semibold().child(format!(
                        "{} · {}",
                        implementation_label(self.service.chapter(), &entry.implementation),
                        scenario_label(self.service.chapter(), &entry.scenario)
                    )))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(run_label(
                        &entry.status,
                        entry.passed,
                        entry.imported,
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(record_time(&entry.created_at)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("learning-history-open-{index}")))
                            .outline()
                            .small()
                            .label("查看结果与轨迹")
                            .disabled(self.busy())
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.load_history(id.clone(), window, cx)
                                });
                            }),
                    ),
            );
        }
        list.into_any_element()
    }

    #[inline(never)]
    fn render_environment(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(environment) = &self.environment else {
            return div().into_any_element();
        };
        let url = environment.install_url.clone();
        let refresh_view = cx.entity();
        div()
            .v_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(div().font_semibold().child(if environment.ready {
                "运行环境"
            } else {
                "运行环境尚未就绪"
            }))
            .child(environment.message.clone())
            .when(!environment.ready, |panel| {
                panel.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .when_some(url, |row, url| {
                            row.child(
                                Button::new("learning-install-help")
                                    .outline()
                                    .small()
                                    .icon(IconName::ExternalLink)
                                    .label("官方安装说明")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                        })
                        .child(
                            Button::new("learning-env-recheck")
                                .ghost()
                                .small()
                                .label("重新检测")
                                .disabled(self.busy())
                                .on_click(move |_, window, cx| {
                                    refresh_view.update(cx, |this, cx| this.load(window, cx));
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    #[inline(never)]
    fn render_center(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let StudyPane::Document(document) = self.study_pane {
            return div()
                .v_flex()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .h_full()
                .bg(rgb(SURFACE))
                .child(self.render_course_document(document, window, cx))
                .child(self.render_environment(cx))
                .into_any_element();
        }
        let pane = match self.study_pane {
            StudyPane::Document(_) => unreachable!("course documents are rendered above"),
            StudyPane::Lesson => self.render_lesson(window, cx),
            StudyPane::Code => self.render_code(window, cx),
            StudyPane::Notes => self.render_notes(cx),
        };
        div()
            .v_flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .h_full()
            .bg(rgb(SURFACE))
            .child(self.render_study_tabs(cx))
            .child(pane)
            .child(self.render_run_controls(cx))
            .into_any_element()
    }

    #[inline(never)]
    fn render_evidence(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let pane = match self.evidence_pane {
            EvidencePane::Result => self.render_result(window, cx),
            EvidencePane::Trace => self.render_trace(window, cx),
            EvidencePane::Attempt => self.render_attempt(window, cx),
            EvidencePane::History => self.render_history(cx),
        };
        div()
            .v_flex()
            .w(px(350.))
            .flex_none()
            .h_full()
            .min_h(px(0.))
            .bg(rgb(PAPER))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(self.render_evidence_tabs(cx))
            .child(div().v_flex().flex_1().min_h(px(0.)).child(pane))
            .child(self.render_environment(cx))
            .into_any_element()
    }
}

impl Render for LearningWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let navigation = self.render_navigation(cx);
        let center = self.render_center(window, cx);
        let evidence = (!matches!(self.study_pane, StudyPane::Document(_)))
            .then(|| self.render_evidence(window, cx));
        let status = if self.closing {
            "关闭前等待保存与取消完成"
        } else if self.reload_required {
            "工作区版本尚未确认，请重新检测"
        } else if self.running {
            "实验运行中"
        } else if self.operation.is_some() {
            "正在处理学习记录"
        } else if self.dirty(cx) {
            "有未保存的修改"
        } else if self.saved_workspace.is_some() {
            "学习记录已保存"
        } else {
            "学习工作区未载入"
        };
        div()
            .v_flex()
            .size_full()
            .min_h(px(0.))
            .text_color(rgb(INK))
            .bg(rgb(PAPER))
            .child(header)
            .when_some(self.render_notice(), |root, notice| root.child(notice))
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(navigation)
                    .child(center)
                    .when_some(evidence, |row, evidence| row.child(evidence)),
            )
            .child(render_status_bar(
                IconName::BookOpen,
                status.to_string(),
                MUTED,
                match self.study_pane {
                    StudyPane::Document(document) => document.title().to_string(),
                    _ => format!(
                        "第{}章 · 第 {} / {} 步 · {}",
                        self.service.chapter(),
                        self.selected_lesson + 1,
                        self.lessons.len().max(1),
                        implementation_label(self.service.chapter(), &self.implementation)
                    ),
                },
                "本章学习证据待评阅".to_string(),
            ))
    }
}

pub(super) fn open_learning_window(services: Arc<AppServices>, cx: &mut App) -> Result<()> {
    if !cx.has_global::<LearningWindowTracker>() {
        cx.set_global(LearningWindowTracker::default());
    }
    let tracker = cx.global::<LearningWindowTracker>();
    if tracker.opening {
        return Ok(());
    }
    if let Some(handle) = tracker.handle
        && cx.windows().contains(&handle)
    {
        if !tracker.closing {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .context("无法激活学习窗口")?;
        }
        return Ok(());
    }
    {
        let tracker = cx.global_mut::<LearningWindowTracker>();
        tracker.opening = true;
        tracker.closing = false;
        tracker.handle = None;
    }
    let service = services.learning();
    let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(1120.), px(700.))),
            titlebar: Some(TitlebarOptions {
                title: Some("墨页 · AI Agent 学习".into()),
                ..Default::default()
            }),
            app_id: Some("dev.moye.epub-editor.learning".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let learning = cx.new(|cx| LearningWindow::new(service, window, cx));
            let weak = learning.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                if weak
                    .update(cx, |this, cx| this.request_close(window, cx))
                    .is_err()
                {
                    remove_window_after_current_frame(window, cx, None);
                }
                false
            });
            learning.update(cx, |this, cx| this.load(window, cx));
            cx.new(|cx| Root::new(learning, window, cx))
        },
    );
    let tracker = cx.global_mut::<LearningWindowTracker>();
    tracker.opening = false;
    match opened {
        Ok(handle) => {
            tracker.handle = Some(handle.into());
            Ok(())
        }
        Err(error) => {
            tracker.closing = false;
            Err(error).context("无法创建学习窗口")
        }
    }
}

#[cfg(test)]
#[path = "learning/selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "learning/tutorial_tests.rs"]
mod tutorial_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};

    #[test]
    fn course_markdown_keeps_web_links_and_code_examples_without_opening_local_paths() {
        let markdown = "[基础诊断](lessons/01-prerequisites.md) 和 [**评分**](worksheets/rubric.md)\n\n[官网](https://example.org/guide)\n\n`[行内样例](example.md)`\n\n```python\nprint('[代码样例](example.md)')\n```\n\n[参考资料][ref]\n\n[ref]: references.md\n";
        let display = course_display_markdown(markdown);
        assert!(display.starts_with("基础诊断 和 评分\n"));
        assert!(display.contains("[官网](https://example.org/guide)"));
        assert!(display.contains("`[行内样例](example.md)`"));
        assert!(display.contains("print('[代码样例](example.md)')"));
        assert!(display.contains("\n参考资料\n"));
        let links: Vec<_> = pulldown_cmark::Parser::new(&display)
            .filter_map(|event| match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                    Some(dest_url.into_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(links, ["https://example.org/guide"]);
    }

    #[test]
    fn guide_and_its_supplementary_documents_use_the_course_source_files() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("courses/agent-foundations");
        for document in CourseDocument::ALL {
            assert_eq!(
                document.markdown(),
                std::fs::read_to_string(root.join(document.path())).unwrap()
            );
        }
        // All local reading links must resolve to bundled documents or one of
        // the original six lessons, not a file inaccessible from the desktop.
        let mut destinations = CourseDocument::ALL
            .iter()
            .map(|document| root.join(document.path()).canonicalize().unwrap())
            .collect::<std::collections::HashSet<_>>();
        for filename in [
            "01-prerequisites.md",
            "02-observe-loop.md",
            "03-build-manual.md",
            "04-recover-failures.md",
            "05-build-langgraph.md",
            "06-transfer-and-review.md",
        ] {
            destinations.insert(root.join("lessons").join(filename).canonicalize().unwrap());
        }
        for (index, document) in CourseDocument::CHAPTERS.iter().enumerate().skip(1) {
            let chapter = root.join(document.path());
            let exercise = chapter
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("exercises")
                .join(chapter.file_name().unwrap())
                .canonicalize()
                .unwrap();
            let lessons =
                moye_epub_editor::learning_records::lessons_for((index + 1) as u8).unwrap();
            assert_eq!(lessons.len(), 1);
            assert_eq!(
                lessons[0].markdown,
                std::fs::read_to_string(&exercise).unwrap()
            );
            destinations.insert(exercise);
        }
        for document in CourseDocument::ALL {
            let path = root.join(document.path());
            for event in pulldown_cmark::Parser::new(document.markdown()) {
                if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
                    dest_url, ..
                }) = event
                {
                    let destination = dest_url.split('#').next().unwrap();
                    if destination.starts_with("https://")
                        || destination.starts_with("http://")
                        || !destination.ends_with(".md")
                    {
                        continue;
                    }
                    let resolved = path
                        .parent()
                        .unwrap()
                        .join(destination)
                        .canonicalize()
                        .unwrap_or_else(|error| {
                            panic!("{} -> {destination}: {error}", document.path())
                        });
                    assert!(
                        destinations.contains(&resolved),
                        "missing desktop destination: {} -> {destination}",
                        document.path()
                    );
                }
            }
        }
        assert_eq!(
            StudyPane::default(),
            StudyPane::Document(CourseDocument::Tutorial)
        );
    }

    #[test]
    fn tutorial_chapters_keep_the_original_experiment_steps_separate() {
        let chapters = CourseDocument::CHAPTERS;
        assert_eq!(chapters.len(), 10);
        assert_eq!(chapters[0], CourseDocument::Overview);
        assert_eq!(chapters[9], CourseDocument::Capstone);
        for (index, chapter) in chapters.iter().enumerate() {
            assert_eq!(chapter.chapter_index(), Some(index));
            assert_eq!(chapters.iter().filter(|item| *item == chapter).count(), 1);
        }
        for material in CourseDocument::MATERIALS {
            assert_eq!(material.chapter_index(), None);
        }
        // Browsing the tutorial does not turn chapter numbers into persisted
        // first-chapter lesson indices or invalidate existing submissions.
        let original = LearningWorkspace {
            lesson_index: 5,
            ..Default::default()
        };
        assert!(original.validate().is_ok());
        assert!(
            LearningWorkspace {
                lesson_index: 6,
                ..original
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn python_highlighting_colors_python_tokens_in_both_themes() {
        register_python_highlighting();
        let config = LanguageRegistry::singleton()
            .language(CODE_LANGUAGE)
            .unwrap();
        assert_eq!(config.name.as_ref(), "python");
        let code = "# 读取资料\ndef research():\n    return '松果项目', 42\n";
        let mut highlighter = SyntaxHighlighter::new(CODE_LANGUAGE);
        highlighter.update(None, &code.into());
        for theme in [
            HighlightTheme::default_light(),
            HighlightTheme::default_dark(),
        ] {
            let styles = highlighter.styles(&(0..code.len()), &theme);
            for (token, capture) in [
                ("# 读取资料", "comment"),
                ("def", "keyword"),
                ("research", "function"),
                ("return", "keyword"),
                ("松果项目", "string"),
                ("42", "number"),
            ] {
                let start = code.find(token).unwrap();
                let expected = theme.style.syntax.style(capture).unwrap().color;
                assert!(expected.is_some());
                assert!(
                    styles.iter().any(|(range, style)| range.start <= start
                        && range.end >= start + token.len()
                        && style.color == expected),
                    "missing {capture} color for {token}: {styles:?}"
                );
            }
        }
    }

    #[test]
    fn python_highlighting_reclassifies_code_after_editing_to_a_comment() {
        register_python_highlighting();
        let mut highlighter = SyntaxHighlighter::new(CODE_LANGUAGE);
        let theme = HighlightTheme::default_light();
        let original = "return '正文'\n";
        highlighter.update(None, &original.into());
        let edited = "# return '正文'\n";
        highlighter.update(None, &edited.into());
        let styles = highlighter.styles(&(0..edited.len()), &theme);
        assert!(styles.iter().any(|(range, style)| range.start == 0
            && range.end >= edited.trim_end().len()
            && style.color == theme.style.syntax.style("comment").unwrap().color));
        assert!(!styles.iter().any(|(_, style)| style.color == theme.style.syntax.style("keyword").unwrap().color));
    }

    #[test]
    fn close_waits_for_execution_and_persistence_before_removal() {
        assert_eq!(close_action(true, false, true), CloseAction::Wait);
        assert_eq!(close_action(false, true, false), CloseAction::Wait);
        assert_eq!(close_action(false, false, true), CloseAction::Save);
        assert_eq!(close_action(false, false, false), CloseAction::Remove);
    }

    #[test]
    fn imported_pass_is_always_presented_as_unverified() {
        assert_eq!(run_label("completed", true, true), "导入记录 · 待核验");
        assert_eq!(run_label("completed", true, false), "本次运行检查通过");
        assert_eq!(run_label("cancelled", false, false), "本次运行已取消");
    }

    #[test]
    fn restored_future_or_invalid_times_are_not_claimed_as_recent() {
        assert_eq!(relative_record_time("120", 100), "记录时间待核验");
        assert_eq!(relative_record_time("not-a-date", 100), "记录时间待核验");
        assert_eq!(relative_record_time("99", 100), "刚刚");
        assert_eq!(relative_record_time("1", 121), "2 分钟前");
    }

    #[test]
    fn output_cannot_escape_its_markdown_code_fence() {
        let output = "工具输出\n```\n![image](https://example.invalid/image)\n```";
        let fenced = fenced_text(output, "text");
        assert!(fenced.starts_with("````text\n"));
        assert!(fenced.ends_with("\n````"));
        assert!(fenced.contains(output));
        assert_eq!(short_text("资料读取完成", 4), "资料读取…");
    }
}
