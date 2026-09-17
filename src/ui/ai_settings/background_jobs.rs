//! 后台任务设置窗口：新任务的运行方式与调度节奏。
//!
//! 只管调度，不管运行明细 —— 任务列表、暂停/恢复/取消在「后台任务」窗口里。
//! 保存时以已保存的 `ProviderSettings` 为底，只覆盖这三个字段。

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_component::checkbox::Checkbox;
use ngy_book_studio::services::{
    ApiKeyUpdate, DEFAULT_BACKGROUND_JOB_CONCURRENCY, DEFAULT_BACKGROUND_JOB_INTERVAL_MS,
    MAX_BACKGROUND_JOB_CONCURRENCY, MAX_BACKGROUND_JOB_INTERVAL_MS, MIN_BACKGROUND_JOB_CONCURRENCY,
    ProviderSettings,
};

pub(super) struct BackgroundJobsSettingsWindow {
    services: Arc<AppServices>,
    auto_run_background_jobs: bool,
    background_job_concurrency_input: Entity<InputState>,
    background_job_interval_input: Entity<InputState>,
    state: SettingsWindowState,
}

impl BackgroundJobsSettingsWindow {
    pub(super) fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let background_job_concurrency_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.background_job_concurrency.to_string())
                .placeholder(DEFAULT_BACKGROUND_JOB_CONCURRENCY.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        let background_job_interval_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.background_job_interval_ms.to_string())
                .placeholder(DEFAULT_BACKGROUND_JOB_INTERVAL_MS.to_string())
                .validate(|value, _| value.chars().all(|character| character.is_ascii_digit()))
        });
        Self {
            services,
            auto_run_background_jobs: settings.auto_run_background_jobs,
            background_job_concurrency_input,
            background_job_interval_input,
            state: SettingsWindowState::new(window),
        }
    }

    /// 以已保存设置为底，只覆盖运行方式与调度字段。
    fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        settings.auto_run_background_jobs = self.auto_run_background_jobs;
        settings.background_job_concurrency = parse_background_job_concurrency(
            self.background_job_concurrency_input
                .read(cx)
                .value()
                .as_ref(),
        )?;
        settings.background_job_interval_ms = parse_background_job_interval_ms(
            self.background_job_interval_input.read(cx).value().as_ref(),
        )?;
        settings.validate()?;
        Ok(settings)
    }

    #[inline(never)]
    fn render_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let auto_run_view = cx.entity();
        let busy = self.state.busy();
        div()
            .v_flex()
            .gap_5()
            .child(panel(
                theme,
                vec![
                    card_title(theme, "新任务运行方式"),
                    card_note(
                        theme,
                        "导入或编辑图书后，墨页会创建逻辑页面渲染、视觉理解和向量索引任务。",
                    ),
                    Checkbox::new("ai-auto-run-background-jobs")
                        .checked(self.auto_run_background_jobs)
                        .disabled(busy)
                        .label("自动运行新创建的后台任务")
                        .debug_selector(|| "ai-auto-run-background-jobs".into())
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            auto_run_view.update(cx, |this, cx| {
                                this.auto_run_background_jobs = checked;
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    card_note(
                        theme,
                        "关闭后，新导入、创建或编辑图书产生的三类任务会以暂停状态创建，可在「后台任务」窗口逐项恢复。更改此设置不会暂停、恢复或取消已经排队或运行的任务。",
                    ),
                ],
            ))
            .child(
                div()
                    .debug_selector(|| "ai-background-job-scheduling".into())
                    .v_flex()
                    .gap_3()
                    .p_4()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .bg(rgb(theme.surface))
                    .child(card_title(theme, "后台任务调度"))
                    .child(render_input_field(
                        theme,
                        busy,
                        "任务并发",
                        "同时运行的模型任务数量，1–1024，默认 1；整本翻译也按它同时翻译多个文本块。并发越高占用的内存与网络越多；配置较低的机器建议保持 1。",
                        &self.background_job_concurrency_input,
                    ))
                    .child(render_input_field(
                        theme,
                        busy,
                        "任务间隔（毫秒）",
                        "一个任务完成后，休眠多久再开始下一个任务，0–60000 毫秒，默认 10。机器配置较差时增大该值可降低持续满载的风险。",
                        &self.background_job_interval_input,
                    ))
                    .child(card_note(
                        theme,
                        "保存后对后续任务立即生效，不需要重启；正在运行的任务不会被打断。",
                    )),
            )
            .into_any_element()
    }
}

impl SettingsWindowView for BackgroundJobsSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::BackgroundJobs
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().background_jobs
    }

    fn state(&self) -> &SettingsWindowState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SettingsWindowState {
        &mut self.state
    }

    fn services(&self) -> &Arc<AppServices> {
        &self.services
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let settings = match self.entered_settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.state
                    .report(format!("后台任务设置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            BTreeMap::new(),
            ApiKeyUpdate::Keep,
            None,
            window,
            cx,
        );
    }
}

