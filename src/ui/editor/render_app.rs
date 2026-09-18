//! The rendered editor window.

use super::*;

impl Render for EditorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.closing && !self.removal_scheduled {
            self.removal_scheduled = true;
            remove_window_after_current_frame(window, cx, self.closing_webview.take());
        }
        let view = cx.entity().clone();
        let write_busy = self.active_write.is_some() || self.export_dialog_open;
        let media_modal_open = self.media_modal.is_some();
        let (status_icon, status_text, status_color) =
            if self.closing || self.webview_build_gate.close_requested {
                (
                    IconName::BookOpen,
                    "正在安全关闭编辑器…",
                    palette(cx).accent,
                )
            } else if let Some(operation) = self.active_write.as_ref() {
                match operation.intent {
                    EditorWriteIntent::Save => {
                        (IconName::BookOpen, "正在后台保存…", palette(cx).accent)
                    }
                    EditorWriteIntent::Export => (
                        IconName::BookOpen,
                        "正在后台保存并导出…",
                        palette(cx).accent,
                    ),
                    EditorWriteIntent::Close => (
                        IconName::BookOpen,
                        "正在后台保存并关闭…",
                        palette(cx).accent,
                    ),
                }
            } else if self.export_dialog_open {
                (
                    IconName::ExternalLink,
                    "正在选择导出位置…",
                    palette(cx).accent,
                )
            } else if self.cover_loading {
                (IconName::BookOpen, "正在读取封面图片…", palette(cx).accent)
            } else if self.media_loading {
                (IconName::BookOpen, "正在读取媒体文件…", palette(cx).accent)
            } else if media_modal_open {
                (IconName::Settings2, "正在编辑媒体信息…", palette(cx).accent)
            } else if let Some(pending) = self.pending_snapshot.as_ref() {
                match pending.action {
                    PendingEditorAction::Save => {
                        (IconName::BookOpen, "正在同步并保存…", palette(cx).accent)
                    }
                    PendingEditorAction::Export => {
                        (IconName::BookOpen, "正在同步并导出…", palette(cx).accent)
                    }
                    PendingEditorAction::Close => {
                        (IconName::BookOpen, "正在保存并关闭…", palette(cx).accent)
                    }
                    _ => (IconName::BookOpen, "正在同步富文本…", palette(cx).accent),
                }
            } else if self.pending_ready_action.is_some() {
                (
                    IconName::BookOpen,
                    "等待富文本视图就绪…",
                    palette(cx).accent,
                )
            } else if self.webview_build_gate.building {
                (
                    IconName::BookOpen,
                    "正在初始化预览组件…",
                    palette(cx).accent,
                )
            } else if self.tab.uses_webview() && self.editor_webview.is_none() {
                (
                    IconName::TriangleAlert,
                    "预览组件不可用",
                    palette(cx).danger,
                )
            } else if self.tab == EditorTab::RichText
                && !self.active_web_page.as_ref().is_some_and(|page| page.ready)
            {
                (
                    IconName::BookOpen,
                    "富文本视图正在加载…",
                    palette(cx).accent,
                )
            } else {
                (IconName::CircleCheck, "编辑器已就绪", 0x376441)
            };
        let chapter_title = self.chapter_title_input.read(cx).value().trim().to_string();
        let editor_summary = if self.chapters.get(self.selected).is_some() {
            format!(
                "第 {} / {} 章 · {}",
                self.selected + 1,
                self.chapters.len(),
                if chapter_title.is_empty() {
                    "未命名章节"
                } else {
                    chapter_title.as_str()
                }
            )
        } else {
            "暂无章节".to_string()
        };
        let mode = match self.tab {
            EditorTab::Source => "HTML 源码编辑",
            EditorTab::Preview => "只读预览",
            EditorTab::RichText => "富文本 · 所见即所得",
        };
        let editor_context = if self.search_query.is_empty() {
            format!("{mode} · 手动保存")
        } else {
            format!(
                "{mode} · 检索 {} 个结果 · 手动保存",
                self.search_results.len()
            )
        };
        let status_bar = render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            editor_summary,
            editor_context,
            palette(cx),
        );

        let toc_rows = self
            .canonical_document
            .as_ref()
            .map(|document| editor_toc_rows(&document.toc))
            .unwrap_or_default();
        let selected_toc_id = self.current_toc_id();
        let chapter_list = if self.search_query.is_empty() {
            let listed_units = toc_rows
                .iter()
                .map(|row| row.unit_id.as_str())
                .collect::<HashSet<_>>();
            let mut items = toc_rows
                .iter()
                .map(|row| {
                    let selected = selected_toc_id.as_deref() == Some(row.id.as_str());
                    let id = row.id.clone();
                    let list_view = view.clone();
                    Button::new(SharedString::from(format!("editor-toc-{}", row.id)))
                        .debug_selector(|| format!("editor-toc-{}", row.id))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(38.))
                        .flex_none()
                        .justify_start()
                        .pl(px(18. + row.depth.min(4) as f32 * 14.))
                        .pr_3()
                        .py_2()
                        .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                        .disabled(write_busy)
                        .on_click(move |_, window, cx| {
                            list_view.update(cx, |this, cx| this.select_toc(&id, window, cx));
                        })
                        .child(
                            div()
                                .w_full()
                                .min_w(px(0.))
                                .text_left()
                                .text_sm()
                                .line_clamp(2)
                                .text_color(if selected {
                                    rgb(palette(cx).accent)
                                } else {
                                    rgb(palette(cx).ink)
                                })
                                .child(row.label.clone()),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let unlisted = self
                .unit_states
                .iter()
                .enumerate()
                .filter(|(_, unit)| !listed_units.contains(unit.id.as_str()))
                .collect::<Vec<_>>();
            if !toc_rows.is_empty() && !unlisted.is_empty() {
                items.push(
                    div()
                        .px_3()
                        .py_2()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child("未列入目录的章节")
                        .into_any_element(),
                );
            }
            for (index, _) in unlisted {
                let Some(chapter) = self.chapters.get(index) else {
                    continue;
                };
                let selected = index == self.selected;
                let list_view = view.clone();
                items.push(
                    Button::new(("editor-chapter", index))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .min_h(px(38.))
                        .flex_none()
                        .justify_start()
                        .px_3()
                        .py_2()
                        .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                        .disabled(write_busy)
                        .on_click(move |_, window, cx| {
                            list_view.update(cx, |this, cx| this.select_chapter(index, window, cx));
                        })
                        .child(
                            div()
                                .w_full()
                                .text_left()
                                .text_sm()
                                .line_clamp(2)
                                .child(chapter.title.clone()),
                        )
                        .into_any_element(),
                );
            }
            items
        } else {
            if self.search_results.is_empty() {
                vec![
                    div()
                        .p_4()
                        .text_center()
                        .text_sm()
                        .text_color(rgb(palette(cx).muted))
                        .child("当前草稿没有匹配章节")
                        .into_any_element(),
                ]
            } else {
                self.search_results
                    .iter()
                    .enumerate()
                    .map(|(index, hit)| {
                        let selected = hit.chapter_index == self.selected;
                        let chapter_index = hit.chapter_index;
                        let result_view = view.clone();
                        Button::new(("editor-search-result", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(px(66.))
                            .justify_start()
                            .px_2()
                            .py_2()
                            .when(selected, |this| this.bg(rgb(palette(cx).accent_soft)))
                            .disabled(write_busy)
                            .on_click(move |_, window, cx| {
                                result_view.update(cx, |this, cx| {
                                    this.select_chapter(chapter_index, window, cx)
                                });
                            })
                            .child(
                                div()
                                    .v_flex()
                                    .w_full()
                                    .min_w(px(0.))
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_left()
                                            .text_sm()
                                            .font_medium()
                                            .text_color(if selected {
                                                rgb(palette(cx).accent)
                                            } else {
                                                rgb(palette(cx).ink)
                                            })
                                            .child(hit.chapter_title.clone()),
                                    )
                                    .child(
                                        div()
                                            .line_clamp(2)
                                            .text_left()
                                            .text_xs()
                                            .text_color(rgb(palette(cx).muted))
                                            .child(hit.snippet.clone()),
                                    ),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }
        };

        let add_view = view.clone();
        let remove_view = view.clone();
        let move_up_view = view.clone();
        let move_down_view = view.clone();
        let indent_view = view.clone();
        let outdent_view = view.clone();
        let export_view = view.clone();
        let save_view = view.clone();
        let change_cover_view = view.clone();
        let clear_cover_view = view.clone();
        let structure_disabled = self.canonical_document.is_none()
            || self.media_loading
            || media_modal_open
            || write_busy
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing;
        let cover_thumbnail = if let Some(image) = self.cover_preview.as_ref() {
            img(image.clone())
                .size_full()
                .rounded(px(7.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .rounded(px(7.))
                .bg(rgb(palette(cx).sidebar))
                .text_color(rgb(palette(cx).accent))
                .child(Icon::new(IconName::BookOpen).small())
                .child(
                    div().text_xs().font_medium().child(
                        self.canonical_document
                            .as_ref()
                            .map(editor_book_format_label)
                            .unwrap_or("图书"),
                    ),
                )
                .into_any_element()
        };
        let cover_status = if self.cover_loading {
            "正在读取…"
        } else if self.cover_dirty && self.cover.is_none() {
            "封面已清除 · 待保存"
        } else if self.cover_dirty {
            "新封面 · 待保存"
        } else if self.cover.is_some() {
            "当前封面"
        } else {
            "暂无封面"
        };
        let change_cover_disabled = self.cover_loading
            || self.canonical_document.is_none()
            || self.media_loading
            || media_modal_open
            || write_busy
            || self.webview_build_gate.building
            || self.pending_snapshot.is_some()
            || self.pending_ready_action.is_some()
            || self.closing;
        let clear_cover_enabled =
            editor_clear_cover_enabled(self.cover.is_some(), change_cover_disabled);
        let cover_panel = render_editor_section(|| {
            div()
                .h_flex()
                .min_h(px(104.))
                .flex_none()
                .items_center()
                .gap_3()
                .px_3()
                .py_3()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .child(
                    div()
                        .w(px(56.))
                        .h(px(76.))
                        .flex_none()
                        .overflow_hidden()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(white())
                        .child(cover_thumbnail),
                )
                .child(
                    div()
                        .v_flex()
                        .min_w(px(0.))
                        .flex_1()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(if self.cover_dirty {
                                    rgb(palette(cx).accent)
                                } else {
                                    rgb(palette(cx).muted)
                                })
                                .child(cover_status),
                        )
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .child(
                                    Button::new("editor-change-cover")
                                        .small()
                                        .outline()
                                        .label(if self.cover_loading {
                                            "读取中…"
                                        } else if self.cover.is_some() {
                                            "更换封面"
                                        } else {
                                            "选择封面"
                                        })
                                        .disabled(change_cover_disabled)
                                        .on_click(move |_, window, cx| {
                                            change_cover_view.update(cx, |this, cx| {
                                                this.change_cover(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-clear-cover")
                                        .small()
                                        .outline()
                                        .label("清除封面")
                                        .disabled(!clear_cover_enabled)
                                        .on_click(move |_, _, cx| {
                                            clear_cover_view
                                                .update(cx, |this, cx| this.clear_cover(cx));
                                        }),
                                ),
                        ),
                )
                .into_any_element()
        });

        let toolbar = render_editor_section(|| {
            div()
                .v_flex()
                .bg(rgb(palette(cx).surface))
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .when_some(self.notice.as_ref(), |toolbar, notice| {
                    let (background, foreground, icon) = if notice.error {
                        (
                            rgb(palette(cx).danger_soft),
                            rgb(palette(cx).danger),
                            IconName::TriangleAlert,
                        )
                    } else {
                        (
                            rgb(palette(cx).success_soft),
                            rgb(palette(cx).success),
                            IconName::CircleCheck,
                        )
                    };
                    toolbar.child(
                        div()
                            .h_flex()
                            .min_h(px(36.))
                            .px_5()
                            .gap_2()
                            .border_t_1()
                            .border_color(rgb(palette(cx).border))
                            .bg(background)
                            .text_xs()
                            .text_color(foreground)
                            .child(Icon::new(icon).small())
                            .child(notice.text.clone()),
                    )
                })
                .child(
                    div()
                        .h_flex()
                        .h(px(64.))
                        .px_5()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(72.))
                                        .text_sm()
                                        .text_color(rgb(palette(cx).muted))
                                        .child("书名"),
                                )
                                .child(
                                    div()
                                        .w(px(280.))
                                        .child(Input::new(&self.title_input).disabled(write_busy)),
                                )
                                .child(
                                    div()
                                        .w(px(56.))
                                        .text_sm()
                                        .text_color(rgb(palette(cx).muted))
                                        .child("作者"),
                                )
                                .child(
                                    div()
                                        .w(px(200.))
                                        .child(Input::new(&self.author_input).disabled(write_busy)),
                                ),
                        )
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .child(
                                    Button::new("editor-export")
                                        .icon(IconName::ExternalLink)
                                        .label("导出 EPUB")
                                        .outline()
                                        .disabled(write_busy || media_modal_open)
                                        .on_click(move |_, window, cx| {
                                            export_view.update(cx, |this, cx| {
                                                this.export_book(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-save")
                                        .label(if write_busy { "处理中…" } else { "保存" })
                                        .primary()
                                        .disabled(write_busy || media_modal_open)
                                        .on_click(move |_, window, cx| {
                                            save_view
                                                .update(cx, |this, cx| this.save_draft(window, cx));
                                        }),
                                ),
                        ),
                )
                .into_any_element()
        });

        let left_sidebar_width = self.left_sidebar_width;
        let sidebar = render_editor_section(|| {
            div()
                .v_flex()
                .w(left_sidebar_width)
                .h_full()
                .flex_none()
                .border_r_1()
                .border_color(rgb(palette(cx).border))
                .bg(rgb(palette(cx).sidebar))
                .child(
                    div()
                        .h_flex()
                        .h(px(48.))
                        .px_4()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .text_sm()
                                .font_semibold()
                                .text_color(rgb(palette(cx).ink))
                                .child(Icon::new(IconName::Menu).small())
                                .child(if self.search_query.is_empty() {
                                    format!("目录（{}）", toc_rows.len())
                                } else {
                                    format!("搜索结果（{}）", self.search_results.len())
                                }),
                        )
                        .child(
                            div()
                                .h_flex()
                                .gap_1()
                                .child(
                                    Button::new("editor-move-chapter-up")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowUp)
                                        .tooltip("上移阅读顺序")
                                        .disabled(structure_disabled || self.selected == 0)
                                        .on_click(move |_, window, cx| {
                                            move_up_view.update(cx, |this, cx| {
                                                this.move_chapter(-1, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-move-chapter-down")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowDown)
                                        .tooltip("下移阅读顺序")
                                        .disabled(
                                            structure_disabled
                                                || self.selected + 1 >= self.chapters.len(),
                                        )
                                        .on_click(move |_, window, cx| {
                                            move_down_view.update(cx, |this, cx| {
                                                this.move_chapter(1, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-outdent-toc")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowLeft)
                                        .tooltip("提升目录层级")
                                        .disabled(structure_disabled)
                                        .on_click(move |_, window, cx| {
                                            outdent_view.update(cx, |this, cx| {
                                                this.outdent_toc(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-indent-toc")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::ArrowRight)
                                        .tooltip("缩进为上一项的子目录")
                                        .disabled(structure_disabled || selected_toc_id.is_none())
                                        .on_click(move |_, window, cx| {
                                            indent_view
                                                .update(cx, |this, cx| this.indent_toc(window, cx));
                                        }),
                                )
                                .child(
                                    Button::new("editor-add-chapter")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Plus)
                                        .tooltip("添加章节")
                                        .disabled(structure_disabled)
                                        .on_click(move |_, window, cx| {
                                            add_view.update(cx, |this, cx| {
                                                this.add_chapter(window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("editor-remove-chapter")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Delete)
                                        .tooltip("删除本章")
                                        .disabled(structure_disabled || self.chapters.len() <= 1)
                                        .on_click(move |_, window, cx| {
                                            remove_view.update(cx, |this, cx| {
                                                this.remove_chapter(window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(palette(cx).border))
                        .child(
                            Input::new(&self.search_input)
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true)
                                .disabled(write_busy),
                        ),
                )
                .child(cover_panel)
                .child(
                    div()
                        .id("editor-chapter-scroll")
                        .flex_1()
                        .min_h(px(0.))
                        .p_2()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .v_flex()
                                .h_auto()
                                .flex_none()
                                .gap_0p5()
                                .children(chapter_list),
                        ),
                )
                .into_any_element()
        });

        let current_unit_kind = self
            .unit_states
            .get(self.selected)
            .map(|unit| unit.kind)
            .unwrap_or(ContentUnitKind::Chapter);
        let kind_view = view.clone();
        let chapter_title_bar = render_editor_section(|| {
            div()
                .h_flex()
                .h(px(50.))
                .px_4()
                .items_center()
                .gap_3()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                .child(
                    div().text_xs().text_color(rgb(palette(cx).muted)).child(
                        if self
                            .chapters
                            .get(self.selected)
                            .is_some_and(|c| c.spine_index.is_none())
                        {
                            "新章节".to_string()
                        } else {
                            "已有章节".to_string()
                        },
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(Input::new(&self.chapter_title_input).disabled(write_busy)),
                )
                .child(
                    Button::new("editor-unit-kind")
                        .small()
                        .outline()
                        .label(unit_kind_label(current_unit_kind))
                        .tooltip("切换章节、页面、幻灯片或工作表类型")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            kind_view.update(cx, |this, cx| this.cycle_unit_kind(window, cx));
                        }),
                )
                .into_any_element()
        });

        let current_media = self
            .unit_states
            .get(self.selected)
            .and_then(|state| {
                self.canonical_document.as_ref().and_then(|document| {
                    document
                        .units
                        .iter()
                        .find(|unit| unit.id == state.id)
                        .map(|unit| media_blocks(&unit.document.blocks))
                })
            })
            .unwrap_or_default();
        let image_view = view.clone();
        let audio_view = view.clone();
        let video_view = view.clone();
        let media_controls = current_media
            .into_iter()
            .enumerate()
            .flat_map(|(index, media)| {
                let edit_view = view.clone();
                let replace_view = view.clone();
                let delete_view = view.clone();
                let edit_target = media.clone();
                let replace_target = media.clone();
                let delete_target = media.clone();
                let kind = match media.kind {
                    MediaKind::Image => "图片",
                    MediaKind::Audio => "音频",
                    MediaKind::Video => "视频",
                };
                [
                    Button::new(("editor-edit-media-metadata", index))
                        .xsmall()
                        .outline()
                        .icon(IconName::Settings2)
                        .label(format!("{kind} · {}", media.label))
                        .tooltip("编辑此媒体的标题与说明")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            edit_view.update(cx, |this, cx| {
                                this.edit_media_metadata(edit_target.clone(), window, cx)
                            });
                        })
                        .into_any_element(),
                    Button::new(("editor-replace-media", index))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Replace)
                        .tooltip("替换此媒体文件并确认信息")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            replace_view.update(cx, |this, cx| {
                                this.replace_media(replace_target.clone(), window, cx)
                            });
                        })
                        .into_any_element(),
                    Button::new(("editor-delete-media", index))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Delete)
                        .tooltip("删除此媒体块")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.request_delete_media(delete_target.clone(), window, cx)
                            });
                        })
                        .into_any_element(),
                ]
            })
            .collect::<Vec<_>>();
        let media_toolbar = render_editor_section(|| {
            div()
                .id("editor-media-toolbar")
                .h_flex()
                .min_h(px(42.))
                .flex_none()
                .gap_1()
                .px_4()
                .py_1()
                .border_b_1()
                .border_color(rgb(palette(cx).border))
                // gpui-component 0.5.1's `overflow_x_scrollbar` moves this
                // element's styles onto a `size_full` wrapper and clears the
                // inner flex-row style. That makes this fixed-height toolbar
                // consume the whole editor and stacks its controls vertically.
                .overflow_x_scroll()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette(cx).muted))
                        .child("媒体"),
                )
                .child(
                    Button::new("editor-insert-image")
                        .xsmall()
                        .outline()
                        .label("+ 图片")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            image_view.update(cx, |this, cx| {
                                this.insert_media(MediaKind::Image, window, cx)
                            });
                        }),
                )
                .child(
                    Button::new("editor-insert-audio")
                        .xsmall()
                        .outline()
                        .label("+ 音频")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            audio_view.update(cx, |this, cx| {
                                this.insert_media(MediaKind::Audio, window, cx)
                            });
                        }),
                )
                .child(
                    Button::new("editor-insert-video")
                        .xsmall()
                        .outline()
                        .label("+ 视频")
                        .disabled(structure_disabled)
                        .on_click(move |_, window, cx| {
                            video_view.update(cx, |this, cx| {
                                this.insert_media(MediaKind::Video, window, cx)
                            });
                        }),
                )
                .children(media_controls)
                .into_any_element()
        });

        // Three editing views: raw source, read-only preview, and WYSIWYG.
        let source_view = view.clone();
        let preview_view = view.clone();
        let richtext_view = view.clone();
        let tab_bar = render_editor_section(|| {
            TabBar::new("editor-tabs")
                .underline()
                .selected_index(self.tab as usize)
                .on_click(move |index, window, cx| {
                    let tab = match index {
                        0 => EditorTab::Source,
                        1 => EditorTab::Preview,
                        _ => EditorTab::RichText,
                    };
                    let view = match tab {
                        EditorTab::Source => source_view.clone(),
                        EditorTab::Preview => preview_view.clone(),
                        EditorTab::RichText => richtext_view.clone(),
                    };
                    view.update(cx, |this, cx| this.switch_tab(tab, window, cx));
                })
                .child(Tab::new().label("HTML 源码"))
                .child(Tab::new().label("预览"))
                .child(Tab::new().label("富文本"))
                .into_any_element()
        });

        let editor_body = render_editor_section(|| match self.tab {
            EditorTab::Source => div()
                .flex_1()
                .min_h(px(0.))
                .bg(rgb(palette(cx).paper))
                .p_2()
                .child(Input::new(&self.body_input).h_full().disabled(write_busy))
                .into_any_element(),
            EditorTab::Preview | EditorTab::RichText => match &self.editor_webview {
                Some(webview) => div()
                    .flex_1()
                    .min_h(px(0.))
                    .bg(rgb(palette(cx).paper))
                    .p_2()
                    .child(
                        div()
                            .size_full()
                            .overflow_hidden()
                            .rounded(px(10.))
                            .border_1()
                            .border_color(rgb(palette(cx).border))
                            // 容器底色是 WebView 建好前的第一帧，必须已经是当前皮肤
                            // 的颜色：写死白色会在深色皮肤里闪一帧白底。
                            .bg(rgb(palette(cx).paper))
                            .child(webview.clone()),
                    )
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .min_h(px(0.))
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(rgb(palette(cx).muted))
                    .child(Icon::new(IconName::Settings2).large())
                    .child(div().text_sm().child("正在加载预览视图…"))
                    .into_any_element(),
            },
        });

        let content = render_editor_section(|| {
            div()
                .v_flex()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .bg(rgb(palette(cx).paper))
                .child(chapter_title_bar)
                .child(media_toolbar)
                .child(tab_bar)
                .child(editor_body)
                .into_any_element()
        });
        let media_modal = self.render_media_metadata_modal(cx);
        let left_resize_handle = render_editor_resize_handle(
            "editor-left-resize",
            EditorResizablePane::Left,
            view.clone(),
            palette(cx),
        );
        let right_resize_handle = (!self.ai_sidebar_collapsed).then(|| {
            render_editor_resize_handle(
                "editor-right-resize",
                EditorResizablePane::Right,
                view.clone(),
                palette(cx),
            )
        });

        render_editor_section(|| {
            div()
                .relative()
                .v_flex()
                .size_full()
                .bg(rgb(palette(cx).paper))
                .child(toolbar)
                .child(
                    div()
                        .h_flex()
                        .items_start()
                        .flex_1()
                        .min_h(px(0.))
                        .child(sidebar)
                        .child(left_resize_handle)
                        .child(content)
                        .when_some(right_resize_handle, |this, handle| this.child(handle))
                        .child(self.ai_sidebar.clone()),
                )
                .child(status_bar)
                .when_some(media_modal, |this, modal| this.child(modal))
                .into_any_element()
        })
    }
}
