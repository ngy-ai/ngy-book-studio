//! 系统配置窗口：界面外观、阅读偏好、翻译默认与存储位置。
//!
//! 这里的偏好都**不是** Provider 配置，但和它们存在同一份 `ProviderSettings` 快照里，
//! 所以保存时同样以已保存设置为底、只覆盖本窗口的字段。界面外观、PDF 紧凑阅读与译文
//! 显示方式在提交成功后立刻应用到已经打开的窗口。

use super::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_component::checkbox::Checkbox;
use ngy_book_studio::services::{
    ApiKeyUpdate, ProviderSettings, TRANSLATION_LANGUAGES, TranslationDisplayMode,
    translation_language_label,
};
use ngy_book_studio::{logging, startup};

/// 一次待提交的数据目录更改：新位置、引导配置文件、当前（旧）数据目录。
#[derive(Clone)]
struct DataDirChange {
    target: PathBuf,
    config_path: PathBuf,
    from: PathBuf,
}

pub(super) struct SystemSettingsWindow {
    services: Arc<AppServices>,
    /// Global PDF reading preference: no gap between pages when enabled.
    pdf_compact_reading: bool,
    /// 界面皮肤。保存后才生效，与上面的阅读偏好同一套语义。
    appearance: Appearance,
    /// Target language for reading-time book translation. `None` disables it.
    default_language: Option<String>,
    /// How reading-time translations are displayed relative to the original text.
    translation_display_mode: TranslationDisplayMode,
    /// Whether AI and web-search requests go through the system proxy.
    use_proxy: bool,
    state: SettingsWindowState,
    /// 目录选择对话框正开着（或更改正在提交）。原生对话框在自己的消息循环里跑，
    /// 用它挡住重复点击。
    choosing_data_directory: bool,
    /// 选中的新目录里已经有一个图书库：等用户确认是否覆盖。
    confirm_overwrite: Option<DataDirChange>,
}

