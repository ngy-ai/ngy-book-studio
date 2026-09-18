//! Construction, the editor inputs and loading a chapter snapshot.

use super::*;

impl LearningWindow {
    pub(super) fn create_inputs(
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

    pub(super) fn replace_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn new(
        service: Arc<LearningService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

    pub(super) fn on_input_event(
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

    pub(super) fn busy(&self) -> bool {
        self.operation.is_some() || self.running || self.closing
    }

    pub(super) fn inputs_disabled(&self) -> bool {
        self.busy() || self.reload_required || self.saved_workspace.is_none()
    }

    pub(super) fn chapter_entry_disabled(&self) -> bool {
        // A chapter that never loaded has no editable work to lose. Permit
        // leaving it or opening its restore controls, while a loaded workspace
        // with an uncertain disk revision must be reconciled first.
        self.busy() || (self.reload_required && self.saved_workspace.is_some())
    }

    pub(super) fn entered_workspace(&self, cx: &App) -> Option<LearningWorkspace> {
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

    pub(super) fn dirty(&self, cx: &App) -> bool {
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

    pub(super) fn apply_snapshot(
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

    pub(super) fn clear_unloaded_chapter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(LearningNotice { text, error: true });
        if self.closing {
            cancel_application_exit(cx);
            self.closing = false;
            cx.global_mut::<LearningWindowTracker>().closing = false;
        }
        cx.notify();
    }

    pub(super) fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}
