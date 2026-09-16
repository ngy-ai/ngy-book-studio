//! 数据目录设置窗口。
//!
//! 首次启动，或上次记录的数据目录不可用时，启动流程不再直接弹系统对话框，而是先开
//! 这个窗口：输入框里放好推荐路径（或上次的选择），用户可以直接改、可以点「浏览…」
//! 用系统对话框选，点「确认并启动」后才校验并记录下来。
//!
//! 校验与写配置在后台执行器上跑（选中的可能是网络盘），失败时把原因显示在同一个
//! 窗口里让用户改，而不是弹原生对话框；图书库打不开也回到这里（[`Self::report_failure`]）。
//! 因此这个窗口是整个启动过程唯一的交互界面：只有它的「确认并启动」成功，主窗口才会出现。

use super::*;
use ngy_book_studio::startup::{self, DataDirSelection, ReselectReason};

/// 用户确认目录后由启动流程接手。`WeakEntity` 是设置窗口自己，启动失败时用它把原因
/// 显示回窗口。
pub(crate) type ConfirmHandler =
    Rc<dyn Fn(DataDirSelection, &mut Window, &mut App, WeakEntity<DataDirSetupWindow>)>;

/// 数据目录设置窗口的实体。
pub(crate) struct DataDirSetupWindow {
    input: Entity<InputState>,
    /// 引导配置的路径，由启动流程给出：确认后把目录记在这里。
    config_path: PathBuf,
    reason: ReselectReason,
    /// 校验或启动失败的原因，直接显示在窗口里。
    error: Option<String>,
    /// 已提交：正在校验目录、写配置或打开图书库。此时不接受第二次提交，也不允许关窗。
    launching: bool,
    on_confirm: ConfirmHandler,
    _input_subscription: Subscription,
}

impl DataDirSetupWindow {
    fn new(
        config_path: PathBuf,
        reason: ReselectReason,
        suggestion: PathBuf,
        on_confirm: ConfirmHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(suggestion.display().to_string())
                .placeholder("例如 D:\\墨页书库")
        });
        // 回车等同于点「确认并启动」：输入路径后不必再去够鼠标。
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary: false }) {
                this.confirm(window, cx);
            }
        });
        Self {
            input,
            config_path,
            reason,
            error: None,
            launching: false,
            on_confirm,
            _input_subscription: subscription,
        }
    }

    fn heading(&self) -> &'static str {
        match self.reason {
            ReselectReason::FirstRun => "选择数据目录",
            ReselectReason::MoveFailed { .. } => "数据目录搬迁未完成",
            _ => "数据目录需要重新确认",
        }
    }

    fn explanation(&self) -> String {
        match &self.reason {
            ReselectReason::FirstRun => {
                "这是第一次启动墨页。下面是推荐位置，可以直接修改，或点「浏览…」选择其它位置。"
                    .to_string()
            }
            ReselectReason::Unusable(error) => format!(
                "上次使用的数据目录现在无法使用，已把它填在下面，请修改后重试。\n\n原因：{error}"
            ),
            ReselectReason::Unreadable(error) => {
                format!("无法读取上次记录的目录设置，请确认下面的位置是否正确。\n\n原因：{error}")
            }
            ReselectReason::MoveFailed { from, error } => format!(
                "更改数据目录后，原数据目录的内容要搬到新位置，但搬迁没有完成 —— 旧数据仍在原处。\n\n\
                 原数据目录：{}\n\n原因：{error}\n\n\
                 确认下面同一个位置可以重试；换成一个空的目录也可以。",
                from.display()
            ),
        }
    }

    /// 校验输入框里的目录并记录，成功则交回启动流程。
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.launching {
            return;
        }
        let Some(directory) = startup::normalize_input_path(self.input.read(cx).value().as_ref())
        else {
            self.fail("请填写数据目录路径。".to_string(), cx);
            return;
        };
        let config_path = self.config_path.clone();

        self.launching = true;
        self.error = None;
        cx.notify();

        let on_confirm = Rc::clone(&self.on_confirm);
        cx.spawn_in(window, async move |view, cx| {
            // 建目录、探针写删和配置落盘都在后台执行器上做：用户可能选到网络盘。
            let prepared = cx
                .background_executor()
                .spawn({
                    let directory = directory.clone();
                    async move { startup::apply_data_dir(&config_path, &directory) }
                })
                .await;
            match prepared {
                Ok(selection) => {
                    let _ = cx.update(|window, cx| on_confirm(selection, window, cx, view.clone()));
                }
                Err(error) => {
                    let _ = view.update(cx, |this, cx| {
                        this.fail(format!("无法使用该目录：{error:#}"), cx);
                    });
                }
            }
        })
        .detach();
    }

    /// 打开系统目录选择器，把选中的目录填进输入框。
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.launching {
            return;
        }
        // 原生对话框显示在本次实体更新返回之后再执行：Windows 文件对话框跑自己的消息
        // 循环，在 GPUI 持有 App 借用时弹出会让随后每一帧都 BorrowMutError。
        //
        // 初始位置只取自存在的目录：首次启动时推荐路径还没建出来，Shell 解析不存在的
        // 路径会让整个对话框打不开（`SHCreateItemFromParsingName`）。
        let typed = startup::normalize_input_path(self.input.read(cx).value().as_ref());
        let mut builder = DialogBuilder::file()
            .set_owner(window)
            .set_title("选择数据目录");
        if let Some(start) = startup::dialog_start_directory(typed.as_deref()) {
            builder = builder.set_location(&start);
        }
        let dialog = builder.open_single_dir();

        cx.spawn_in(window, async move |view, cx| {
            let picked = dialog.show();
            // 取回窗口句柄才能改输入框内容，所以用异步上下文自己的 update。
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| match picked {
                    Ok(Some(directory)) => {
                        let text = directory.display().to_string();
                        this.error = None;
                        this.input
                            .update(cx, |input, cx| input.set_value(text, window, cx));
                        cx.notify();
                    }
                    // 取消：保持输入框原样，也不清除已有的错误提示。
                    Ok(None) => {}
                    Err(error) => {
                        this.error = Some(format!("无法打开目录选择器：{error}"));
                        cx.notify();
                    }
                });
            });
        })
        .detach();
    }

    /// 由启动流程调用：用 `message` 说明这次为什么没能启动。
    pub(crate) fn report_failure(&mut self, message: String, cx: &mut Context<Self>) {
        self.fail(message, cx);
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.launching = false;
        self.error = Some(message);
        cx.notify();
    }

    fn is_launching(&self) -> bool {
        self.launching
    }

    fn render_error(&self) -> Option<gpui::AnyElement> {
        let error = self.error.as_ref()?;
        Some(
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .bg(rgb(0xf7e1df))
                .text_sm()
                .text_color(rgb(0x9f302c))
                .child(Icon::new(IconName::TriangleAlert).small())
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .line_height(gpui::relative(1.5))
                        .child(error.clone()),
                )
                .into_any_element(),
        )
    }
}