impl SystemSettingsWindow {
    pub(super) fn new(
        services: Arc<AppServices>,
        settings: ProviderSettings,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            services,
            pdf_compact_reading: settings.pdf_compact_reading,
            appearance: settings.appearance,
            default_language: settings.default_language.clone(),
            translation_display_mode: settings.translation_display_mode,
            use_proxy: settings.use_proxy,
            state: SettingsWindowState::new(window),
            choosing_data_directory: false,
            confirm_overwrite: None,
        }
    }

    /// 以已保存设置为底，只覆盖本窗口负责的偏好。
    fn entered_settings(&self) -> Result<ProviderSettings> {
        let mut settings = self.services.provider_settings()?;
        settings.pdf_compact_reading = self.pdf_compact_reading;
        settings.appearance = self.appearance;
        settings.default_language = self.default_language.clone();
        settings.translation_display_mode = self.translation_display_mode;
        settings.use_proxy = self.use_proxy;
        settings.validate()?;
        Ok(settings)
    }

    fn set_default_language(&mut self, language: Option<String>, cx: &mut Context<Self>) {
        self.default_language = language;
        cx.notify();
    }

    fn set_translation_display_mode(
        &mut self,
        mode: TranslationDisplayMode,
        cx: &mut Context<Self>,
    ) {
        self.translation_display_mode = mode;
        cx.notify();
    }

    /// 更改数据目录：选新位置、校验、记录，然后重启墨页完成搬迁。
    ///
    /// 运行中的图书库、对象存储和日志都绑在旧目录上，就地搬走会让当前进程立刻失效，
    /// 所以这里只记录选择：真正的搬迁由重启后的新进程在打开图书库**之前**完成
    /// （`startup::complete_pending_move`）。重启不是可选项 —— 不重启，本次会话会继续
    /// 往旧目录里写新导入的书。
    fn change_data_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn prepare_data_directory_change(&mut self, change: DataDirChange, cx: &mut Context<Self>) {
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
    fn commit_data_directory_change(
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
    fn restart_application(&mut self, data_dir: &Path, cx: &mut Context<Self>) {
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
    fn open_directory(&mut self, path: PathBuf, label: &str, cx: &mut Context<Self>) {
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

    #[inline(never)]
    fn render_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = palette(cx);
        let busy = self.state.busy();
        let compact_view = cx.entity();
        let language_view = cx.entity();
        let display_mode_view = cx.entity();
        let use_proxy_view = cx.entity();
        // 图书库自己记的是标准化路径（Windows 上是 `\\?\C:\…`），菜单、资源管理器和
        // 对话框都更认常规写法，所以显示与打开都用转换后的路径。
        let data_dir = startup::shell_dialog_directory(self.services.data_dir());
        let log_dir = startup::shell_dialog_directory(&logging::log_directory(&data_dir));

        let selected_language = self.default_language.clone();
        let current_label = selected_language
            .as_deref()
            .and_then(translation_language_label)
            .unwrap_or("不翻译（仅原文）");
        let language_button = Button::new("ai-default-language")
            .debug_selector(|| "ai-default-language".into())
            .outline()
            .label(current_label)
            .disabled(busy)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.item(
                    PopupMenuItem::new("不翻译（仅原文）")
                        .checked(selected_language.is_none())
                        .on_click({
                            let view = language_view.clone();
                            move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_default_language(None, cx);
                                });
                            }
                        }),
                );
                for (tag, label) in TRANSLATION_LANGUAGES {
                    let selected = selected_language.as_deref() == Some(tag);
                    let view = language_view.clone();
                    let tag = tag.to_string();
                    menu = menu.item(PopupMenuItem::new(label).checked(selected).on_click(
                        move |_, _, cx| {
                            let tag = tag.clone();
                            view.update(cx, |this, cx| {
                                this.set_default_language(Some(tag), cx);
                            });
                        },
                    ));
                }
                menu
            });

        let selected_mode = self.translation_display_mode;
        let display_mode_button = Button::new("ai-translation-display-mode")
            .debug_selector(|| "ai-translation-display-mode".into())
            .outline()
            .label(match selected_mode {
                TranslationDisplayMode::Bilingual => "双语",
                TranslationDisplayMode::OriginalOnly => "原文",
                TranslationDisplayMode::TranslationOnly => "译文",
            })
            .disabled(busy)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (mode, label) in [
                    (TranslationDisplayMode::TranslationOnly, "译文"),
                    (TranslationDisplayMode::Bilingual, "双语"),
                    (TranslationDisplayMode::OriginalOnly, "原文"),
                ] {
                    let view = display_mode_view.clone();
                    let selected = selected_mode == mode;
                    menu = menu.item(PopupMenuItem::new(label).checked(selected).on_click(
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.set_translation_display_mode(mode, cx);
                            });
                        },
                    ));
                }
                menu
            });

        // 外观是唯一会影响全部窗口（含已经打开的书）的偏好，所以排在系统面板最前面。
        let appearance_switch = TabBar::new("ai-appearance")
            .segmented()
            .selected_index(match self.appearance {
                Appearance::Light => 0,
                Appearance::Dark => 1,
            })
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                let index = (*index).min(Appearance::ALL.len() - 1);
                this.appearance = Appearance::ALL[index];
                cx.notify();
            }))
            .children(
                Appearance::ALL
                    .into_iter()
                    .map(|appearance| Tab::new().label(appearance.label())),
            );

        div()
            .v_flex()
            .gap_5()
            .child(panel(
                theme,
                vec![
                    card_title(theme, "界面外观"),
                    appearance_switch.into_any_element(),
                    card_note(
                        theme,
                        "浅色与深色两套皮肤覆盖全部窗口：图书库、阅读器、编辑器与 AI 侧栏。\
                         正文、逐章翻译、富文本编辑和 PDF 阅读一起换色，阅读进度、标注与笔记不受影响。\
                         保存后立即应用到已经打开的窗口。",
                    ),
                ],
            ))
            .child(panel(
                theme,
                vec![
                    card_title(theme, "PDF 阅读"),
                    Checkbox::new("ai-pdf-compact-reading")
                        .checked(self.pdf_compact_reading)
                        .disabled(busy)
                        .label("PDF 紧凑阅读")
                        .debug_selector(|| "ai-pdf-compact-reading".into())
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            compact_view.update(cx, |this, cx| {
                                this.pdf_compact_reading = checked;
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    card_note(
                        theme,
                        "开启后 PDF 阅读窗口的页面上下贴合，不再保留页间留白；关闭时恢复默认间距。\
                         页面投影、左右留白与阅读位置不变，标注与笔记仍按页面文字位置保存。",
                    ),
                ],
            ))
            .child(panel(
                theme,
                vec![
                    card_title(theme, "网络"),
                    Checkbox::new("ai-use-system-proxy")
                        .checked(self.use_proxy)
                        .disabled(busy)
                        .label("使用系统代理")
                        .debug_selector(|| "ai-use-system-proxy".into())
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            use_proxy_view.update(cx, |this, cx| {
                                this.use_proxy = checked;
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    card_note(
                        theme,
                        "开启后 AI 问答、联网搜索与后台翻译请求会走系统代理（环境变量 \
                         HTTP_PROXY / HTTPS_PROXY，NO_PROXY 里的地址仍然直连）。关闭后一律\
                         直连，本机模型端点（例如 127.0.0.1 上的 Ollama）不会被代理接管。\
                         保存后新的请求生效，已在进行的请求不受影响。",
                    ),
                ],
            ))
            .child(panel(
                theme,
                vec![
                    card_title(theme, "图书翻译"),
                    language_button.into_any_element(),
                    display_mode_button.into_any_element(),
                    card_note(
                        theme,
                        "选择语言后，导入或重新翻译的 EPUB、MOBI、AZW、AZW3、Word 图书会按文本块\
                        翻译为目标语言。“译文”只显示译文、隐藏原文，点击段落可在译文与双语之间\
                        切换，笔记仍锚定原文；“双语”则译文在上、原文在下同时显示；“原文”只显示\
                        原文（后台翻译照常进行）。这里是全局默认，阅读窗口可以按图书切换，\
                        图书自己的选择优先于它。选择“不翻译”则保持原文。",
                    ),
                ],
            ))
            .child(panel(
                theme,
                vec![
                    card_title(theme, "存储位置"),
                    storage_path_row("数据目录", &data_dir, theme),
                    storage_path_row("日志目录", &log_dir, theme),
                    self.render_confirm_overwrite(cx)
                        .unwrap_or_else(|| div().into_any_element()),
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            Button::new("ai-data-directory-change")
                                .label("更改数据目录…")
                                .outline()
                                .disabled(busy || self.choosing_data_directory)
                                .debug_selector(|| "ai-data-directory-change".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.change_data_directory(window, cx);
                                })),
                        )
                        .child({
                            let directory = data_dir.clone();
                            Button::new("ai-open-data-directory")
                                .label("打开数据目录")
                                .outline()
                                .debug_selector(|| "ai-open-data-directory".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_directory(directory.clone(), "数据目录", cx);
                                }))
                        })
                        .child({
                            let directory = log_dir.clone();
                            Button::new("ai-open-log-directory")
                                .label("打开日志目录")
                                .outline()
                                .debug_selector(|| "ai-open-log-directory".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_directory(directory.clone(), "日志目录", cx);
                                }))
                        })
                        .into_any_element(),
                    card_note(
                        theme,
                        "图书库、笔记、索引、对象存储和日志都放在数据目录里；首次启动时由你选择，\
                         记录保存在用户配置目录下的 bootstrap.json。更换位置时整个数据目录会被搬到\
                         新位置（旧目录随之清空），墨页会自动重启完成搬迁 —— 搬迁在打开图书库之前\
                         进行，所以本次会话不会再有图书写进旧目录。备份或迁移时直接复制整个数据\
                         目录即可。日志按 UTC 日期分文件，保留最近 7 天。",
                    ),
                ],
            ))
            .into_any_element()
    }

    /// 目标目录里已经有一个图书库时，先在这里要一次明确的覆盖确认。
    fn render_confirm_overwrite(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let change = self.confirm_overwrite.clone()?;
        let theme = palette(cx);
        Some(
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .bg(rgb(theme.danger_soft))
                .child(
                    div()
                        .text_sm()
                        .line_height(gpui::relative(1.5))
                        .text_color(rgb(theme.danger))
                        .child(format!(
                            "{} 已经是一个图书库。继续会先删掉它的内容，再把当前数据目录的内容\
                             整体搬过去 —— 这一步不可撤销。",
                            change.target.display()
                        )),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            Button::new("ai-data-directory-overwrite")
                                .label("覆盖并重启")
                                .primary()
                                .debug_selector(|| "ai-data-directory-overwrite".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let Some(change) = this.confirm_overwrite.clone() else {
                                        return;
                                    };
                                    this.commit_data_directory_change(change, true, cx);
                                })),
                        )
                        .child(
                            Button::new("ai-data-directory-overwrite-cancel")
                                .label("取消")
                                .outline()
                                .debug_selector(|| "ai-data-directory-overwrite-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_overwrite = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl SettingsWindowView for SystemSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::System
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().system
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
        let settings = match self.entered_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.state.report(format!("系统配置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            std::collections::BTreeMap::new(),
            ApiKeyUpdate::Keep,
            None,
            window,
            cx,
        );
    }

    fn apply_saved_preferences(&mut self, cx: &mut App) {
        // 提交成功后立刻把这三项推给已经打开的窗口；写入本身才是唯一事实来源。
        apply_pdf_compact_reading(self.pdf_compact_reading, cx);
        apply_translation_display_mode(cx);
        set_appearance(self.appearance, cx);
    }
}

impl Render for SystemSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        settings_frame(self, body, None, cx)
    }
}

