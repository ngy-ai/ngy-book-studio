//! The system settings panel.

use super::*;

/// 朗读语速的预设档位：分段按钮比滑杆更好点，也不会产生越界的取值。
/// 中间那档（1.0x）是默认值，`selected_index` 的兜底下标因此是 1。
const SPEECH_RATE_PRESETS: [f32; 5] = [0.75, 1.0, 1.25, 1.5, 2.0];

impl SystemSettingsWindow {
    #[inline(never)]
    pub(crate) fn render_panel(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        let busy = self.state.busy();
        let compact_view = cx.entity();
        let language_view = cx.entity();
        let display_mode_view = cx.entity();
        let use_proxy_view = cx.entity();
        let speech_view = cx.entity();
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

        // 朗读：音色下拉 + 语速档位 + 试听。音色列表来自系统（窗口创建时读一次），
        // 没有可选语音时控件禁用，说明写在卡片注释里。
        let selected_voice = self.speech_voice_id.clone();
        let selected_voice_list = self.speech_voices.clone();
        let voice_label = self
            .speech_voices
            .iter()
            .find(|voice| voice.id == selected_voice)
            .map(|voice| format!("{}（{}）", voice.label, voice.language))
            .unwrap_or_else(|| "系统默认语音".to_string());
        let voice_button = Button::new("ai-speech-voice")
            .debug_selector(|| "ai-speech-voice".into())
            .outline()
            .label(voice_label)
            .disabled(busy || self.speech_voices.is_empty())
            .dropdown_menu({
                let view = speech_view.clone();
                move |menu, _, _| {
                    let mut menu = menu.item(
                        PopupMenuItem::new("系统默认语音")
                            .checked(selected_voice.is_empty())
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_speech_voice(String::new(), cx);
                                    });
                                }
                            }),
                    );
                    for voice in &selected_voice_list {
                        menu = menu.item(
                            PopupMenuItem::new(format!("{}（{}）", voice.label, voice.language))
                                .checked(voice.id == selected_voice)
                                .on_click({
                                    let view = view.clone();
                                    let id = voice.id.clone();
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.set_speech_voice(id.clone(), cx);
                                        });
                                    }
                                }),
                        );
                    }
                    menu
                }
            });
        let speech_rate_switch = TabBar::new("ai-speech-rate")
            .segmented()
            .selected_index(
                SPEECH_RATE_PRESETS
                    .iter()
                    .position(|rate| (rate - self.speech_rate).abs() < 0.01)
                    .unwrap_or(1),
            )
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                if let Some(rate) = SPEECH_RATE_PRESETS.get(*index) {
                    this.set_speech_rate(*rate, cx);
                }
            }))
            .children(
                SPEECH_RATE_PRESETS
                    .iter()
                    .map(|rate| Tab::new().label(format!("{rate}x"))),
            );
        let preview_view = cx.entity();
        let speech_preview = Button::new("ai-speech-preview")
            .debug_selector(|| "ai-speech-preview".into())
            .outline()
            .label("试听")
            .disabled(busy || self.speech_voices.is_empty())
            .on_click(move |_, _, cx| {
                preview_view.update(cx, |this, cx| this.preview_speech(cx));
            });

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
                    card_title(theme, "朗读"),
                    div()
                        .h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap_3()
                        .child(voice_button)
                        .child(speech_rate_switch)
                        .child(speech_preview)
                        .into_any_element(),
                    card_note(
                        theme,
                        "阅读窗口的朗读使用 Windows 系统语音：离线、无需下载模型，点下去即可出声。\
                         音色与语速在这里设置，对所有图书生效。试听会占用音频输出：正在进行的朗读\
                         会随之停下；系统没有可用语音时，朗读控件与阅读窗口的朗读按钮都会提示不可用。",
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
}