impl Render for DataDirSetupWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let launching = self.launching;
        div()
            .size_full()
            .bg(rgb(PAPER))
            .v_flex()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(600.))
                    .v_flex()
                    .gap_4()
                    .p_6()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .text_color(rgb(INK))
                                    .child(self.heading()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .line_height(gpui::relative(1.5))
                                    .text_color(rgb(MUTED))
                                    .child(self.explanation()),
                            ),
                    )
                    .child(
                        div()
                            .v_flex()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(INK))
                                    .child("数据目录"),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(Input::new(&self.input).disabled(launching)),
                                    )
                                    .child(
                                        Button::new("data-dir-setup-browse")
                                            .label("浏览…")
                                            .outline()
                                            .disabled(launching)
                                            .debug_selector(|| "data-dir-setup-browse".into())
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.browse(window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .line_height(gpui::relative(1.5))
                            .text_color(rgb(MUTED))
                            .child(
                                "图书、笔记、索引和日志都放在这里，建议放在非系统盘；日志写在它的 \
                                 logs/ 子目录下，按 UTC 日期分文件、保留最近 7 天。确认后立即使用，\
                                 下次启动不再询问，之后可以在「系统配置」里查看或更改。",
                            ),
                    )
                    .children(self.render_error())
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("data-dir-setup-quit")
                                    .label("退出墨页")
                                    .outline()
                                    .disabled(launching)
                                    .debug_selector(|| "data-dir-setup-quit".into())
                                    .on_click(cx.listener(|_, _, _, cx| cx.quit())),
                            )
                            .child(
                                Button::new("data-dir-setup-confirm")
                                    .label(if launching {
                                        "正在打开图书库…"
                                    } else {
                                        "确认并启动"
                                    })
                                    .primary()
                                    .disabled(launching)
                                    .debug_selector(|| "data-dir-setup-confirm".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

/// 打开设置窗口。`config_path` 是记录选择的位置，`on_confirm` 收到校验通过的数据目录后
/// 负责启动主界面。
pub(crate) fn open_data_dir_setup_window(
    cx: &mut App,
    config_path: PathBuf,
    reason: ReselectReason,
    suggestion: PathBuf,
    on_confirm: impl Fn(DataDirSelection, &mut Window, &mut App, WeakEntity<DataDirSetupWindow>)
    + 'static,
) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(660.), px(430.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(540.), px(380.))),
            titlebar: Some(TitlebarOptions {
                title: Some("墨页 · 设置数据目录".into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio.data-dir".to_string()),
            ..Default::default()
        },
        move |window, cx| {
            let setup = cx.new(|cx| {
                DataDirSetupWindow::new(
                    config_path,
                    reason,
                    suggestion,
                    Rc::new(on_confirm),
                    window,
                    cx,
                )
            });
            let close_view = setup.downgrade();
            super::on_window_close(window, cx, move |_, cx| {
                // 已提交、正在校验或正在打开图书库：不响应关闭，否则后台工作回来时
                // 汇报对象已经没了。
                if close_view
                    .upgrade()
                    .is_some_and(|view| view.read(cx).is_launching())
                {
                    return false;
                }
                // 引导阶段没有别的窗口：关掉设置窗口就是放弃启动。
                cx.quit();
                false
            });
            cx.new(|cx| Root::new(setup, window, cx))
        },
    )
    .map(|_| ())
    .context("无法创建设置数据目录窗口")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use ngy_book_studio::startup::load_remembered_data_dir;
    use std::cell::RefCell;
    use std::fs;

    type Confirmed = Rc<RefCell<Option<DataDirSelection>>>;

    /// 打开设置窗口，并把确认回调记下的结果交回测试。
    fn open_setup<'a>(
        cx: &'a mut TestAppContext,
        config_path: PathBuf,
        suggestion: PathBuf,
    ) -> (
        Confirmed,
        Entity<DataDirSetupWindow>,
        &'a mut VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        let confirmed: Confirmed = Rc::new(RefCell::new(None));
        let captured = Rc::clone(&confirmed);
        let mut setup_view = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let on_confirm: ConfirmHandler = Rc::new(move |selection, _, _, _| {
                    *captured.borrow_mut() = Some(selection);
                });
                DataDirSetupWindow::new(
                    config_path,
                    ReselectReason::FirstRun,
                    suggestion,
                    on_confirm,
                    window,
                    cx,
                )
            });
            setup_view = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(700.), px(460.)));
        visual.run_until_parked();
        (confirmed, setup_view.unwrap(), visual)
    }

    fn set_input(visual: &mut VisualTestContext, view: &Entity<DataDirSetupWindow>, value: String) {
        let input = view.read_with(visual, |view, _| view.input.clone());
        visual.update(|window, cx| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        });
        visual.run_until_parked();
    }

    fn click(visual: &mut VisualTestContext, selector: &'static str) {
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} 必须渲染出来"));
        assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
        visual.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        visual.simulate_click(bounds.center(), Modifiers::none());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn the_input_starts_with_the_suggested_directory_and_confirm_saves_the_edit(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().expect("临时目录");
        let config_path = temp.path().join("bootstrap.json");
        let suggested = temp.path().join("推荐位置");
        let (confirmed, view, visual) = open_setup(cx, config_path.clone(), suggested.clone());

        // 输入框里预填推荐路径，用户可以直接改。
        let shown = view.read_with(visual, |view, cx| view.input.read(cx).value().to_string());
        assert_eq!(shown, suggested.display().to_string());

        let chosen = temp.path().join("用户改过的位置");
        set_input(visual, &view, chosen.display().to_string());
        click(visual, "data-dir-setup-confirm");

        assert_eq!(
            confirmed
                .borrow()
                .as_ref()
                .map(|selection| selection.data_dir.clone()),
            Some(chosen.clone())
        );
        assert_eq!(
            load_remembered_data_dir(&config_path)
                .expect("读配置")
                .as_deref(),
            Some(chosen.as_path())
        );
        view.read_with(visual, |view, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            assert!(view.launching, "确认后进入启动流程");
        });
    }

    #[gpui::test]
    fn an_unusable_directory_keeps_the_window_open_for_another_try(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("临时目录");
        let config_path = temp.path().join("bootstrap.json");
        let blocked = temp.path().join("占位文件");
        fs::write(&blocked, b"x").expect("写文件");
        let (confirmed, view, visual) =
            open_setup(cx, config_path.clone(), temp.path().join("推荐位置"));

        set_input(visual, &view, blocked.display().to_string());
        click(visual, "data-dir-setup-confirm");

        assert!(confirmed.borrow().is_none(), "不可用的目录不能进入启动流程");
        assert_eq!(
            load_remembered_data_dir(&config_path).expect("读配置"),
            None
        );
        view.read_with(visual, |view, _| {
            assert!(
                view.error
                    .as_ref()
                    .is_some_and(|error| error.contains("无法使用该目录")),
                "{:?}",
                view.error
            );
            assert!(!view.launching, "失败后要能重新输入");
        });
    }

    #[gpui::test]
    fn an_empty_input_is_rejected_without_saving(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("临时目录");
        let config_path = temp.path().join("bootstrap.json");
        let (confirmed, view, visual) =
            open_setup(cx, config_path.clone(), temp.path().join("推荐位置"));

        set_input(visual, &view, "   ".to_string());
        click(visual, "data-dir-setup-confirm");

        assert!(confirmed.borrow().is_none());
        assert_eq!(
            load_remembered_data_dir(&config_path).expect("读配置"),
            None
        );
        view.read_with(visual, |view, _| {
            assert_eq!(view.error.as_deref(), Some("请填写数据目录路径。"));
        });
    }

    #[gpui::test]
    fn a_launch_failure_is_reported_in_the_same_window(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("临时目录");
        let config_path = temp.path().join("bootstrap.json");
        let (_, view, visual) = open_setup(cx, config_path, temp.path().join("推荐位置"));

        view.update(visual, |view, cx| {
            view.report_failure("无法打开图书库：库文件损坏".to_string(), cx);
        });
        visual.run_until_parked();

        view.read_with(visual, |view, _| {
            assert!(!view.launching);
            assert_eq!(view.error.as_deref(), Some("无法打开图书库：库文件损坏"));
        });
    }
}
