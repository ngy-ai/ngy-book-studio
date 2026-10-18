//! Construction and the data-directory change flow.

use super::*;

impl SystemSettingsWindow {
    pub(crate) fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompt_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .default_value(settings.selection_explanation_question.clone())
                .placeholder("选中文本后点「AI 解释」时自动填入的问题")
        });
        Self {
            services,
            prompt_input,
            pdf_compact_reading: settings.pdf_compact_reading,
            appearance: settings.appearance,
            default_language: settings.default_language.clone(),
            translation_display_mode: settings.translation_display_mode,
            use_proxy: settings.use_proxy,
            speech_voice_id: settings.speech_voice_id.clone(),
            speech_rate: settings.speech_rate,
            // 系统语音列表读一次即可：枚举很快，但没必要每次渲染都问合成器。
            // 读不到就当成「本机没有可用语音」，界面据此禁用朗读控件。
            speech_voices: ngy_book_studio::speech::platform_engine()
                .voices()
                .unwrap_or_default(),
            state: SettingsWindowState::new(window),
            choosing_data_directory: false,
            confirm_overwrite: None,
        }
    }

    /// 以已保存设置为底，只覆盖本窗口负责的偏好。
    pub(crate) fn entered_settings(&self, cx: &App) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        settings.selection_explanation_question = self.prompt_input.read(cx).value().to_string();
        settings.pdf_compact_reading = self.pdf_compact_reading;
        settings.appearance = self.appearance;
        settings.default_language = self.default_language.clone();
        settings.translation_display_mode = self.translation_display_mode;
        settings.use_proxy = self.use_proxy;
        settings.speech_voice_id = self.speech_voice_id.clone();
        settings.speech_rate = self.speech_rate;
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn set_default_language(
        &mut self,
        language: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.default_language = language;
        cx.notify();
    }

    pub(crate) fn set_translation_display_mode(
        &mut self,
        mode: TranslationDisplayMode,
        cx: &mut Context<Self>,
    ) {
        self.translation_display_mode = mode;
        cx.notify();
    }

    pub(crate) fn set_speech_voice(&mut self, voice_id: String, cx: &mut Context<Self>) {
        self.speech_voice_id = voice_id;
        cx.notify();
    }

    pub(crate) fn set_speech_rate(&mut self, rate: f32, cx: &mut Context<Self>) {
        self.speech_rate = rate;
        cx.notify();
    }

    /// 试听当前选择：合成一句话并播放到底。
    ///
    /// 合成与播放都在后台线程（会占用音频输出），界面只等一个结果提示；试听会
    /// 打断正在进行的朗读——它是应用级播放资源，两者不能同时出声。因此在交给
    /// 后台线程之前先停掉登记表里的朗读会话，而不是只提示用户。
    pub(crate) fn preview_speech(&mut self, cx: &mut Context<Self>) {
        crate::ui::stop_any_read_aloud(cx);
        let voice_id = self.speech_voice_id.clone();
        let rate = self.speech_rate;
        let preview = cx
            .background_executor()
            .spawn(async move { ngy_book_studio::speech::preview(&voice_id, rate) });
        cx.spawn(async move |view, cx| {
            let outcome = preview.await;
            let _ = view.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => this.state.report("试听完成。".to_string(), false),
                    Err(error) => this.state.report(format!("试听失败：{error:#}"), true),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 更改数据目录：选新位置、校验、记录，然后重启墨页完成搬迁。
    ///
    /// 运行中的图书库、对象存储和日志都绑在旧目录上，就地搬走会让当前进程立刻失效，
    /// 所以这里只记录选择：真正的搬迁由重启后的新进程在打开图书库**之前**完成
    /// （`startup::complete_pending_move`）。重启不是可选项 —— 不重启，本次会话会继续
    /// 往旧目录里写新导入的书。
    pub(crate) fn change_data_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choosing_data_directory {
            return;
        }
        let config_path = match startup::bootstrap_path() {
            Ok(path) => path,
            Err(error) => {
                self.state
                    .report(format!("无法定位目录设置文件：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        let from = self.services.data_dir().to_path_buf();
        // 原生对话框在本次实体更新返回之后再显示：Windows 文件对话框跑自己的消息
        // 循环，在 GPUI 持有 App 借用时弹出会让随后每一帧都 BorrowMutError。
        let dialog = DialogBuilder::file()
            .set_owner(window)
            .set_title("选择数据目录")
            .set_location(&startup::shell_dialog_directory(&from))
            .open_single_dir();
        self.choosing_data_directory = true;
        self.confirm_overwrite = None;
        cx.spawn_in(window, async move |view, cx| {
            let picked = dialog.show();
            let _ = view.update(cx, |this, cx| {
                this.choosing_data_directory = false;
                match picked {
                    Ok(Some(target)) => this.prepare_data_directory_change(
                        DataDirChange {
                            target,
                            config_path,
                            from,
                        },
                        cx,
                    ),
                    // 取消：保留原来的提示，不假装改过。
                    Ok(None) => cx.notify(),
                    Err(error) => {
                        this.state
                            .report(format!("无法打开目录选择器：{error}"), true);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// 校验选中的新目录：空目录直接提交，已经是一个图书库时先要一次覆盖确认。
    pub(crate) fn prepare_data_directory_change(
        &mut self,
        change: DataDirChange,
        cx: &mut Context<Self>,
    ) {
        let prepared = cx.background_executor().spawn({
            let target = change.target.clone();
            // 建目录、探针写删都在后台：用户可能选到网络盘。
            async move {
                startup::ensure_data_dir_usable(&target)?;
                Ok::<_, anyhow::Error>(startup::holds_a_library(&target))
            }
        });
        cx.spawn(async move |view, cx| {
            let outcome = prepared.await;
            let _ = view.update(cx, |this, cx| match outcome {
                // 目标里已经有一个图书库：覆盖不可逆，先问清楚。
                Ok(true) => {
                    this.confirm_overwrite = Some(change);
                    this.state.notice = None;
                    cx.notify();
                }
                Ok(false) => this.commit_data_directory_change(change, false, cx),
                Err(error) => {
                    this.state
                        .report(format!("无法使用该目录：{error:#}"), true);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 写下引导配置并重启墨页；搬迁交给重启后的新进程。
    pub(crate) fn commit_data_directory_change(
        &mut self,
        change: DataDirChange,
        overwrite: bool,
        cx: &mut Context<Self>,
    ) {
        let applied = cx.background_executor().spawn({
            let (config_path, target, from) = (
                change.config_path.clone(),
                change.target.clone(),
                change.from.clone(),
            );
            async move { startup::apply_data_dir_change(&config_path, &target, &from, overwrite) }
        });
        self.choosing_data_directory = true;
        self.confirm_overwrite = None;
        self.state.report(
            format!(
                "正在把数据目录切换到 {}，随后墨页会重新启动并把原目录的内容搬过去…",
                change.target.display()
            ),
            false,
        );
        cx.notify();

        cx.spawn(async move |view, cx| {
            let outcome = applied.await;
            let _ = view.update(cx, |this, cx| {
                this.choosing_data_directory = false;
                match outcome {
                    // 记录已经落盘：重启后新进程会把旧目录的内容搬过来。
                    Ok(_) => this.restart_application(&change.target, cx),
                    Err(error) => {
                        this.state
                            .report(format!("无法使用该目录：{error:#}"), true);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 重启墨页。搬迁和切换数据目录都要重新打开图书库，只能靠重启。
    pub(crate) fn restart_application(&mut self, data_dir: &Path, cx: &mut Context<Self>) {
        let spawned = std::env::current_exe()
            .map_err(|error| error.to_string())
            .and_then(|executable| {
                std::process::Command::new(&executable)
                    .spawn()
                    .map(|_| ())
                    .map_err(|error| format!("{}：{error}", executable.display()))
            });
        match spawned {
            Ok(()) => cx.quit(),
            Err(error) => {
                self.state.report(
                    format!(
                        "已把数据目录记为 {}。请手动关闭墨页再重新打开：新目录会在下次启动时\
                         启用，原目录的内容也在那时搬过去。\n\n自动重启失败：{error}",
                        data_dir.display()
                    ),
                    true,
                );
                cx.notify();
            }
        }
    }

    /// 用系统默认程序打开目录，出问题只提示不打断。
    pub(crate) fn open_directory(&mut self, path: PathBuf, label: &str, cx: &mut Context<Self>) {
        #[cfg(target_os = "windows")]
        {
            if let Err(error) = open::that_detached(&path) {
                self.state.report(format!("无法打开{label}：{error}"), true);
                cx.notify();
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (path, label);
            self.state
                .report(format!("当前平台不支持在文件管理器中打开{label}。"), true);
            cx.notify();
        }
    }
}