impl Render for BackgroundJobsSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        settings_frame(self, body, None, cx)
    }
}

fn parse_background_job_concurrency(value: &str) -> Result<usize> {
    let value = value.trim();
    let concurrency = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!(
            "后台任务并发必须是 {MIN_BACKGROUND_JOB_CONCURRENCY} 到 {MAX_BACKGROUND_JOB_CONCURRENCY} 之间的整数"
        )
    })?;
    anyhow::ensure!(
        (MIN_BACKGROUND_JOB_CONCURRENCY..=MAX_BACKGROUND_JOB_CONCURRENCY).contains(&concurrency),
        "后台任务并发必须是 {MIN_BACKGROUND_JOB_CONCURRENCY} 到 {MAX_BACKGROUND_JOB_CONCURRENCY} 之间的整数"
    );
    Ok(concurrency)
}

fn parse_background_job_interval_ms(value: &str) -> Result<u64> {
    let value = value.trim();
    let interval_ms = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("后台任务间隔必须是 0 到 {MAX_BACKGROUND_JOB_INTERVAL_MS} 之间的整数毫秒")
    })?;
    anyhow::ensure!(
        interval_ms <= MAX_BACKGROUND_JOB_INTERVAL_MS,
        "后台任务间隔必须是 0 到 {MAX_BACKGROUND_JOB_INTERVAL_MS} 之间的整数毫秒"
    );
    Ok(interval_ms)
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use gpui::{Modifiers, TestAppContext};

    #[test]
    fn background_job_scheduling_parsing_enforces_supported_ranges() {
        assert_eq!(
            parse_background_job_concurrency(" 1 ").unwrap(),
            MIN_BACKGROUND_JOB_CONCURRENCY
        );
        assert_eq!(
            parse_background_job_concurrency(&MAX_BACKGROUND_JOB_CONCURRENCY.to_string()).unwrap(),
            MAX_BACKGROUND_JOB_CONCURRENCY
        );
        let over_max = (MAX_BACKGROUND_JOB_CONCURRENCY + 1).to_string();
        for invalid in ["", "0", "-1", "1.5", "abc", over_max.as_str()] {
            assert!(
                parse_background_job_concurrency(invalid).is_err(),
                "invalid concurrency unexpectedly accepted: {invalid}"
            );
        }

        assert_eq!(parse_background_job_interval_ms("0").unwrap(), 0);
        assert_eq!(parse_background_job_interval_ms(" 10 ").unwrap(), 10);
        assert_eq!(
            parse_background_job_interval_ms(&MAX_BACKGROUND_JOB_INTERVAL_MS.to_string()).unwrap(),
            MAX_BACKGROUND_JOB_INTERVAL_MS
        );
        for invalid in ["", "-1", "1.5", "abc", "60001"] {
            assert!(
                parse_background_job_interval_ms(invalid).is_err(),
                "invalid interval unexpectedly accepted: {invalid}"
            );
        }
    }

    #[gpui::test]
    fn scheduling_drafts_parse_without_saving(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_settings_view::<BackgroundJobsSettingsWindow>(
            cx,
            BackgroundJobsSettingsWindow::new,
        );
        assert!(
            visual
                .debug_bounds("ai-background-job-scheduling")
                .is_some(),
            "调度卡片必须渲染在后台任务窗口里"
        );
        let auto_run = visual
            .debug_bounds("ai-auto-run-background-jobs")
            .expect("auto-run checkbox must be rendered");
        visual.simulate_click(auto_run.center(), Modifiers::none());
        redraw(visual);
        let (concurrency, interval) = settings.read_with(visual, |view, _| {
            (
                view.background_job_concurrency_input.clone(),
                view.background_job_interval_input.clone(),
            )
        });
        edit_input(visual, &concurrency, "0");
        settings.read_with(visual, |view, cx| {
            assert!(
                view.entered_settings(cx).is_err(),
                "out-of-range concurrency must block saving"
            );
        });

        edit_input(visual, &concurrency, "3");
        edit_input(visual, &interval, "250");
        settings.read_with(visual, |view, cx| {
            let entered = view.entered_settings(cx).unwrap();
            assert!(entered.auto_run_background_jobs);
            assert_eq!(entered.background_job_concurrency, 3);
            assert_eq!(entered.background_job_interval_ms, 250);
            let saved = view.services.provider_settings().unwrap();
            assert!(!saved.auto_run_background_jobs);
            assert_eq!(
                saved.background_job_concurrency,
                DEFAULT_BACKGROUND_JOB_CONCURRENCY
            );
            assert_eq!(
                saved.background_job_interval_ms,
                DEFAULT_BACKGROUND_JOB_INTERVAL_MS
            );
        });
    }
}