/// 存储位置的只读路径行：标签在上、路径在下，长路径换行显示。
fn storage_path_row(label: &'static str, path: &Path, theme: Palette) -> gpui::AnyElement {
    div()
        .v_flex()
        .gap_0p5()
        .child(div().text_xs().text_color(rgb(theme.muted)).child(label))
        .child(
            div()
                .text_sm()
                .line_height(gpui::relative(1.4))
                .text_color(rgb(theme.ink))
                .child(path.display().to_string()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    fn open_window(
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        Entity<SystemSettingsWindow>,
        &mut VisualTestContext,
    ) {
        open_settings_view::<SystemSettingsWindow>(cx, SystemSettingsWindow::new)
    }

    #[gpui::test]
    fn preferences_are_drafts_until_saved(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        visual.simulate_resize(size(px(1080.), px(1600.)));
        redraw(visual);
        let compact = visual
            .debug_bounds("ai-pdf-compact-reading")
            .expect("compact reading checkbox must be rendered");
        visual.simulate_click(compact.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, _| {
            assert!(view.pdf_compact_reading);
            assert!(view.entered_settings().unwrap().pdf_compact_reading);
            assert!(
                !view
                    .services
                    .provider_settings()
                    .unwrap()
                    .pdf_compact_reading,
                "changing the checkbox must not save the reading preference",
            );
        });

        let proxy = visual
            .debug_bounds("ai-use-system-proxy")
            .expect("system proxy checkbox must be rendered");
        settings.read_with(visual, |view, _| {
            assert!(
                view.use_proxy,
                "the system proxy stays on until the user opts out",
            );
        });
        visual.simulate_click(proxy.center(), Modifiers::none());
        redraw(visual);
        settings.read_with(visual, |view, _| {
            assert!(!view.use_proxy);
            assert!(!view.entered_settings().unwrap().use_proxy);
            assert!(
                view.services.provider_settings().unwrap().use_proxy,
                "changing the checkbox must not save the network preference",
            );
        });
        assert!(
            visual
                .debug_bounds("ai-background-job-scheduling")
                .is_none(),
            "调度卡片属于后台任务窗口，不应出现在系统配置窗口里"
        );
    }

    #[gpui::test]
    fn language_dropdown_updates_the_translation_draft(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        settings.read_with(visual, |view, _| {
            assert_eq!(
                view.default_language.as_deref(),
                Some("zh-Hans"),
                "系统配置窗口从简体中文开始",
            );
        });
        let button = visual
            .debug_bounds("ai-default-language")
            .expect("default-language dropdown must be rendered");
        assert!(button.size.width > px(0.) && button.size.height > px(0.));

        visual.update(|_, cx| {
            settings.update(cx, |view, cx| {
                view.set_default_language(Some("ja".to_string()), cx);
            });
        });
        redraw(visual);
        settings.read_with(visual, |view, _| {
            assert_eq!(view.default_language.as_deref(), Some("ja"));
            assert_eq!(
                view.entered_settings().unwrap().default_language.as_deref(),
                Some("ja")
            );
            assert_eq!(
                view.services
                    .provider_settings()
                    .unwrap()
                    .default_language
                    .as_deref(),
                Some("zh-Hans"),
                "changing the language must not save the draft",
            );
        });
    }

    #[gpui::test]
    fn storage_paths_are_shown_without_touching_other_settings(cx: &mut TestAppContext) {
        let (_directory, settings, visual) = open_window(cx);
        let change = visual
            .debug_bounds("ai-data-directory-change")
            .expect("data-directory button must be rendered");
        assert!(change.size.width > px(0.));
        settings.read_with(visual, |view, _| {
            let entered = view.entered_settings().unwrap();
            assert_eq!(
                entered.chat_model,
                view.services.provider_settings().unwrap().chat_model
            );
            assert!(view.state.notice().is_none());
        });
    }
}
